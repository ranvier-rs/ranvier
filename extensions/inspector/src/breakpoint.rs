use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A conditional breakpoint that pauses execution at a specific node
/// when an optional condition is met against the payload.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ConditionalBreakpoint {
    pub id: String,
    pub node_id: String,
    /// Optional condition in `field op value` format, e.g. `status == "error"`, `amount > 500`.
    /// When `None`, the breakpoint fires unconditionally (like a normal breakpoint).
    pub condition: Option<String>,
    pub enabled: bool,
}

/// Evaluate a simple condition expression against a JSON payload.
///
/// Supported syntax: `field op value`
/// - `field`: a dot-separated JSON path (e.g. `status`, `data.amount`)
/// - `op`: one of `==`, `!=`, `>`, `<`, `>=`, `<=`
/// - `value`: a number, `"string"` (with quotes), or bare identifier (`true`, `false`, `null`)
///
/// Returns `true` if the condition matches, `false` if it doesn't match
/// or the expression cannot be parsed.
pub fn evaluate_condition(condition: &str, payload: &serde_json::Value) -> bool {
    let Some((field, op, expected)) = parse_condition(condition) else {
        return false;
    };

    let actual = resolve_path(payload, &field);

    match op {
        CompareOp::Eq => values_equal(&actual, &expected),
        CompareOp::Ne => !values_equal(&actual, &expected),
        CompareOp::Gt => compare_numeric(&actual, &expected).is_some_and(|o| o.is_gt()),
        CompareOp::Lt => compare_numeric(&actual, &expected).is_some_and(|o| o.is_lt()),
        CompareOp::Ge => compare_numeric(&actual, &expected).is_some_and(|o| o.is_ge()),
        CompareOp::Le => compare_numeric(&actual, &expected).is_some_and(|o| o.is_le()),
    }
}

#[derive(Debug, Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

fn parse_condition(expr: &str) -> Option<(String, CompareOp, ConditionValue)> {
    // Try multi-char operators first
    for (token, op) in [
        ("==", CompareOp::Eq),
        ("!=", CompareOp::Ne),
        (">=", CompareOp::Ge),
        ("<=", CompareOp::Le),
        (">", CompareOp::Gt),
        ("<", CompareOp::Lt),
    ] {
        if let Some(idx) = expr.find(token) {
            let field = expr[..idx].trim().to_string();
            let value_str = expr[idx + token.len()..].trim();
            if field.is_empty() || value_str.is_empty() {
                continue;
            }
            let value = parse_value(value_str);
            return Some((field, op, value));
        }
    }
    None
}

#[derive(Debug, Clone)]
enum ConditionValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

fn parse_value(s: &str) -> ConditionValue {
    // Quoted string
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return ConditionValue::String(s[1..s.len() - 1].to_string());
    }
    // Boolean
    if s.eq_ignore_ascii_case("true") {
        return ConditionValue::Bool(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return ConditionValue::Bool(false);
    }
    // Null
    if s.eq_ignore_ascii_case("null") {
        return ConditionValue::Null;
    }
    // Number
    if let Ok(n) = s.parse::<f64>() {
        return ConditionValue::Number(n);
    }
    // Bare string fallback
    ConditionValue::String(s.to_string())
}

fn resolve_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                current = map.get(segment)?;
            }
            serde_json::Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn values_equal(actual: &Option<&serde_json::Value>, expected: &ConditionValue) -> bool {
    let Some(actual) = actual else {
        return matches!(expected, ConditionValue::Null);
    };
    match expected {
        ConditionValue::String(s) => actual.as_str() == Some(s.as_str()),
        ConditionValue::Number(n) => actual.as_f64() == Some(*n),
        ConditionValue::Bool(b) => actual.as_bool() == Some(*b),
        ConditionValue::Null => actual.is_null(),
    }
}

fn compare_numeric(
    actual: &Option<&serde_json::Value>,
    expected: &ConditionValue,
) -> Option<std::cmp::Ordering> {
    let actual_num = (*actual)?.as_f64()?;
    let expected_num = match expected {
        ConditionValue::Number(n) => *n,
        _ => return None,
    };
    actual_num.partial_cmp(&expected_num)
}

/// Global registry for conditional breakpoints managed by the Inspector.
struct BreakpointStore {
    breakpoints: HashMap<String, ConditionalBreakpoint>,
    next_id: u64,
}

impl BreakpointStore {
    fn new() -> Self {
        Self {
            breakpoints: HashMap::new(),
            next_id: 1,
        }
    }

    fn add(&mut self, node_id: String, condition: Option<String>) -> ConditionalBreakpoint {
        let id = format!("bp-{}", self.next_id);
        self.next_id += 1;
        let bp = ConditionalBreakpoint {
            id: id.clone(),
            node_id,
            condition,
            enabled: true,
        };
        self.breakpoints.insert(id, bp.clone());
        bp
    }

    fn remove(&mut self, id: &str) -> bool {
        self.breakpoints.remove(id).is_some()
    }

    fn update(&mut self, id: &str, enabled: Option<bool>, condition: Option<Option<String>>) -> Option<ConditionalBreakpoint> {
        let bp = self.breakpoints.get_mut(id)?;
        if let Some(e) = enabled {
            bp.enabled = e;
        }
        if let Some(c) = condition {
            bp.condition = c;
        }
        Some(bp.clone())
    }

    fn list(&self) -> Vec<ConditionalBreakpoint> {
        let mut bps: Vec<_> = self.breakpoints.values().cloned().collect();
        bps.sort_by(|a, b| a.id.cmp(&b.id));
        bps
    }

    /// Check if any enabled breakpoint fires for the given node and payload.
    fn should_pause(&self, node_id: &str, payload: Option<&serde_json::Value>) -> bool {
        self.breakpoints.values().any(|bp| {
            if !bp.enabled || bp.node_id != node_id {
                return false;
            }
            match (&bp.condition, payload) {
                (None, _) => true, // unconditional
                (Some(cond), Some(p)) => evaluate_condition(cond, p),
                (Some(_), None) => false, // condition requires payload
            }
        })
    }
}

static BREAKPOINT_STORE: OnceLock<Arc<Mutex<BreakpointStore>>> = OnceLock::new();

fn get_store() -> Arc<Mutex<BreakpointStore>> {
    BREAKPOINT_STORE
        .get_or_init(|| Arc::new(Mutex::new(BreakpointStore::new())))
        .clone()
}

/// Add a conditional breakpoint.
pub fn add_breakpoint(
    node_id: String,
    condition: Option<String>,
) -> ConditionalBreakpoint {
    get_store().lock().unwrap().add(node_id, condition)
}

/// Remove a breakpoint by ID.
pub fn remove_breakpoint(id: &str) -> bool {
    get_store().lock().unwrap().remove(id)
}

/// Update a breakpoint's enabled state and/or condition.
pub fn update_breakpoint(
    id: &str,
    enabled: Option<bool>,
    condition: Option<Option<String>>,
) -> Option<ConditionalBreakpoint> {
    get_store().lock().unwrap().update(id, enabled, condition)
}

/// List all conditional breakpoints.
pub fn list_breakpoints() -> Vec<ConditionalBreakpoint> {
    get_store().lock().unwrap().list()
}

/// Check if any conditional breakpoint should fire for the given node and optional payload.
pub fn should_pause_conditional(
    node_id: &str,
    payload: Option<&serde_json::Value>,
) -> bool {
    get_store().lock().unwrap().should_pause(node_id, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluate_eq_string() {
        let payload = json!({"status": "error", "code": 500});
        assert!(evaluate_condition("status == \"error\"", &payload));
        assert!(!evaluate_condition("status == \"ok\"", &payload));
    }

    #[test]
    fn evaluate_gt_number() {
        let payload = json!({"amount": 1500, "count": 3});
        assert!(evaluate_condition("amount > 1000", &payload));
        assert!(!evaluate_condition("amount > 2000", &payload));
        assert!(evaluate_condition("count >= 3", &payload));
        assert!(!evaluate_condition("count >= 4", &payload));
    }

    #[test]
    fn evaluate_nested_path() {
        let payload = json!({"data": {"user": {"role": "admin"}}});
        assert!(evaluate_condition("data.user.role == \"admin\"", &payload));
        assert!(!evaluate_condition("data.user.role == \"guest\"", &payload));
    }

    #[test]
    fn evaluate_ne_and_bool() {
        let payload = json!({"active": true, "name": "test"});
        assert!(evaluate_condition("active == true", &payload));
        assert!(evaluate_condition("name != \"other\"", &payload));
        assert!(!evaluate_condition("active == false", &payload));
    }

    #[test]
    fn evaluate_null_and_missing() {
        let payload = json!({"value": null});
        assert!(evaluate_condition("value == null", &payload));
        assert!(evaluate_condition("missing == null", &payload)); // missing resolves to None → Null
        assert!(!evaluate_condition("value == 0", &payload));
    }

    #[test]
    fn store_add_remove_list() {
        let mut store = BreakpointStore::new();
        let bp1 = store.add("nodeA".into(), None);
        let bp2 = store.add("nodeB".into(), Some("status == \"error\"".into()));
        assert_eq!(store.list().len(), 2);

        // should_pause without condition
        assert!(store.should_pause("nodeA", None));
        assert!(!store.should_pause("nodeC", None));

        // should_pause with condition
        let payload = json!({"status": "error"});
        assert!(store.should_pause("nodeB", Some(&payload)));
        let payload_ok = json!({"status": "ok"});
        assert!(!store.should_pause("nodeB", Some(&payload_ok)));

        // remove
        assert!(store.remove(&bp1.id));
        assert_eq!(store.list().len(), 1);
        assert!(!store.remove(&bp1.id)); // already removed

        // update
        let updated = store.update(&bp2.id, Some(false), None).unwrap();
        assert!(!updated.enabled);
        assert!(!store.should_pause("nodeB", Some(&payload)));
    }
}
