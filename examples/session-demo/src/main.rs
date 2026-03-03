use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use ranvier_session::prelude::*;
use ranvier_session::layer::inject_session;
use bytes::Bytes;
use http::Response;
use http_body_util::Full;
use tracing::{info, Level};

/// A counter state struct that we can store in the session.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct VisitorInfo {
    visits: u32,
    last_visited: String,
}

/// Transition that reads and updates the visitor info from the session.
#[transition]
async fn visit_counter(_state: (), _resources: &(), bus: &mut Bus) -> Outcome<String, String> {
    // Extract the session from the Bus (it was injected by the `bus_injector`)
    let session_opt = bus.get::<Session>().ok();
    
    match session_opt {
        Some(session) => {
            // Retrieve current visits (or default to 0)
            let mut info: VisitorInfo = session
                .get("visitor_info")
                .await
                .and_then(|res| res.ok())
                .unwrap_or(VisitorInfo { visits: 0, last_visited: "".to_string() });
            
            info.visits += 1;
            info.last_visited = chrono::Utc::now().to_rfc3339();
            
            // Save the updated info back to the session
            if let Err(e) = session.insert("visitor_info", &info).await {
                tracing::error!("Failed to save session data: {}", e);
            }

            Outcome::Next(format!(
                "Hello! This is visit #{}.\nLast visited at: {}\nSession ID: {}",
                info.visits,
                info.last_visited,
                session.id().await
            ))
        }
        None => {
            info!("No session found in Bus.");
            Outcome::Next("No session found. Are cookies disabled?".to_string())
        }
    }
}

/// A reset transition to clear the session data.
#[transition]
async fn reset_session(_state: (), _resources: &(), bus: &mut Bus) -> Outcome<String, String> {
    if let Some(session) = bus.get::<Session>().ok() {
        session.destroy().await;
        Outcome::Next("Session destroyed. Visit / again to start a new session!".to_string())
    } else {
        Outcome::Next("No active session to clear.".to_string())
    }
}

// Map the outcomes to proper HTTP responses
fn format_success(msg: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(200)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(msg)))
        .unwrap()
}

fn handle_error(err: &String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(500)
        .body(Full::new(Bytes::from(format!("Internal Error: {}", err))))
        .unwrap()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Starting Ranvier Session Demo (MemoryStore)");

    // 1. Initialize the MemoryStore for sessions
    let store = MemoryStore::new();

    // 2. Build our standard Axons
    let visit_axon = Axon::<(), (), String>::new("VisitCounter")
        .then(visit_counter);
        
    let reset_axon = Axon::<(), (), String>::new("ResetSession")
        .then(reset_session);

    // 3. Build the HTTP Ingress
    let ingress = Ranvier::http()
        .bind("127.0.0.1:3000") // Localhost port 3000
        // Use the tower middleware to manage session load + save around requests
        .layer(SessionLayer::new(store).with_cookie_name("ranvier.sid"))
        // Inject the extracted Session into the Bus before Axons execute
        .bus_injector(inject_session)
        // Bind the routes
        .route_method_with_error(http::Method::GET, "/", visit_axon, handle_error)
        .route_method_with_error(http::Method::GET, "/reset", reset_axon, handle_error);

    info!("Server listening on http://127.0.0.1:3000");
    ingress.run(()).await.map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
