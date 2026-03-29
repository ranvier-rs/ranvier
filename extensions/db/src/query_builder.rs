//! Safe dynamic SQL query builder with parameterized bindings.

use serde::Serialize;
use std::fmt;

/// SQL parameter value for query bindings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SqlValue {
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Null,
}

impl fmt::Display for SqlValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqlValue::Int(v) => write!(f, "{v}"),
            SqlValue::Float(v) => write!(f, "{v}"),
            SqlValue::Text(v) => write!(f, "{v}"),
            SqlValue::Bool(v) => write!(f, "{v}"),
            SqlValue::Null => write!(f, "NULL"),
        }
    }
}

impl From<i64> for SqlValue {
    fn from(v: i64) -> Self {
        SqlValue::Int(v)
    }
}

impl From<i32> for SqlValue {
    fn from(v: i32) -> Self {
        SqlValue::Int(v as i64)
    }
}

impl From<f64> for SqlValue {
    fn from(v: f64) -> Self {
        SqlValue::Float(v)
    }
}

impl From<String> for SqlValue {
    fn from(v: String) -> Self {
        SqlValue::Text(v)
    }
}

impl From<&str> for SqlValue {
    fn from(v: &str) -> Self {
        SqlValue::Text(v.to_string())
    }
}

impl From<bool> for SqlValue {
    fn from(v: bool) -> Self {
        SqlValue::Bool(v)
    }
}

/// Sort direction for ORDER BY clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl fmt::Display for SortDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SortDirection::Asc => write!(f, "ASC"),
            SortDirection::Desc => write!(f, "DESC"),
        }
    }
}

/// Result of building a query — SQL string + parameter bindings.
#[derive(Debug, Clone)]
pub struct BuiltQuery {
    pub sql: String,
    pub bindings: Vec<SqlValue>,
}

/// Safe dynamic SQL query builder.
///
/// Constructs SQL queries with `$N` positional parameter bindings.
/// Column names are validated to prevent SQL injection.
///
/// # Safety
///
/// - All values are passed as parameterized bindings (`$1`, `$2`, ...)
/// - Column names in `filter`, `order_by` etc. are validated: only `[a-zA-Z0-9_.]` allowed
/// - LIMIT and OFFSET use parameter bindings, never string interpolation
pub struct QueryBuilder {
    base_sql: String,
    conditions: Vec<String>,
    bindings: Vec<SqlValue>,
    order_clauses: Vec<String>,
    limit_offset: Option<(i64, i64)>,
}

impl QueryBuilder {
    /// Create a new query builder with a base SQL statement.
    ///
    /// The base SQL should be a complete statement without WHERE, ORDER BY,
    /// or LIMIT/OFFSET clauses (those are added by builder methods).
    ///
    /// ```rust
    /// use ranvier_db::QueryBuilder;
    /// let qb = QueryBuilder::new("SELECT * FROM users");
    /// ```
    pub fn new(base_sql: &str) -> Self {
        Self {
            base_sql: base_sql.to_string(),
            conditions: Vec::new(),
            bindings: Vec::new(),
            order_clauses: Vec::new(),
            limit_offset: None,
        }
    }

    /// Add a WHERE condition: `column = $N`.
    ///
    /// # Panics
    /// Panics if `column` contains invalid characters (only `[a-zA-Z0-9_.]` allowed).
    pub fn filter(mut self, column: &str, value: impl Into<SqlValue>) -> Self {
        validate_column_name(column);
        self.bindings.push(value.into());
        let idx = self.bindings.len();
        self.conditions.push(format!("{column} = ${idx}"));
        self
    }

    /// Add a WHERE condition only if the value is `Some`.
    ///
    /// When `value` is `None`, no condition is added.
    pub fn filter_optional(self, column: &str, value: &Option<impl Into<SqlValue> + Clone>) -> Self {
        match value {
            Some(v) => self.filter(column, v.clone().into()),
            None => self,
        }
    }

    /// Add a WHERE LIKE condition: `column LIKE $N`.
    ///
    /// The caller should include `%` wildcards in the pattern as needed.
    ///
    /// # Panics
    /// Panics if `column` contains invalid characters.
    pub fn filter_like(mut self, column: &str, pattern: &str) -> Self {
        validate_column_name(column);
        self.bindings.push(SqlValue::Text(pattern.to_string()));
        let idx = self.bindings.len();
        self.conditions.push(format!("{column} LIKE ${idx}"));
        self
    }

    /// Add a WHERE IN condition: `column IN ($N, $M, ...)`.
    ///
    /// # Panics
    /// Panics if `column` contains invalid characters or `values` is empty.
    pub fn filter_in(mut self, column: &str, values: Vec<SqlValue>) -> Self {
        validate_column_name(column);
        assert!(!values.is_empty(), "filter_in requires at least one value");
        let placeholders: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let idx = self.bindings.len() + i + 1;
                format!("${idx}")
            })
            .collect();
        self.conditions
            .push(format!("{column} IN ({})", placeholders.join(", ")));
        self.bindings.extend(values);
        self
    }

    /// Add an ORDER BY clause with validated column name.
    ///
    /// Multiple calls add additional sort columns.
    ///
    /// # Panics
    /// Panics if `column` contains invalid characters.
    pub fn order_by(mut self, column: &str, direction: SortDirection) -> Self {
        validate_column_name(column);
        self.order_clauses.push(format!("{column} {direction}"));
        self
    }

    /// Add LIMIT and OFFSET using parameter bindings.
    ///
    /// Both values are bound as parameters (`$N`, `$M`), never interpolated.
    pub fn paginate(mut self, limit: i64, offset: i64) -> Self {
        self.limit_offset = Some((limit, offset));
        self
    }

    /// Build the final SQL query with all conditions, ordering, and pagination.
    pub fn build(mut self) -> BuiltQuery {
        let mut sql = self.base_sql;

        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }

        if !self.order_clauses.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_clauses.join(", "));
        }

        if let Some((limit, offset)) = self.limit_offset {
            self.bindings.push(SqlValue::Int(limit));
            let limit_idx = self.bindings.len();
            self.bindings.push(SqlValue::Int(offset));
            let offset_idx = self.bindings.len();
            sql.push_str(&format!(" LIMIT ${limit_idx} OFFSET ${offset_idx}"));
        }

        BuiltQuery {
            sql,
            bindings: self.bindings,
        }
    }
}

/// Validates that a column name contains only safe characters: `[a-zA-Z0-9_.]`
///
/// # Panics
/// Panics with a descriptive message if the column name is invalid.
fn validate_column_name(column: &str) {
    assert!(
        !column.is_empty() && column.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'),
        "Invalid column name '{column}': only [a-zA-Z0-9_.] characters allowed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_select_with_filter() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter("status", "active")
            .build();
        assert_eq!(q.sql, "SELECT * FROM users WHERE status = $1");
        assert_eq!(q.bindings, vec![SqlValue::Text("active".into())]);
    }

    #[test]
    fn multiple_filters() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter("status", "active")
            .filter("role", "admin")
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM users WHERE status = $1 AND role = $2"
        );
        assert_eq!(q.bindings.len(), 2);
    }

    #[test]
    fn filter_optional_some() {
        let region = Some("APAC".to_string());
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter_optional("region", &region)
            .build();
        assert_eq!(q.sql, "SELECT * FROM users WHERE region = $1");
        assert_eq!(q.bindings, vec![SqlValue::Text("APAC".into())]);
    }

    #[test]
    fn filter_optional_none() {
        let region: Option<String> = None;
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter_optional("region", &region)
            .build();
        assert_eq!(q.sql, "SELECT * FROM users");
        assert!(q.bindings.is_empty());
    }

    #[test]
    fn filter_like() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter_like("name", "%john%")
            .build();
        assert_eq!(q.sql, "SELECT * FROM users WHERE name LIKE $1");
        assert_eq!(q.bindings, vec![SqlValue::Text("%john%".into())]);
    }

    #[test]
    fn filter_in() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter_in(
                "id",
                vec![SqlValue::Int(1), SqlValue::Int(2), SqlValue::Int(3)],
            )
            .build();
        assert_eq!(q.sql, "SELECT * FROM users WHERE id IN ($1, $2, $3)");
        assert_eq!(q.bindings.len(), 3);
    }

    #[test]
    fn order_by_single() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .order_by("name", SortDirection::Asc)
            .build();
        assert_eq!(q.sql, "SELECT * FROM users ORDER BY name ASC");
    }

    #[test]
    fn order_by_multiple() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .order_by("created_at", SortDirection::Desc)
            .order_by("name", SortDirection::Asc)
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM users ORDER BY created_at DESC, name ASC"
        );
    }

    #[test]
    fn paginate_with_bindings() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter("status", "active")
            .paginate(20, 0)
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM users WHERE status = $1 LIMIT $2 OFFSET $3"
        );
        assert_eq!(
            q.bindings,
            vec![
                SqlValue::Text("active".into()),
                SqlValue::Int(20),
                SqlValue::Int(0)
            ]
        );
    }

    #[test]
    fn full_query() {
        let q = QueryBuilder::new("SELECT * FROM departments")
            .filter("use_yn", "Y")
            .filter_optional("dept_cd", &Some("IT".to_string()))
            .filter_optional("manager", &None::<String>)
            .order_by("dept_nm", SortDirection::Asc)
            .paginate(20, 40)
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM departments WHERE use_yn = $1 AND dept_cd = $2 ORDER BY dept_nm ASC LIMIT $3 OFFSET $4"
        );
        assert_eq!(q.bindings.len(), 4);
    }

    #[test]
    fn no_conditions() {
        let q = QueryBuilder::new("SELECT COUNT(*) FROM users").build();
        assert_eq!(q.sql, "SELECT COUNT(*) FROM users");
        assert!(q.bindings.is_empty());
    }

    #[test]
    fn int_and_bool_values() {
        let q = QueryBuilder::new("SELECT * FROM settings")
            .filter("priority", 5i64)
            .filter("enabled", true)
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM settings WHERE priority = $1 AND enabled = $2"
        );
        assert_eq!(
            q.bindings,
            vec![SqlValue::Int(5), SqlValue::Bool(true)]
        );
    }

    #[test]
    fn dotted_column_name() {
        let q = QueryBuilder::new("SELECT t.* FROM users t")
            .filter("t.status", "active")
            .build();
        assert_eq!(q.sql, "SELECT t.* FROM users t WHERE t.status = $1");
    }

    #[test]
    #[should_panic(expected = "Invalid column name")]
    fn rejects_sql_injection_in_column() {
        QueryBuilder::new("SELECT * FROM users")
            .filter("name; DROP TABLE users", "x")
            .build();
    }

    #[test]
    #[should_panic(expected = "Invalid column name")]
    fn rejects_sql_injection_in_order_by() {
        QueryBuilder::new("SELECT * FROM users")
            .order_by("name; DROP TABLE users", SortDirection::Asc)
            .build();
    }

    #[test]
    #[should_panic(expected = "Invalid column name")]
    fn rejects_parentheses_in_column() {
        QueryBuilder::new("SELECT * FROM users")
            .filter("COUNT(*)", "1")
            .build();
    }

    #[test]
    #[should_panic(expected = "Invalid column name")]
    fn rejects_empty_column_name() {
        QueryBuilder::new("SELECT * FROM users")
            .filter("", "x")
            .build();
    }

    #[test]
    fn json_serialization() {
        let q = QueryBuilder::new("SELECT * FROM users")
            .filter("status", "active")
            .paginate(10, 20)
            .build();
        let json = serde_json::to_value(&q.bindings).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{"Text": "active"}, {"Int": 10}, {"Int": 20}])
        );
    }

    #[test]
    #[should_panic(expected = "filter_in requires at least one value")]
    fn filter_in_empty_panics() {
        QueryBuilder::new("SELECT * FROM users")
            .filter_in("id", vec![])
            .build();
    }
}
