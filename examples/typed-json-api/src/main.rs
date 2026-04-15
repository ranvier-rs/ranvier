/*!
# Typed JSON API Demo (v0.43)

Demonstrates **typed JSON auto-serialization** at the HTTP route boundary.
Transitions return domain structs — JSON serialization is infrastructure.

## Featured APIs

- **`get_json_out`**: `Outcome<T, E>` → auto-serialized JSON response
- **`post_typed_json_out`**: Typed JSON body + typed JSON output
- **`delete_json_out`**: DELETE with typed JSON response
- **`BusHttpExt`**: `path_param()`, `query_param()`, `query_param_or()`
- **`Bus::get_cloned()`**: Concise resource extraction
- **`try_outcome!`**: Ergonomic `Result → Outcome::Fault` conversion
- **`CorsGuard::permissive()`**: One-line dev CORS

## Running

```bash
cargo run -p typed-json-api
# Then: curl http://localhost:3100/api/items
#        curl -X POST http://localhost:3100/api/items -H 'Content-Type: application/json' -d '{"name":"demo","price":9.99}'
#        curl http://localhost:3100/api/items/{id}
#        curl -X DELETE http://localhost:3100/api/items/{id}
```

## Design

No database — uses `Arc<RwLock<Vec<Item>>>` as an in-memory store injected
via `bus_injector`. This keeps the example focused on HTTP/JSON patterns.
*/

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use ranvier_core::{prelude::*, try_outcome};
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ─── Models ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateItemInput {
    pub name: String,
    pub price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemPage {
    pub items: Vec<Item>,
    pub page: i64,
    pub total: usize,
}

type Store = Arc<RwLock<Vec<Item>>>;

// ─── Transitions ────────────────────────────────────────────────

/// Lists items with pagination via `query_param_or`.
#[derive(Clone, Copy)]
struct ListItems;

#[async_trait]
impl Transition<(), ItemPage> for ListItems {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ItemPage, Self::Error> {
        let store = try_outcome!(bus.get_cloned::<Store>(), "Store not in Bus");
        let page: i64 = bus.query_param_or("page", 1);
        let per_page: i64 = bus.query_param_or("per_page", 10);

        let items = store.read().unwrap();
        let total = items.len();
        let start = ((page - 1) * per_page).max(0) as usize;
        let end = (start + per_page as usize).min(total);
        let page_items = items[start..end].to_vec();

        Outcome::Next(ItemPage {
            items: page_items,
            page,
            total,
        })
    }
}

/// Creates an item from typed JSON input.
#[derive(Clone, Copy)]
struct CreateItem;

#[async_trait]
impl Transition<CreateItemInput, Item> for CreateItem {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: CreateItemInput,
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<Item, Self::Error> {
        let store = try_outcome!(bus.get_cloned::<Store>(), "Store not in Bus");
        let item = Item {
            id: Uuid::new_v4().to_string(),
            name: input.name,
            price: input.price,
        };
        store.write().unwrap().push(item.clone());
        Outcome::Next(item)
    }
}

/// Gets a single item by path parameter.
#[derive(Clone, Copy)]
struct GetItem;

#[async_trait]
impl Transition<(), Item> for GetItem {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<Item, Self::Error> {
        let id: String = try_outcome!(bus.path_param("id"));
        let store = try_outcome!(bus.get_cloned::<Store>(), "Store not in Bus");
        let items = store.read().unwrap();
        match items.iter().find(|i| i.id == id) {
            Some(item) => Outcome::Next(item.clone()),
            None => Outcome::Fault(format!("Item not found: {id}")),
        }
    }
}

/// Deletes an item by path parameter.
#[derive(Clone, Copy)]
struct DeleteItem;

#[async_trait]
impl Transition<(), DeleteResult> for DeleteItem {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<DeleteResult, Self::Error> {
        let id: String = try_outcome!(bus.path_param("id"));
        let store = try_outcome!(bus.get_cloned::<Store>(), "Store not in Bus");
        let mut items = store.write().unwrap();
        let before = items.len();
        items.retain(|i| i.id != id);
        Outcome::Next(DeleteResult {
            deleted: items.len() < before,
            id,
        })
    }
}

// ─── Main ───────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store: Store = Arc::new(RwLock::new(vec![
        Item {
            id: Uuid::new_v4().to_string(),
            name: "Widget".into(),
            price: 19.99,
        },
        Item {
            id: Uuid::new_v4().to_string(),
            name: "Gadget".into(),
            price: 49.99,
        },
    ]));

    let list = Axon::simple::<String>("list-items").then(ListItems);
    let create = Axon::typed::<CreateItemInput, String>("create-item").then(CreateItem);
    let get = Axon::simple::<String>("get-item").then(GetItem);
    let delete = Axon::simple::<String>("delete-item").then(DeleteItem);

    println!("╔═══════════════════════════════════════════════╗");
    println!("║  Typed JSON API Demo — Ranvier v0.43          ║");
    println!("║  http://localhost:3100/api/items               ║");
    println!("╚═══════════════════════════════════════════════╝");

    Ranvier::http()
        .bind("127.0.0.1:3100")
        .bus_injector({
            let store = store.clone();
            move |_parts, bus| {
                bus.insert(store.clone());
            }
        })
        .guard(AccessLogGuard::<()>::new())
        .guard(CorsGuard::<()>::permissive())
        // Typed JSON auto-serialization at route boundary
        .get_json_out("/api/items", list)
        .get_json_out("/api/items/:id", get)
        .post_typed_json_out("/api/items", create)
        .delete_json_out("/api/items/:id", delete)
        .run(())
        .await
}
