//! Multi-tenancy primitives for the Ranvier paradigm.
//!
//! Provides protocol-agnostic tenant isolation types that Transitions
//! consume via the Bus:
//!
//! * [`TenantId`] — typed tenant identifier (Bus-injectable)
//! * [`TenantExtractor`] — trait for resolving a tenant from context
//! * [`IsolationPolicy`] — how strict tenant enforcement is
//! * [`TenantResolver`] — hint for where to find the tenant in HTTP requests

use serde::{Deserialize, Serialize};

// ── Tenant ID ─────────────────────────────────────────────────

/// Represents a distinct Tenant within a Ranvier execution cluster.
///
/// Insert into the Bus at the HTTP boundary; Transitions read it via
/// `bus.read::<TenantId>()`.
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

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Extractor trait ───────────────────────────────────────────

/// Helper extension trait to retrieve [`TenantId`] from a generic
/// Context or Bus payload.
pub trait TenantExtractor {
    fn tenant_id(&self) -> Option<TenantId>;
}

// ── Isolation policy ──────────────────────────────────────────

/// A policy defining how isolated routing is handled for a generic endpoint.
#[derive(Debug, Clone)]
pub enum IsolationPolicy {
    /// Disallow access if no tenant ID is present.
    Strict,
    /// Fallback to a default tenant.
    DefaultTenant(TenantId),
}

// ── Resolver hint ─────────────────────────────────────────────

/// Identifiers for resolving tenants from raw HTTP requests.
///
/// Used by the HTTP boundary (or a Transition) to decide where to
/// look for the tenant identifier.
#[derive(Debug, Clone)]
pub enum TenantResolver {
    /// Header name holding the tenant ID (e.g. `X-Tenant-ID`).
    Header(&'static str),
    /// Subdomain index (e.g. 0 for `tenant1.example.com`).
    Subdomain,
    /// URL path prefix.
    PathPrefix,
}

// ── Bus helpers ───────────────────────────────────────────────

/// Insert a [`TenantId`] into the Bus.
pub fn inject_tenant_id(bus: &mut crate::bus::Bus, id: TenantId) {
    bus.insert(id);
}

/// Read the [`TenantId`] from the Bus.
pub fn tenant_id(bus: &crate::bus::Bus) -> Option<&TenantId> {
    bus.read::<TenantId>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    #[test]
    fn tenant_id_roundtrip() {
        let id = TenantId::new("acme-corp");
        assert_eq!(id.as_str(), "acme-corp");
        assert_eq!(id.to_string(), "acme-corp");
    }

    #[test]
    fn tenant_id_bus_inject_and_read() {
        let mut bus = Bus::new();
        inject_tenant_id(&mut bus, TenantId::new("tenant-1"));
        let read = tenant_id(&bus).expect("should be present");
        assert_eq!(read.as_str(), "tenant-1");
    }

    #[test]
    fn tenant_id_equality() {
        let a = TenantId::new("alpha");
        let b = TenantId::new("alpha");
        let c = TenantId::new("beta");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn tenant_id_serde_roundtrip() {
        let id = TenantId::new("serialized");
        let json = serde_json::to_string(&id).expect("serialize");
        let back: TenantId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }
}
