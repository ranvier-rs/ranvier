use std::time::Duration;

use http::header::HeaderName;
use ranvier_core::prelude::*;
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone)]
struct PublicHello;

#[async_trait::async_trait]
impl Transition<(), String> for PublicHello {
    type Error = Infallible;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next("public endpoint".to_string())
    }
}

#[derive(Clone)]
struct BurstHello;

#[async_trait::async_trait]
impl Transition<(), String> for BurstHello {
    type Error = Infallible;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next("burst endpoint".to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("guard-demo listening on http://127.0.0.1:3110");
    println!("Try OPTIONS preflight with Origin header and repeated GETs with x-client-id.");

    let client_header = HeaderName::from_static("x-client-id");
    let global_rate_limit = RateLimitLayer::new(
        RateLimitPolicy::new(30, Duration::from_secs(60)).key_header(client_header.clone()),
    );
    let burst_rate_limit = RateLimitLayer::new(
        RateLimitPolicy::new(5, Duration::from_secs(60)).key_header(client_header),
    );

    let public = Axon::<(), (), String, ()>::new("PublicHello").then(PublicHello);
    let burst = Axon::<(), (), String, ()>::new("BurstHello").then(BurstHello);

    Ranvier::http::<()>()
        .bind("127.0.0.1:3110")
        .layer(CorsGuardLayer::origins(["http://localhost:5173"]))
        .layer(SecurityHeadersLayer::default())
        .layer(global_rate_limit)
        .get("/public", public)
        .get_with_layer("/burst", burst, burst_rate_limit)
        .run(())
        .await
}
