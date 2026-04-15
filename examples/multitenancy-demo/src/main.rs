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
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Tenant-Scoped Store (simulates per-tenant data isolation)
// ============================================================================

#[derive(Clone)]
struct TenantStore {
    data: Arc<RwLock<HashMap<String, Vec<TenantRecord>>>>,
}

impl ResourceRequirement for TenantStore {}

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
            Ok(id) => {
                println!("  [Validate] Tenant context: {}", id.as_str());
                Outcome::Next(input)
            }
            Err(_) => {
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
            Ok(id) => id.as_str().to_string(),
            Err(_) => return Outcome::Fault("missing_tenant_id".to_string()),
        };

        resources
            .insert(&tenant_id, input.name.clone(), input.description.clone())
            .await;

        println!(
            "  [Store] Saved '{}' for tenant '{}'",
            input.name, tenant_id
        );

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
        _bus: &mut Bus,
    ) -> Outcome<TenantReport, Self::Error> {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

        let result = axon
            .execute(
                CreateItemRequest {
                    name: name.into(),
                    description: desc.into(),
                },
                &store,
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Next(created) => {
                println!("    Created: {} (tenant={})", created.name, created.tenant)
            }
            Outcome::Fault(e) => println!("    Error: {}", e),
            _ => {}
        }
    }

    // --- Demo 2: Tenant B creates items ---
    println!("\n--- Tenant B: Creating items ---");

    for (name, desc) in [("Sprocket", "An industrial sprocket")] {
        let mut bus = Bus::new();
        bus.insert(TenantId::new("tenant-b"));

        let result = axon
            .execute(
                CreateItemRequest {
                    name: name.into(),
                    description: desc.into(),
                },
                &store,
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Next(created) => {
                println!("    Created: {} (tenant={})", created.name, created.tenant)
            }
            Outcome::Fault(e) => println!("    Error: {}", e),
            _ => {}
        }
    }

    // --- Demo 3: No tenant context → rejection ---
    println!("\n--- No tenant context: Strict rejection ---");
    {
        let mut bus = Bus::new();
        let result = axon
            .execute(
                CreateItemRequest {
                    name: "Orphan".into(),
                    description: "No tenant".into(),
                },
                &store,
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Fault(e) => println!("    Rejected: {}", e),
            _ => println!("    Unexpected success"),
        }
    }

    // --- Demo 4: Per-tenant reports show isolation ---
    println!("\n--- Tenant-scoped reports (data isolation) ---");

    let report_axon =
        Axon::<String, String, String, TenantStore>::new("tenant.report").then(GenerateReport);

    for tenant in ["tenant-a", "tenant-b", "tenant-c"] {
        let mut bus = Bus::new();
        let result = report_axon
            .execute(tenant.to_string(), &store, &mut bus)
            .await;

        match &result {
            Outcome::Next(report) => {
                println!(
                    "    {}: {} item(s) {:?}",
                    report.tenant, report.item_count, report.items
                );
            }
            _ => {}
        }
    }

    println!("\ndone");
    Ok(())
}
