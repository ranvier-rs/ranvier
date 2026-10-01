//! Multi-Tenancy Demo
//!
//! ## Purpose
//! Demonstrates tenant isolation patterns using `ranvier_core::tenant` within
//! Axon workflows: tenant-scoped data access, Bus-injected tenant context,
//! and resource isolation best practices.
//!
//! ## Run
//! ```bash
//! cargo run -p multitenancy-demo
//! ```
//!
//! ## Key Concepts
//! - `TenantId` as a Bus capability for tenant context propagation
//! - Tenant-scoped transitions that read TenantId from Bus
//! - Per-tenant resource isolation via shared `TenantStore`
//! - Strict vs. default-tenant isolation policies
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `auth-jwt-role-demo` — authentication and role-based access
//!
//! ## Next Steps
//! - `session-pattern` — session management patterns
//! - `guard-demo` — HTTP guard pipeline (CORS, rate limit, IP filter)

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::tenant::TenantId;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

// ============================================================================
// Tenant-Scoped Store (simulates per-tenant data isolation)
// ============================================================================

#[derive(Clone)]
struct TenantStore {
    data: Arc<RwLock<HashMap<String, Vec<TenantRecord>>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TenantRecord {
    tenant: String,
    key: String,
    value: String,
}

impl TenantStore {
    fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn insert(&self, tenant: &str, key: String, value: String) {
        let mut data = self.data.write().await;
        data.entry(tenant.to_string())
            .or_default()
            .push(TenantRecord {
                tenant: tenant.to_string(),
                key,
                value,
            });
    }

    async fn list(&self, tenant: &str) -> Vec<TenantRecord> {
        let data = self.data.read().await;
        data.get(tenant).cloned().unwrap_or_default()
    }
}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateItemRequest {
    name: String,
    description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ItemCreated {
    tenant: String,
    name: String,
    description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TenantReport {
    tenant: String,
    item_count: usize,
    items: Vec<String>,
}

// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone)]
struct ValidateTenantContext;

#[async_trait]
impl Transition<CreateItemRequest, CreateItemRequest> for ValidateTenantContext {
    type Error = String;
    type Resources = TenantStore;

    async fn run(
        &self,
        input: CreateItemRequest,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<CreateItemRequest, Self::Error> {
        match bus.get_cloned::<TenantId>() {
            Ok(id) if valid_tenant(id.as_str()) => Outcome::Next(input),
            _ => {
                println!("  [Validate] No tenant context — rejecting");
                Outcome::Fault("missing_tenant_id".to_string())
            }
        }
    }
}

#[derive(Clone)]
struct StoreItem;

#[async_trait]
impl Transition<CreateItemRequest, ItemCreated> for StoreItem {
    type Error = String;
    type Resources = TenantStore;

    async fn run(
        &self,
        input: CreateItemRequest,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ItemCreated, Self::Error> {
        let tenant_id = match bus.get_cloned::<TenantId>() {
            Ok(id) if valid_tenant(id.as_str()) => id.as_str().to_string(),
            _ => return Outcome::Fault("missing_tenant_id".to_string()),
        };

        resources
            .insert(&tenant_id, input.name.clone(), input.description.clone())
            .await;

        Outcome::Next(ItemCreated {
            tenant: tenant_id,
            name: input.name,
            description: input.description,
        })
    }
}

#[derive(Clone)]
struct GenerateReport;

#[async_trait]
impl Transition<String, TenantReport> for GenerateReport {
    type Error = String;
    type Resources = TenantStore;

    async fn run(
        &self,
        tenant_id: String,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<TenantReport, Self::Error> {
        match bus.get_cloned::<TenantId>() {
            Ok(id) if valid_tenant(id.as_str()) && id.as_str() == tenant_id => {}
            _ => return Outcome::Fault("tenant_scope_mismatch".into()),
        }
        let records = resources.list(&tenant_id).await;
        let items: Vec<String> = records.iter().map(|r| r.key.clone()).collect();
        let report = TenantReport {
            tenant: tenant_id,
            item_count: items.len(),
            items,
        };
        Outcome::Next(report)
    }
}

// ============================================================================
// Main
// ============================================================================

async fn run_demo() -> anyhow::Result<()> {
    println!("=== Multi-Tenancy Demo ===\n");

    let store = TenantStore::new();

    // --- Demo 1: Tenant A creates items ---
    println!("--- Tenant A: Creating items ---");

    let axon = Axon::<CreateItemRequest, CreateItemRequest, String, TenantStore>::new(
        "tenant.create_item",
    )
    .then(ValidateTenantContext)
    .then(StoreItem);

    for (name, desc) in [
        ("Widget", "A standard widget"),
        ("Gadget", "A fancy gadget"),
    ] {
        let mut bus = Bus::new();
        bus.insert(TenantId::new("tenant-a"));

        let result = local_demo_outcome(
            axon.execute(
                CreateItemRequest {
                    name: name.into(),
                    description: desc.into(),
                },
                &store,
                &mut bus,
            )
            .await,
        )?;

        if !matches!(result, Outcome::Next(ref created) if created.tenant == "tenant-a" && created.name == name && created.description == desc)
        {
            anyhow::bail!("Created item did not match tenant scope");
        }
    }

    // --- Demo 2: Tenant B creates items ---
    println!("\n--- Tenant B: Creating items ---");

    for (name, desc) in [("Sprocket", "An industrial sprocket")] {
        let mut bus = Bus::new();
        bus.insert(TenantId::new("tenant-b"));

        let result = local_demo_outcome(
            axon.execute(
                CreateItemRequest {
                    name: name.into(),
                    description: desc.into(),
                },
                &store,
                &mut bus,
            )
            .await,
        )?;

        if !matches!(result, Outcome::Next(ref created) if created.tenant == "tenant-b" && created.name == name && created.description == desc)
        {
            anyhow::bail!("Created item did not match tenant scope");
        }
    }

    // --- Demo 3: No tenant context → rejection ---
    println!("\n--- No tenant context: Strict rejection ---");
    {
        let mut bus = Bus::new();
        let result = local_demo_outcome(
            axon.execute(
                CreateItemRequest {
                    name: "Orphan".into(),
                    description: "No tenant".into(),
                },
                &store,
                &mut bus,
            )
            .await,
        )?;

        if !matches!(result, Outcome::Fault(ref error) if error == "missing_tenant_id") {
            anyhow::bail!("Missing tenant was not rejected");
        }
    }

    // --- Demo 4: Per-tenant reports show isolation ---
    println!("\n--- Tenant-scoped reports (data isolation) ---");

    let report_axon =
        Axon::<String, String, String, TenantStore>::new("tenant.report").then(GenerateReport);

    for (tenant, expected) in [
        ("tenant-a", vec!["Widget", "Gadget"]),
        ("tenant-b", vec!["Sprocket"]),
        ("tenant-c", vec![]),
    ] {
        let mut bus = Bus::new();
        bus.insert(TenantId::new(tenant));
        let result = local_demo_outcome(
            report_axon
                .execute(tenant.to_string(), &store, &mut bus)
                .await,
        )?;

        if !matches!(result, Outcome::Next(ref report) if report.tenant == tenant && report.item_count == expected.len() && report.items == expected)
        {
            anyhow::bail!("Tenant report did not match isolated records");
        }
    }

    println!("\ndone");
    Ok(())
}

fn valid_tenant(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_demo().await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_create_and_scoped_reports() {
        run_demo().await.unwrap();
    }
    #[tokio::test]
    async fn foreign_report_and_invalid_context_are_rejected() {
        let store = TenantStore::new();
        store
            .insert("tenant-b", "private".into(), "secret".into())
            .await;
        let flow = Axon::<String, String, String, TenantStore>::new("read").then(GenerateReport);
        let mut bus = Bus::new();
        bus.insert(TenantId::new("tenant-a"));
        let result =
            local_demo_outcome(flow.execute("tenant-b".into(), &store, &mut bus).await).unwrap();
        assert!(matches!(result, Outcome::Fault(error) if error == "tenant_scope_mismatch"));
        for tenant in ["", " ", "../tenant-b"] {
            let mut bus = Bus::new();
            bus.insert(TenantId::new(tenant));
            assert!(matches!(
                StoreItem
                    .run(
                        CreateItemRequest {
                            name: "item".into(),
                            description: "value".into()
                        },
                        &store,
                        &mut bus
                    )
                    .await,
                Outcome::Fault(_)
            ));
        }
        assert_eq!(store.list("tenant-b").await.len(), 1);
        assert!(store.list("").await.is_empty());
    }
}
