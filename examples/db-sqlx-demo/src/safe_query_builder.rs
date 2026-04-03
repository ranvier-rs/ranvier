//! Local helper for safe dynamic SQL construction.
//!
//! This is intentionally kept as example-local code instead of a public
//! framework crate. Copy/adapt it into application code when needed.
#![allow(dead_code)]

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
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
            Self::Int(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Text(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Null => write!(f, "NULL"),
        }
    }
}

impl From<i64> for SqlValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl From<i32> for SqlValue {
    fn from(v: i32) -> Self {
        Self::Int(v as i64)
    }
}

impl From<f64> for SqlValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<String> for SqlValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<&str> for SqlValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}

impl From<bool> for SqlValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl fmt::Display for SortDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asc => write!(f, "ASC"),
            Self::Desc => write!(f, "DESC"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuiltQuery {
    pub sql: String,
    pub bindings: Vec<SqlValue>,
}

#[derive(Debug, Clone)]
pub struct QueryBuilder {
    base_sql: String,
    conditions: Vec<String>,
    bindings: Vec<SqlValue>,
    order_clauses: Vec<String>,
    limit_offset: Option<(i64, i64)>,
}

impl QueryBuilder {
    pub fn new(base_sql: &str) -> Self {
        Self {
            base_sql: base_sql.to_string(),
            conditions: Vec::new(),
            bindings: Vec::new(),
            order_clauses: Vec::new(),
            limit_offset: None,
        }
    }

    pub fn filter(mut self, column: &str, value: impl Into<SqlValue>) -> Self {
        validate_column_name(column);
        match value.into() {
            SqlValue::Null => {
                self.conditions.push(format!("{column} IS NULL"));
            }
            value => {
                self.bindings.push(value);
                let idx = self.bindings.len();
                self.conditions.push(format!("{column} = ${idx}"));
            }
        }
        self
    }

    pub fn filter_optional(
        self,
        column: &str,
        value: &Option<impl Into<SqlValue> + Clone>,
    ) -> Self {
        match value {
            Some(v) => self.filter(column, v.clone().into()),
            None => self,
        }
    }

    pub fn filter_like(mut self, column: &str, pattern: &str) -> Self {
        validate_column_name(column);
        self.bindings.push(SqlValue::Text(pattern.to_string()));
        let idx = self.bindings.len();
        self.conditions.push(format!("{column} LIKE ${idx}"));
        self
    }

    pub fn filter_in(mut self, column: &str, values: Vec<SqlValue>) -> Self {
        validate_column_name(column);
        assert!(!values.is_empty(), "filter_in requires at least one value");
        assert!(
            !values.iter().any(|value| matches!(value, SqlValue::Null)),
            "filter_in does not support NULL values; use a separate IS NULL clause"
        );
        let placeholders: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", self.bindings.len() + i + 1))
            .collect();
        self.conditions
            .push(format!("{column} IN ({})", placeholders.join(", ")));
        self.bindings.extend(values);
        self
    }

    pub fn order_by(mut self, column: &str, direction: SortDirection) -> Self {
        validate_column_name(column);
        self.order_clauses.push(format!("{column} {direction}"));
        self
    }

    pub fn paginate(mut self, limit: i64, offset: i64) -> Self {
        self.limit_offset = Some((limit, offset));
        self
    }

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

fn validate_column_name(column: &str) {
    assert!(
        !column.is_empty()
            && column
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'),
        "Invalid column name '{column}': only [a-zA-Z0-9_.] characters allowed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_paginated_query() {
        let query = QueryBuilder::new("SELECT * FROM users")
            .filter("status", "active")
            .order_by("created_at", SortDirection::Desc)
            .paginate(20, 40)
            .build();

        assert_eq!(
            query.sql,
            "SELECT * FROM users WHERE status = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        );
        assert_eq!(
            query.bindings,
            vec![
                SqlValue::Text("active".to_string()),
                SqlValue::Int(20),
                SqlValue::Int(40),
            ]
        );
    }

    #[test]
    fn skips_optional_filter_when_none() {
        let none_value: Option<String> = None;
        let query = QueryBuilder::new("SELECT * FROM users")
            .filter_optional("role", &none_value)
            .build();

        assert_eq!(query.sql, "SELECT * FROM users");
        assert!(query.bindings.is_empty());
    }

    #[test]
    fn builds_like_and_in_query() {
        let query = QueryBuilder::new("SELECT * FROM users")
            .filter_like("name", "ali%")
            .filter_in(
                "status",
                vec![
                    SqlValue::Text("active".to_string()),
                    SqlValue::Text("pending".to_string()),
                    SqlValue::Text("disabled".to_string()),
                ],
            )
            .build();

        assert_eq!(
            query.sql,
            "SELECT * FROM users WHERE name LIKE $1 AND status IN ($2, $3, $4)"
        );
        assert_eq!(
            query.bindings,
            vec![
                SqlValue::Text("ali%".to_string()),
                SqlValue::Text("active".to_string()),
                SqlValue::Text("pending".to_string()),
                SqlValue::Text("disabled".to_string()),
            ]
        );
    }

    #[test]
    fn renders_is_null_without_binding() {
        let query = QueryBuilder::new("SELECT * FROM users")
            .filter("deleted_at", SqlValue::Null)
            .build();

        assert_eq!(query.sql, "SELECT * FROM users WHERE deleted_at IS NULL");
        assert!(query.bindings.is_empty());
    }

    #[test]
    #[should_panic(expected = "filter_in does not support NULL values")]
    fn rejects_null_in_filter_in() {
        let _ = QueryBuilder::new("SELECT * FROM users")
            .filter_in("status", vec![SqlValue::Null])
            .build();
    }

    #[test]
    #[should_panic(expected = "Invalid column name")]
    fn rejects_sql_injection_attempt() {
        let _ = QueryBuilder::new("SELECT * FROM users")
            .order_by("name; DROP TABLE users", SortDirection::Asc)
            .build();
    }
}
