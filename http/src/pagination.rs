//! Built-in pagination types for HTTP endpoints.
//!
//! Provides [`PageParams`] for extracting pagination query parameters from the Bus,
//! and [`Paginated<T>`] for wrapping paginated responses with metadata.
//!
//! # Example
//!
//! ```rust,ignore
//! use ranvier_core::prelude::*;
//! use ranvier_http::prelude::*;
//!
//! #[transition]
//! async fn list_items(_: (), _: &(), bus: &mut Bus) -> Outcome<Paginated<Item>, String> {
//!     let page = PageParams::from_bus(bus);
//!     let pool = try_outcome!(bus.get_cloned::<PgPool>(), "PgPool");
//!
//!     let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
//!         .fetch_one(&*pool).await.unwrap_or(0);
//!     let items = Outcome::from_result(
//!         sqlx::query_as("SELECT * FROM items LIMIT $1 OFFSET $2")
//!             .bind(page.per_page)
//!             .bind(page.offset())
//!             .fetch_all(&*pool).await
//!     );
//!     let items = try_outcome!(items);
//!     Outcome::Next(Paginated::new(items, total, &page))
//! }
//! ```

use serde::Serialize;

use crate::bus_ext::BusHttpExt;
use ranvier_core::bus::Bus;

/// Pagination query parameters extracted from the Bus.
///
/// Extracts `page` and `per_page` from HTTP query parameters with sensible defaults
/// and range clamping.
///
/// - `page`: 1-based page number (default: 1, min: 1)
/// - `per_page`: items per page (default: 20, min: 1, max: 200)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PageParams {
    pub page: i64,
    pub per_page: i64,
}

impl PageParams {
    /// Default page size.
    pub const DEFAULT_PER_PAGE: i64 = 20;
    /// Maximum allowed page size.
    pub const MAX_PER_PAGE: i64 = 200;

    /// Create new PageParams with automatic range clamping.
    ///
    /// - `page` is clamped to minimum 1
    /// - `per_page` is clamped to range [1, 200]
    pub fn new(page: i64, per_page: i64) -> Self {
        Self {
            page: page.max(1),
            per_page: per_page.clamp(1, Self::MAX_PER_PAGE),
        }
    }

    /// Extract PageParams from the Bus using explicit query parameter reads.
    ///
    /// Reads `page` and `per_page` query parameters. Missing parameters
    /// get default values (page=1, per_page=20). Values are clamped to valid ranges.
    pub fn from_bus(bus: &Bus) -> Self {
        let page: i64 = bus.query_param_or("page", 1);
        let per_page: i64 = bus.query_param_or("per_page", Self::DEFAULT_PER_PAGE);
        Self::new(page, per_page)
    }

    /// Calculate the SQL OFFSET value for this page.
    ///
    /// Returns `(page - 1) * per_page`.
    pub fn offset(&self) -> i64 {
        (self.page - 1) * self.per_page
    }
}

impl Default for PageParams {
    fn default() -> Self {
        Self::new(1, Self::DEFAULT_PER_PAGE)
    }
}

/// Paginated response wrapper with metadata.
///
/// Wraps a `Vec<T>` of items with pagination metadata for JSON serialization.
/// Use [`Paginated::new`] to construct from query results and [`PageParams`].
#[derive(Debug, Clone, Serialize)]
pub struct Paginated<T: Serialize> {
    pub items: Vec<T>,
    pub total_count: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

impl<T: Serialize> Paginated<T> {
    /// Create a new paginated response.
    ///
    /// `total_pages` is automatically calculated from `total_count` and `params.per_page`.
    pub fn new(items: Vec<T>, total_count: i64, params: &PageParams) -> Self {
        let total_pages = if params.per_page > 0 {
            (total_count + params.per_page - 1) / params.per_page
        } else {
            0
        };
        Self {
            items,
            total_count,
            page: params.page,
            per_page: params.per_page,
            total_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::bus::Bus;
    use crate::ingress::QueryParams;

    #[test]
    fn page_params_defaults() {
        let params = PageParams::default();
        assert_eq!(params.page, 1);
        assert_eq!(params.per_page, 20);
        assert_eq!(params.offset(), 0);
    }

    #[test]
    fn page_params_new_clamps_page() {
        let params = PageParams::new(0, 20);
        assert_eq!(params.page, 1);

        let params = PageParams::new(-5, 20);
        assert_eq!(params.page, 1);
    }

    #[test]
    fn page_params_new_clamps_per_page() {
        let params = PageParams::new(1, 0);
        assert_eq!(params.per_page, 1);

        let params = PageParams::new(1, 500);
        assert_eq!(params.per_page, 200);

        let params = PageParams::new(1, -10);
        assert_eq!(params.per_page, 1);
    }

    #[test]
    fn page_params_offset_calculation() {
        let params = PageParams::new(1, 20);
        assert_eq!(params.offset(), 0);

        let params = PageParams::new(2, 20);
        assert_eq!(params.offset(), 20);

        let params = PageParams::new(3, 50);
        assert_eq!(params.offset(), 100);
    }

    #[test]
    fn page_params_from_bus_with_query() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query("page=2&per_page=50"));

        let params = PageParams::from_bus(&bus);
        assert_eq!(params.page, 2);
        assert_eq!(params.per_page, 50);
    }

    #[test]
    fn page_params_from_bus_defaults() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query(""));

        let params = PageParams::from_bus(&bus);
        assert_eq!(params.page, 1);
        assert_eq!(params.per_page, 20);
    }

    #[test]
    fn page_params_from_bus_empty_bus() {
        let bus = Bus::new();
        let params = PageParams::from_bus(&bus);
        assert_eq!(params.page, 1);
        assert_eq!(params.per_page, 20);
    }

    #[test]
    fn page_params_from_bus_clamps_values() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query("page=0&per_page=999"));

        let params = PageParams::from_bus(&bus);
        assert_eq!(params.page, 1);
        assert_eq!(params.per_page, 200);
    }

    #[test]
    fn paginated_total_pages_exact() {
        let params = PageParams::new(1, 20);
        let paginated = Paginated::new(vec![1, 2, 3], 100, &params);
        assert_eq!(paginated.total_pages, 5);
        assert_eq!(paginated.total_count, 100);
        assert_eq!(paginated.page, 1);
        assert_eq!(paginated.per_page, 20);
    }

    #[test]
    fn paginated_total_pages_remainder() {
        let params = PageParams::new(1, 20);
        let paginated = Paginated::new(vec![1, 2, 3], 101, &params);
        assert_eq!(paginated.total_pages, 6);
    }

    #[test]
    fn paginated_total_pages_zero() {
        let params = PageParams::new(1, 20);
        let paginated: Paginated<i32> = Paginated::new(vec![], 0, &params);
        assert_eq!(paginated.total_pages, 0);
    }

    #[test]
    fn paginated_json_serialization() {
        let params = PageParams::new(2, 10);
        let paginated = Paginated::new(vec!["a", "b"], 25, &params);
        let json = serde_json::to_value(&paginated).unwrap();

        assert_eq!(json["items"], serde_json::json!(["a", "b"]));
        assert_eq!(json["total_count"], 25);
        assert_eq!(json["page"], 2);
        assert_eq!(json["per_page"], 10);
        assert_eq!(json["total_pages"], 3);
    }
}
