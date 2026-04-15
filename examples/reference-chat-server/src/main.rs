//! # Reference Chat Server
//!
//! A multi-room WebSocket chat server built with Ranvier.
//!
//! Demonstrates:
//! - WebSocket connections with room management
//! - JWT-style authentication
//! - `RanvierConfig` for configuration management
//! - REST + WebSocket hybrid routing
//! - In-memory message persistence
//!
//! ## Running
//!
//! ```sh
//! cargo run -p reference-chat-server
//! ```
//!
//! ## Endpoints
//!
//! - `POST /login` — Get auth token (body: `{"username": "alice"}`)
//! - `GET /rooms` — List public rooms
//! - `POST /rooms` — Create room (auth required, body: `{"name": "my-room"}`)
//! - `GET /rooms/:id/history` — Get room message history
//! - `GET /ws` — WebSocket connection (query: `?token=tok_xxx`)

mod auth;
mod models;
mod transitions;
mod ws;

use ranvier_core::config::RanvierConfig;
use ranvier_http::{PathParams, Ranvier};
use ranvier_runtime::Axon;
use ws::room_manager::RoomManager;

use transitions::create_room::create_room;
use transitions::list_rooms::list_rooms;
use transitions::login::login;
use transitions::room_history::room_history;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = RanvierConfig::load()?;
    config.init_logging();

    let token_store = auth::new_token_store();
    let room_manager = RoomManager::new();

    tracing::info!(
        host = %config.server.host,
        port = %config.server.port,
        "Starting chat server"
    );

    let token_store_for_bus = token_store.clone();
    let room_manager_for_bus = room_manager.clone();

    Ranvier::http()
        .config(&config)
        .graceful_shutdown(config.shutdown_timeout())
        .health_endpoint("/health")
        .readiness_liveness_default()
        .bus_injector(move |parts, bus| {
            bus.insert(token_store_for_bus.clone());
            bus.insert(room_manager_for_bus.clone());

            if let Some(auth) = parts.headers.get("authorization")
                && let Ok(value) = auth.to_str()
            {
                bus.insert(value.to_string());
            }

            if let Some(params) = parts.extensions.get::<PathParams>() {
                bus.insert(params.clone());
            }
        })
        .post_json(
            "/login",
            Axon::typed::<serde_json::Value, String>("login").then(login),
        )
        .get(
            "/rooms",
            Axon::simple::<String>("list_rooms").then(list_rooms),
        )
        .post_json(
            "/rooms",
            Axon::typed::<serde_json::Value, String>("create_room").then(create_room),
        )
        .get(
            "/rooms/:id/history",
            Axon::simple::<String>("room_history").then(room_history),
        )
        .ws("/ws", ws::handler::handle_ws)
        .on_start(|| {
            tracing::info!("Chat server started — connect via WebSocket at /ws?token=<token>");
        })
        .on_shutdown(|| {
            tracing::info!("Chat server shut down");
        })
        .run(())
        .await
}
