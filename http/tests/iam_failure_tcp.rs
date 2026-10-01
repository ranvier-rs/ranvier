use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ranvier_core::iam::{IamError, IamIdentity, IamPolicy, IamToken, IamVerifier};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::service::RanvierService;
use ranvier_runtime::Axon;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Verifier;
#[async_trait::async_trait]
impl IamVerifier for Verifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        match token {
            "private-admin-token" => Ok(IamIdentity::new("private-subject").with_role("admin")),
            "private-user-token" => Ok(IamIdentity::new("private-subject").with_role("user")),
            "private-panic-token" => panic!("synthetic TCP verifier panic"),
            _ => Err(IamError::InvalidToken("private-verifier-error".into())),
        }
    }
}
#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), serde_json::Value> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<serde_json::Value, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(serde_json::json!({"ok":true}))
    }
}
struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn iam_failures_over_tcp_reject_without_effect_or_private_payload() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<(), (), String>::start("iam-tcp")
            .with_iam(IamPolicy::RequireRole("admin".into()), Verifier)
            .then(Effect(effects.clone()));
        let service = RanvierService::new(
            axon,
            |request: http::Request<hyper::body::Incoming>, bus: &mut Bus| {
                if let Some(token) = request
                    .headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| h.strip_prefix("Bearer "))
                {
                    bus.insert(IamToken(token.into()));
                }
            },
            (),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut server = AbortTask(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        }));
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        let mut driver = AbortTask(tokio::spawn(connection));
        let cases = [
            (Some("private-admin-token"), 200, None),
            (None, 401, Some("iam.missing_token")),
            (
                Some("private-invalid-token"),
                401,
                Some("iam.verification_failed"),
            ),
            (Some("private-user-token"), 403, Some("iam.policy_denied")),
            (
                Some("private-panic-token"),
                500,
                Some("iam.verifier_panicked"),
            ),
            (Some("private-admin-token"), 200, None),
        ];
        let mut expected_effects = 0;
        for (index, (token, status, label)) in cases.into_iter().enumerate() {
            let mut request = http::Request::builder()
                .uri("/secure")
                .header("host", "fixture");
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            if index == cases.len() - 1 {
                request = request.header("connection", "close");
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::new())).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status, "case {index}");
            assert_eq!(response.headers()["ranvier-durability"], "unpersisted");
            if status == 401 {
                assert_eq!(response.headers()["www-authenticate"], "Bearer");
            } else {
                assert!(response.headers().get("www-authenticate").is_none());
            }
            if label.is_some() {
                assert_eq!(response.headers()["cache-control"], "no-store");
            }
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = std::str::from_utf8(&body).unwrap();
            for marker in [
                "private-admin-token",
                "private-invalid-token",
                "private-user-token",
                "private-panic-token",
                "private-subject",
                "private-verifier-error",
            ] {
                assert!(
                    !text.contains(marker),
                    "private response marker in case {index}"
                );
            }
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if let Some(label) = label {
                assert_eq!(body["kind"], "iam");
                assert_eq!(body["event_type"], label);
                assert!(body.get("payload").is_none());
            } else {
                expected_effects += 1;
                assert_eq!(body, serde_json::json!({"ok":true}));
            }
            assert_eq!(effects.load(Ordering::SeqCst), expected_effects);
        }
        drop(sender);
        (&mut driver.0).await.unwrap().unwrap();
        (&mut server.0).await.unwrap();
    })
    .await
    .unwrap();
}
