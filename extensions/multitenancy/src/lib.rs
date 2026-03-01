pub mod layer;

use serde::{Deserialize, Serialize};

/// Represents a distinct Tenant within a Ranvier execution cluster.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Helper extension trait to retrieve TenantId from a generic Context or Bus payload.
pub trait TenantExtractor {
    fn tenant_id(&self) -> Option<TenantId>;
}

/// A policy defining how isolated routing is handled for a generic endpoint.
#[derive(Debug, Clone)]
pub enum IsolationPolicy {
    /// Disallow access if no tenant ID is present
    Strict,
    /// Fallback to a default tenant
    DefaultTenant(TenantId),
}

/// Identifiers for resolving tenants from raw HTTP requests
#[derive(Debug, Clone)]
pub enum TenantResolver {
    /// Header name holding the tenant ID (e.g. X-Tenant-ID)
    Header(&'static str),
    /// Subdomain index (e.g. 0 for tenant1.example.com)
    Subdomain,
    /// URL path prefix
    PathPrefix,
}
