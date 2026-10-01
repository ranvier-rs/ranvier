#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, Outcome, Transition,
    iam::{IamError, IamIdentity, IamPolicy, IamToken, IamVerifier},
    streaming::StreamingTransition,
};
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
async fn configured_iam_survives_then_stream_conversion() {
    for token in [
        None,
        Some("invalid"),
        Some("user"),
        Some("panic"),
        Some("valid"),
    ] {
        let verified = Arc::new(AtomicUsize::new(0));
        let prefixes = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let circuit = Axon::<(), (), String>::start("configured-stream-iam")
            .with_iam(
                IamPolicy::RequireRole("admin".into()),
                Verifier(verified.clone()),
            )
            .then(Prefix(prefixes.clone()))
            .then_stream(Init(initialized.clone()));
        let mut bus = Bus::new();
        bus.insert(IamIdentity::new("stale-admin").with_role("admin"));
        if let Some(token) = token {
            bus.insert(IamToken(token.into()));
        }
        let result = circuit.execute((), &(), &mut bus).await;
        if token == Some("valid") {
            use futures_util::StreamExt;
            let mut stream = result.unwrap();
            assert_eq!(stream.next().await, Some("verified".into()));
            assert_eq!(prefixes.load(Ordering::SeqCst), 1);
            assert_eq!(initialized.load(Ordering::SeqCst), 1);
        } else {
            assert!(result.is_err(), "invalid stream must reject");
            assert_eq!(prefixes.load(Ordering::SeqCst), 0);
            assert_eq!(initialized.load(Ordering::SeqCst), 0);
            assert!(bus.read::<IamIdentity>().is_none());
        }
        assert_eq!(
            verified.load(Ordering::SeqCst),
            usize::from(token.is_some())
        );
    }
}
