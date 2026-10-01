#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, Outcome, Transition,
    iam::{IamError, IamIdentity, IamPolicy, IamToken, IamVerifier},
    streaming::StreamingTransition,
};
use ranvier_http::prelude::{Ranvier, TestApp, TestRequest};
use ranvier_runtime::Axon;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Verifier(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl IamVerifier for Verifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        match token {
            "valid" => Ok(IamIdentity::new("verified").with_role("admin")),
            "user" => Ok(IamIdentity::new("verified")),
            "panic" => panic!("synthetic stream IAM panic"),
            _ => Err(IamError::InvalidToken("private-detail".into())),
        }
    }
}
#[derive(Clone)]
struct Prefix(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Prefix {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        assert_eq!(bus.read::<IamIdentity>().unwrap().subject, "verified");
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Init(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl StreamingTransition<()> for Init {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter(vec![
            bus.read::<IamIdentity>()
                .map(|v| v.subject.clone())
                .unwrap_or_default(),
        ])))
    }
}

#[tokio::test]
async fn configured_stream_iam_rejects_before_sse_headers_and_effects() {
    let verified = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let initialized = Arc::new(AtomicUsize::new(0));
    let circuit = Axon::<(), (), String>::start("http-stream-iam")
        .with_iam(
            IamPolicy::RequireRole("admin".into()),
            Verifier(verified.clone()),
        )
        .then(Prefix(effects.clone()))
        .then_stream(Init(initialized.clone()));
    let app = TestApp::new(
        Ranvier::http::<()>()
            .bus_injector(|parts, bus| {
                bus.insert(IamIdentity::new("stale-admin").with_role("admin"));
                if let Some(token) = parts
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                {
                    bus.insert(IamToken(token.into()));
                }
            })
            .post_sse("/stream", circuit),
        (),
    );
    for (token, status) in [
        (None, 401),
        (Some("invalid"), 401),
        (Some("user"), 403),
        (Some("panic"), 500),
        (Some("valid"), 200),
    ] {
        let mut request = TestRequest::post("/stream");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), app.send(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        if status == 200 {
            assert!(response.text().unwrap().contains("verified"));
        } else {
            assert_eq!(response.header("cache-control").unwrap(), "no-store");
            assert_eq!(
                response
                    .header("www-authenticate")
                    .map(|v| v.to_str().unwrap()),
                if status == 401 { Some("Bearer") } else { None }
            );
            assert!(!response.text().unwrap().contains("private-detail"));
            assert!(!response.text().unwrap().contains("data:"));
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            assert_eq!(initialized.load(Ordering::SeqCst), 0);
        }
    }
    assert_eq!(verified.load(Ordering::SeqCst), 4);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
}
