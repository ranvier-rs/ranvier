//! # ranvier-db — Safe Dynamic SQL Query Builder
//!
//! Provides [`QueryBuilder`] for constructing dynamic SQL queries with
//! parameterized bindings, preventing SQL injection by design.
//!
//! ## Key Features
//!
//! - **Parameter binding**: All values use `$N` positional parameters (never interpolated)
//! - **Column name validation**: Only `[a-zA-Z0-9_.]` allowed in column names
//! - **Optional filters**: `filter_optional` adds conditions only when `Some`
//! - **Safe ORDER BY**: Column names validated before inclusion
//! - **Protocol independent**: No HTTP or framework dependencies
//!
//! ## Example
//!
//! ```rust
//! use ranvier_db::prelude::*;
//!
//! let query = QueryBuilder::new("SELECT * FROM departments")
//!     .filter("status", "active")
//!     .filter_optional("region", &Some("APAC".to_string()))
//!     .filter_optional("manager", &None::<String>)
//!     .order_by("name", SortDirection::Asc)
//!     .paginate(20, 0)
//!     .build();
//!
//! assert_eq!(
//!     query.sql,
//!     "SELECT * FROM departments WHERE status = $1 AND region = $2 ORDER BY name ASC LIMIT $3 OFFSET $4"
//! );
//! assert_eq!(query.bindings.len(), 4);
//! ```

pub mod query_builder;

pub mod prelude {
    pub use crate::query_builder::{BuiltQuery, QueryBuilder, SortDirection, SqlValue};
}

pub use query_builder::{BuiltQuery, QueryBuilder, SortDirection, SqlValue};
