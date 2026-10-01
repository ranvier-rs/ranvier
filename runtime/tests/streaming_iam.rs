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
    for cancellable in [false, true] {
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
            let result = if cancellable {
                circuit
                    .execute_cancellable((), &(), &mut bus, ranvier_core::CancellationToken::new())
                    .await
                    .map_err(|e| match e {
                        ranvier_runtime::streaming_axon::CancellableStreamingError::Stream(e) => e,
                        _ => panic!("unexpected cancellation"),
                    })
            } else {
                circuit.execute((), &(), &mut bus).await
            };
            if token == Some("valid") {
                use futures_util::StreamExt;
                let mut stream = result.unwrap();
                assert_eq!(stream.next().await, Some("verified".into()));
                assert_eq!(prefixes.load(Ordering::SeqCst), 1);
                assert_eq!(initialized.load(Ordering::SeqCst), 1);
            } else {
                let expected = match token {
                    None => "iam.missing_token",
                    Some("invalid") => "iam.verification_failed",
                    Some("user") => "iam.policy_denied",
                    Some("panic") => "iam.verifier_panicked",
                    _ => unreachable!(),
                };
                assert!(
                    matches!(result, Err(ranvier_runtime::streaming_axon::StreamingAxonError::IamRejected(event)) if event == expected)
                );
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
}

#[tokio::test]
async fn inherited_identity_fails_closed_and_collection_preserves_classifier() {
    let verified = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let initialized = Arc::new(AtomicUsize::new(0));
    let make = || {
        Axon::<(), (), String>::start("scope-stream")
            .with_iam(IamPolicy::RequireIdentity, Verifier(verified.clone()))
            .then(Prefix(effects.clone()))
            .then_stream(Init(initialized.clone()))
    };
    let mut parent = Bus::new();
    parent.insert_shared(IamIdentity::new("parent"));
    let mut child = parent.fork_for_parallel();
    child.insert(IamToken("valid".into()));
    let result = make().execute((), &(), &mut child).await;
    assert!(matches!(
        result,
        Err(
            ranvier_runtime::streaming_axon::StreamingAxonError::IamRejected(
                "iam.context_clear_failed"
            )
        )
    ));
    assert_eq!(parent.read::<IamIdentity>().unwrap().subject, "parent");
    assert_eq!(verified.load(Ordering::SeqCst), 0);
    let mut bus = Bus::new();
    let report = make()
        .map_items(|item| item)
        .collect_into_vec()
        .execute((), &(), &mut bus)
        .await;
    assert!(
        matches!(report, ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Emit(ref event, None), .. } if event == "iam.missing_token")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn optional_iam_delegates_and_precancelled_required_stream_has_no_effects() {
    for configured in [false, true] {
        let verified = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let mut prefix = Axon::<(), (), String>::start("optional-stream");
        if configured {
            prefix = prefix.with_iam(IamPolicy::None, Verifier(verified.clone()));
        }
        let circuit = prefix
            .then(Prefix(effects.clone()))
            .then_stream(Init(initialized.clone()));
        let mut bus = Bus::new();
        bus.insert(IamIdentity::new("verified"));
        bus.insert(IamToken("panic".into()));
        let mut stream = circuit.execute((), &(), &mut bus).await.unwrap();
        use futures_util::StreamExt;
        assert_eq!(stream.next().await, Some("verified".into()));
        assert_eq!(verified.load(Ordering::SeqCst), 0);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(initialized.load(Ordering::SeqCst), 1);
    }
    let verified = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let initialized = Arc::new(AtomicUsize::new(0));
    let circuit = Axon::<(), (), String>::start("precancelled-stream")
        .with_iam(IamPolicy::RequireIdentity, Verifier(verified.clone()))
        .then(Prefix(effects.clone()))
        .then_stream(Init(initialized.clone()));
    let token = ranvier_core::CancellationToken::new();
    token.cancel(ranvier_core::CancellationReason::Explicit);
    let result = circuit
        .execute_cancellable((), &(), &mut Bus::new(), token)
        .await;
    assert!(matches!(
        result,
        Err(ranvier_runtime::streaming_axon::CancellableStreamingError::Cancelled(_))
    ));
    assert_eq!(verified.load(Ordering::SeqCst), 0);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
}
