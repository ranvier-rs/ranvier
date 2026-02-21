use std::convert::Infallible;
use std::path::PathBuf;

use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone)]
struct ApiPing;

#[async_trait::async_trait]
impl Transition<(), String> for ApiPing {
    type Error = Infallible;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next("pong".to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let public_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("public");
    let index_file = public_dir.join("index.html");

    println!("static-spa-demo listening on http://127.0.0.1:3112");
    println!("  static file: http://127.0.0.1:3112/static/app.js");
    println!("  spa route:   http://127.0.0.1:3112/dashboard/settings");
    println!("  api route:   http://127.0.0.1:3112/api/ping");

    Ranvier::http::<()>()
        .bind("127.0.0.1:3112")
        .serve_dir("/static", public_dir.to_string_lossy().to_string())
        .spa_fallback(index_file.to_string_lossy().to_string())
        .compression_layer()
        .get(
            "/api/ping",
            Axon::<(), (), Infallible, ()>::new("ApiPing").then(ApiPing),
        )
        .run(())
        .await
}
