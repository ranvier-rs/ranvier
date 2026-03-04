use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone)]
struct WhoAmI;

#[async_trait::async_trait]
impl Transition<(), String> for WhoAmI {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let user = bus
            .read::<String>()
            .cloned()
            .unwrap_or_else(|| "anonymous".to_string());
        Outcome::next(user)
    }
}

#[tokio::test]
async fn bus_injector_moves_request_context_into_bus() {
    let ingress = Ranvier::http::<()>()
        .bus_injector(|req, bus| {
            if let Some(value) = req.headers.get("x-user") {
                if let Ok(user) = value.to_str() {
                    bus.insert(user.to_string());
                }
            }
        })
        .get(
            "/whoami",
            Axon::<(), (), String, ()>::new("WhoAmI").then(WhoAmI),
        );

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/whoami").header("x-user", "alice"))
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().expect("utf8 body"), "alice");
}
