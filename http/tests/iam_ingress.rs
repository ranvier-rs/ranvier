use ranvier_core::iam::{IamError, IamIdentity, IamPolicy, IamToken, IamVerifier};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::{Ranvier, TestApp, TestRequest};
use ranvier_runtime::Axon;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Verifier;
#[async_trait::async_trait]
impl IamVerifier for Verifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        match token {
            "admin" => Ok(IamIdentity::new("private-subject").with_role("admin")),
            "user" => Ok(IamIdentity::new("private-subject").with_role("user")),
            "panic" => panic!("synthetic ingress verifier panic"),
            _ => Err(IamError::InvalidToken("private-verifier-detail".into())),
        }
    }
}
#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl<I: Send + Sync + 'static> Transition<I, String> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: I, _: &(), _: &mut Bus) -> Outcome<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next("ok".into())
    }
}
#[tokio::test]
async fn iam_denials_precede_ingress_route_specific_response_mapping() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let calls = Arc::new(AtomicUsize::new(0));
        let ordinary = || {
            Axon::<(), (), String>::start("ingress-iam")
                .with_iam(IamPolicy::RequireRole("admin".into()), Verifier)
                .then(Effect(calls.clone()))
        };
        let typed = || {
            Axon::<serde_json::Value, serde_json::Value, String>::start("ingress-typed-iam")
                .with_iam(IamPolicy::RequireRole("admin".into()), Verifier)
                .then(Effect(calls.clone()))
        };
        let ingress = Ranvier::http::<()>()
            .bus_injector(|parts, bus| {
                if let Some(token) = parts
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                {
                    bus.insert(IamToken(token.into()));
                }
            })
            .get("/ordinary", ordinary())
            .get_json_out("/json", ordinary())
            .post_typed("/typed", typed())
            .post_typed_json_out("/typed-json", typed())
            .fallback(ordinary());
        let app = TestApp::new(ingress, ());
        let mut successful = 0;
        for path in ["/ordinary", "/json", "/typed", "/typed-json", "/fallback"] {
            for (token, status) in [
                (None, 401),
                (Some("invalid"), 401),
                (Some("user"), 403),
                (Some("panic"), 500),
                (Some("admin"), if path == "/fallback" { 404 } else { 200 }),
            ] {
                let mut request = if path.starts_with("/typed") {
                    TestRequest::post(path)
                        .json(&serde_json::json!({}))
                        .unwrap()
                } else {
                    TestRequest::get(path)
                };
                if let Some(token) = token {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                let response = app.send(request).await.unwrap();
                assert_eq!(
                    response.status().as_u16(),
                    status,
                    "path {path}, token {token:?}"
                );
                if token == Some("admin") {
                    successful += 1;
                } else {
                    assert_eq!(response.header("cache-control").unwrap(), "no-store");
                    if status == 401 {
                        assert_eq!(response.header("www-authenticate").unwrap(), "Bearer");
                    } else {
                        assert!(response.header("www-authenticate").is_none());
                    }
                    let body: serde_json::Value = response.json().unwrap();
                    assert_eq!(body["kind"], "iam");
                    assert!(body.get("payload").is_none());
                    assert!(!body.to_string().contains("private-"));
                }
                assert_eq!(calls.load(Ordering::SeqCst), successful);
            }
        }
    })
    .await
    .unwrap();
}
