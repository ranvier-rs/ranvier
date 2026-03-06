use rand::Rng;
use serde_json::{Map, Value, json};

/// Generate an empty JSON template from a JSON Schema.
///
/// Required fields are set to type-appropriate zero values:
/// - `"string"` → `""`
/// - `"integer"` / `"number"` → `0`
/// - `"boolean"` → `false`
/// - `"array"` → `[]`
/// - `"object"` → recursive expand
/// - Optional fields → `null`
pub fn generate_template(schema: &Value) -> Value {
    generate_from_schema(schema, schema, false)
}

/// Generate a random sample JSON from a JSON Schema.
///
/// Values are generated based on type + constraints:
/// - `"string"` with `enum` → random choice
/// - `"string"` with `minLength`/`maxLength` → random length string
/// - `"integer"` / `"number"` with `minimum`/`maximum` → random in range
/// - `"boolean"` → random true/false
/// - `"array"` with `items` → 1-3 random items
/// - `"object"` → recursive expand
pub fn generate_sample(schema: &Value) -> Value {
    generate_from_schema(schema, schema, true)
}

fn generate_from_schema(schema: &Value, root: &Value, randomize: bool) -> Value {
    // Handle $ref
    if let Some(ref_path) = schema.get("$ref").and_then(Value::as_str) {
        if let Some(resolved) = resolve_ref(root, ref_path) {
            return generate_from_schema(resolved, root, randomize);
        }
        return Value::Null;
    }

    // Handle allOf
    if let Some(Value::Array(all_of)) = schema.get("allOf") {
        let mut merged = Map::new();
        for sub in all_of {
            if let Value::Object(obj) = generate_from_schema(sub, root, randomize) {
                for (k, v) in obj {
                    merged.insert(k, v);
                }
            }
        }
        return Value::Object(merged);
    }

    // Handle oneOf/anyOf — pick the first variant
    if let Some(Value::Array(variants)) = schema.get("oneOf").or(schema.get("anyOf")) {
        if let Some(first) = variants.first() {
            return generate_from_schema(first, root, randomize);
        }
    }

    // Handle enum
    if let Some(Value::Array(enum_vals)) = schema.get("enum") {
        if randomize {
            let mut rng = rand::rng();
            let idx = rng.random_range(0..enum_vals.len());
            return enum_vals[idx].clone();
        }
        return enum_vals.first().cloned().unwrap_or(Value::Null);
    }

    // Handle const
    if let Some(const_val) = schema.get("const") {
        return const_val.clone();
    }

    let type_val = schema.get("type").and_then(Value::as_str).unwrap_or("");

    match type_val {
        "object" => generate_object(schema, root, randomize),
        "array" => generate_array(schema, root, randomize),
        "string" => generate_string(schema, randomize),
        "integer" => generate_integer(schema, randomize),
        "number" => generate_number(schema, randomize),
        "boolean" => generate_boolean(randomize),
        "null" => Value::Null,
        _ => {
            // If properties exist, treat as object even without explicit "type": "object"
            if schema.get("properties").is_some() {
                return generate_object(schema, root, randomize);
            }
            Value::Null
        }
    }
}

fn generate_object(schema: &Value, root: &Value, randomize: bool) -> Value {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut obj = Map::new();

    if let Some(Value::Object(properties)) = schema.get("properties") {
        for (key, prop_schema) in properties {
            let is_required = required.contains(&key.as_str());
            let val = generate_from_schema(prop_schema, root, randomize);
            if is_required || randomize {
                obj.insert(key.clone(), val);
            } else {
                obj.insert(key.clone(), Value::Null);
            }
        }
    }

    Value::Object(obj)
}

fn generate_array(schema: &Value, root: &Value, randomize: bool) -> Value {
    if !randomize {
        return Value::Array(vec![]);
    }

    let items_schema = schema.get("items").unwrap_or(&Value::Null);
    let mut rng = rand::rng();
    let min_items = schema
        .get("minItems")
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let max_items = schema
        .get("maxItems")
        .and_then(Value::as_u64)
        .unwrap_or(3) as usize;
    let count = rng.random_range(min_items..=max_items);

    let items: Vec<Value> = (0..count)
        .map(|_| generate_from_schema(items_schema, root, true))
        .collect();
    Value::Array(items)
}

fn generate_string(schema: &Value, randomize: bool) -> Value {
    if !randomize {
        return json!("");
    }

    let mut rng = rand::rng();

    // Handle format
    if let Some(format) = schema.get("format").and_then(Value::as_str) {
        return match format {
            "email" => json!(format!("user{}@example.com", rng.random_range(1..999))),
            "uri" | "url" => json!("https://example.com"),
            "uuid" => json!(uuid::Uuid::new_v4().to_string()),
            "date" => json!("2026-01-15"),
            "date-time" => json!("2026-01-15T10:30:00Z"),
            "ipv4" => json!(format!(
                "{}.{}.{}.{}",
                rng.random_range(1..255u32),
                rng.random_range(0..255u32),
                rng.random_range(0..255u32),
                rng.random_range(1..255u32)
            )),
            _ => json!(format!("sample_{}", rng.random_range(100..999))),
        };
    }

    let min_len = schema
        .get("minLength")
        .and_then(Value::as_u64)
        .unwrap_or(3) as usize;
    let max_len = schema
        .get("maxLength")
        .and_then(Value::as_u64)
        .unwrap_or(12) as usize;
    let len = rng.random_range(min_len..=max_len);

    let s: String = (0..len).map(|_| rng.random_range(b'a'..=b'z') as char).collect();
    json!(s)
}

fn generate_integer(schema: &Value, randomize: bool) -> Value {
    if !randomize {
        return json!(0);
    }

    let mut rng = rand::rng();
    let min = schema.get("minimum").and_then(Value::as_i64).unwrap_or(0);
    let max = schema
        .get("maximum")
        .and_then(Value::as_i64)
        .unwrap_or(1000);
    json!(rng.random_range(min..=max))
}

fn generate_number(schema: &Value, randomize: bool) -> Value {
    if !randomize {
        return json!(0.0);
    }

    let mut rng = rand::rng();
    let min = schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0);
    let max = schema
        .get("maximum")
        .and_then(Value::as_f64)
        .unwrap_or(1000.0);
    let val = min + rng.random::<f64>() * (max - min);
    json!((val * 100.0).round() / 100.0) // 2 decimal places
}

fn generate_boolean(randomize: bool) -> Value {
    if !randomize {
        return json!(false);
    }
    json!(rand::rng().random::<bool>())
}

/// Resolve a `$ref` path like `#/$defs/MyType` against the root schema.
fn resolve_ref<'a>(root: &'a Value, ref_path: &str) -> Option<&'a Value> {
    let path = ref_path.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_generates_empty_structure() {
        let schema = json!({
            "type": "object",
            "required": ["name", "age"],
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" },
                "email": { "type": "string", "format": "email" },
                "active": { "type": "boolean" }
            }
        });

        let result = generate_template(&schema);
        assert_eq!(result["name"], "");
        assert_eq!(result["age"], 0);
        assert!(result.get("email").is_some());
        assert!(result.get("active").is_some());
    }

    #[test]
    fn template_generates_nested_objects() {
        let schema = json!({
            "type": "object",
            "required": ["user"],
            "properties": {
                "user": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": { "type": "integer" },
                        "name": { "type": "string" }
                    }
                }
            }
        });

        let result = generate_template(&schema);
        assert_eq!(result["user"]["id"], 0);
    }

    #[test]
    fn template_generates_empty_array() {
        let schema = json!({
            "type": "object",
            "required": ["tags"],
            "properties": {
                "tags": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }
        });

        let result = generate_template(&schema);
        assert_eq!(result["tags"], json!([]));
    }

    #[test]
    fn sample_generates_random_values() {
        let schema = json!({
            "type": "object",
            "required": ["name", "age"],
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer", "minimum": 18, "maximum": 65 }
            }
        });

        let result = generate_sample(&schema);
        assert!(result["name"].is_string());
        let age = result["age"].as_i64().unwrap();
        assert!((18..=65).contains(&age));
    }

    #[test]
    fn sample_handles_enum() {
        let schema = json!({
            "type": "object",
            "required": ["status"],
            "properties": {
                "status": { "type": "string", "enum": ["active", "inactive", "pending"] }
            }
        });

        let result = generate_sample(&schema);
        let status = result["status"].as_str().unwrap();
        assert!(["active", "inactive", "pending"].contains(&status));
    }

    #[test]
    fn sample_resolves_ref() {
        let schema = json!({
            "type": "object",
            "required": ["user"],
            "properties": {
                "user": { "$ref": "#/$defs/User" }
            },
            "$defs": {
                "User": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            }
        });

        let result = generate_sample(&schema);
        assert!(result["user"]["name"].is_string());
    }

    #[test]
    fn sample_generates_formatted_strings() {
        let schema = json!({
            "type": "object",
            "required": ["email", "id"],
            "properties": {
                "email": { "type": "string", "format": "email" },
                "id": { "type": "string", "format": "uuid" }
            }
        });

        let result = generate_sample(&schema);
        let email = result["email"].as_str().unwrap();
        assert!(email.contains('@'));
        let id = result["id"].as_str().unwrap();
        assert!(id.contains('-'));
    }

    #[test]
    fn template_handles_schema_without_type() {
        let schema = json!({
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        });

        let result = generate_template(&schema);
        assert_eq!(result["name"], "");
    }
}
