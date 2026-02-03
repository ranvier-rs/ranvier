use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ranvier_core::schematic::Schematic;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

static EVENT_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();

fn get_sender() -> &'static broadcast::Sender<String> {
    EVENT_CHANNEL.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(100);
        tx
    })
}

/// Start the Inspector Server.
pub struct Inspector {
    port: u16,
    schematic: Arc<Mutex<Schematic>>,
}

impl Inspector {
    pub fn new(schematic: Schematic, port: u16) -> Self {
        // Ensure channel exists
        get_sender();

        Self {
            port,
            schematic: Arc::new(Mutex::new(schematic)),
        }
    }

    pub async fn serve(self) -> Result<(), std::io::Error> {
        let app = Router::new()
            .route("/schematic", get(get_schematic))
            .route("/events", get(ws_handler))
            .layer(CorsLayer::permissive())
            .with_state(self.schematic);

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        tracing::info!("Ranvier Inspector listening on http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await
    }
}

pub fn layer() -> InspectorLayer {
    InspectorLayer
}

pub struct InspectorLayer;

impl<S> Layer<S> for InspectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target().starts_with("ranvier") {
            // Simple JSON serialization of the event
            // In a real impl, we'd use a visitor to extract fields
            let msg = format!(
                "{{\"type\": \"event\", \"target\": \"{}\", \"level\": \"{}\"}}",
                metadata.target(),
                metadata.level()
            );
            let _ = get_sender().send(msg);
        }
    }

    // Using on_enter/exit for Span tracking would be better for Node visualization
    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if span.name() == "Node" {
                // Send Node Enter Event
                // We need extensions to really get data, but name is a start
                let msg = format!(
                    "{{\"type\": \"node_enter\", \"name\": \"{}\"}}",
                    span.name()
                );
                let _ = get_sender().send(msg);
            }
        }
    }
}

async fn get_schematic(State(schematic): State<Arc<Mutex<Schematic>>>) -> Json<Schematic> {
    let schematic = schematic.lock().unwrap();
    Json(schematic.clone())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(_): State<Arc<Mutex<Schematic>>>,
) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    let mut rx = get_sender().subscribe();

    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg)).await.is_err() {
            break;
        }
    }
}
