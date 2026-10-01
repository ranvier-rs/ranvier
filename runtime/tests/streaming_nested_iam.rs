#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, Outcome, Transition,
    iam::{IamIdentity, IamToken},
    streaming::StreamingTransition,
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::{pin::Pin, sync::Arc};
#[derive(Clone)]
struct Mutate {
    mode: u8,
    entered: Arc<tokio::sync::Notify>,
    verified: bool,
}
#[async_trait::async_trait]
impl StreamingTransition<()> for Mutate {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        if self.verified {
            let identity = bus.read::<IamIdentity>().unwrap();
            assert_eq!(identity.subject, "verified-child");
            assert!(identity.roles.iter().any(|r| r == "child-role"));
        }
        bus.remove::<IamIdentity>();
        bus.remove::<IamToken>();
        bus.insert_shared(IamIdentity::new("child"));
        bus.insert_shared(IamToken("child-token".into()));
        self.entered.notify_one();
        match self.mode {
            0 => Ok(Box::pin(futures_util::stream::iter(vec![
                "child-item".into(),
            ]))),
            1 => Err("synthetic child init error".into()),
            2 => panic!("synthetic child init panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Parent {
    present: bool,
    shared: bool,
    mode: u8,
    ordinary_child: bool,
    required: bool,
}
#[async_trait::async_trait]
impl Transition<(), ()> for Parent {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        use futures_util::FutureExt;
        if self.present {
            let identity = IamIdentity::new("parent").with_role("parent-role");
            let token = IamToken("parent-token".into());
            if self.shared {
                bus.insert_shared(identity);
                bus.insert_shared(token);
            } else {
                bus.insert(identity);
                bus.insert(token);
            }
        }
        let identity_ptr = bus.read::<IamIdentity>().map(|v| v as *const _ as usize);
        let token_ptr = bus.read::<IamToken>().map(|v| v as *const _ as usize);
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut base = Axon::<(), (), String>::start("nested-child");
        let verified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        if self.required {
            base = base.with_iam(
                ranvier_core::iam::IamPolicy::RequireIdentity,
                Verify(verified.clone()),
            );
        }
        {
            let execution = async {
                let mutate = Mutate {
                    mode: self.mode,
                    entered: entered.clone(),
                    verified: self.required,
                };
                if self.ordinary_child {
                    let report = base.then(mutate).execute((), &(), bus).await;
                    match report {
                        ExecutionReport::Unpersisted {
                            result: Outcome::Next(()),
                            ..
                        } => Ok(()),
                        ExecutionReport::Unpersisted {
                            result: Outcome::Fault(_),
                            ..
                        } => Err(()),
                        _ => panic!("unexpected child report"),
                    }
                } else {
                    base.then_stream(mutate)
                        .execute((), &(), bus)
                        .await
                        .map(|_| ())
                        .map_err(|_| ())
                }
            };
            let future = std::panic::AssertUnwindSafe(execution).catch_unwind();
            tokio::pin!(future);
            if self.mode == 3 {
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending child unexpectedly ended"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("child did not enter"),
                }
            } else {
                let result = future.await;
                assert_eq!(result.is_err(), self.mode == 2 && !self.ordinary_child);
                if let Ok(result) = result {
                    assert_eq!(
                        result.is_err(),
                        self.mode == 1 || (self.mode == 2 && self.ordinary_child)
                    );
                }
            }
        }
        assert_eq!(
            verified.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(self.required)
        );
        assert_eq!(
            bus.read::<IamIdentity>().map(|v| v as *const _ as usize),
            identity_ptr,
            "parent identity storage changed"
        );
        assert_eq!(
            bus.read::<IamToken>().map(|v| v as *const _ as usize),
            token_ptr,
            "parent token storage changed"
        );
        assert_eq!(
            bus.read::<IamIdentity>().map(|v| v.subject.as_str()),
            if self.present { Some("parent") } else { None }
        );
        assert_eq!(
            bus.read::<IamToken>().map(|v| v.0.as_str()),
            if self.present {
                Some("parent-token")
            } else {
                None
            }
        );
        assert_eq!(
            bus.fork_for_parallel().read::<IamIdentity>().is_some(),
            self.present && self.shared
        );
        assert_eq!(
            bus.fork_for_parallel().read::<IamToken>().is_some(),
            self.present && self.shared
        );
        Outcome::Next(())
    }
}

struct Verify(Arc<std::sync::atomic::AtomicUsize>);
#[async_trait::async_trait]
impl ranvier_core::iam::IamVerifier for Verify {
    async fn verify(&self, token: &str) -> Result<IamIdentity, ranvier_core::iam::IamError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(token, "parent-token");
        Ok(IamIdentity::new("verified-child").with_role("child-role"))
    }
}
#[async_trait::async_trait]
impl Transition<(), ()> for Mutate {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        if self.verified {
            let identity = bus.read::<IamIdentity>().unwrap();
            assert_eq!(identity.subject, "verified-child");
            assert!(identity.roles.iter().any(|r| r == "child-role"));
        }
        bus.remove::<IamIdentity>();
        bus.remove::<IamToken>();
        bus.insert_shared(IamIdentity::new("child"));
        bus.insert_shared(IamToken("child-token".into()));
        self.entered.notify_one();
        match self.mode {
            0 => Outcome::Next(()),
            1 => Outcome::Fault("synthetic child fault".into()),
            2 => panic!("synthetic child transition panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Finish;
#[async_trait::async_trait]
impl StreamingTransition<()> for Finish {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        Ok(Box::pin(futures_util::stream::iter(vec![
            "finished".into(),
        ])))
    }
}
async fn check_parent(stream_parent: bool, parent: Parent) {
    let base = Axon::<(), (), String>::start("parent").then(parent);
    if stream_parent {
        use futures_util::StreamExt;
        let mut stream = base
            .then_stream(Finish)
            .execute((), &(), &mut Bus::new())
            .await
            .unwrap();
        assert_eq!(stream.next().await, Some("finished".into()));
    } else {
        let report = base.execute((), &(), &mut Bus::new()).await;
        assert!(matches!(
            report,
            ExecutionReport::Unpersisted {
                result: Outcome::Next(()),
                ..
            }
        ));
    }
}
#[tokio::test]
async fn mixed_parents_restore_exact_iam_after_child_return_unwind_and_drop() {
    for stream_parent in [false, true] {
        for ordinary_child in [false, true] {
            for present in [false, true] {
                for shared in [false, true] {
                    for mode in 0..4 {
                        check_parent(
                            stream_parent,
                            Parent {
                                present,
                                shared,
                                mode,
                                ordinary_child,
                                required: false,
                            },
                        )
                        .await;
                    }
                }
            }
        }
    }
}
#[tokio::test]
async fn required_child_verification_restores_parent_iam_after_return_unwind_and_drop() {
    for stream_parent in [false, true] {
        for ordinary_child in [false, true] {
            for shared in [false, true] {
                for mode in 0..4 {
                    check_parent(
                        stream_parent,
                        Parent {
                            present: true,
                            shared,
                            mode,
                            ordinary_child,
                            required: true,
                        },
                    )
                    .await;
                }
            }
        }
    }
}

struct Reject;
#[async_trait::async_trait]
impl ranvier_core::iam::IamVerifier for Reject {
    async fn verify(&self, _: &str) -> Result<IamIdentity, ranvier_core::iam::IamError> {
        Err(ranvier_core::iam::IamError::InvalidToken(
            "synthetic rejected token".into(),
        ))
    }
}
#[tokio::test]
async fn root_boundary_marker_does_not_leak_after_error_unwind_or_future_drop() {
    use futures_util::FutureExt;
    let mut bus = Bus::new();
    for mode in [1, 2, 3] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let circuit = Axon::<(), (), String>::start("root-marker").then_stream(Mutate {
            mode,
            entered: entered.clone(),
            verified: false,
        });
        {
            let future =
                std::panic::AssertUnwindSafe(circuit.execute((), &(), &mut bus)).catch_unwind();
            tokio::pin!(future);
            if mode == 3 {
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending root unexpectedly ended"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("root did not enter"),
                }
            } else {
                let _ = future.await;
            }
        }
        let required = Axon::<(), (), String>::start("root-rejected")
            .with_iam(ranvier_core::iam::IamPolicy::RequireIdentity, Reject)
            .then_stream(Finish);
        let result = required.execute((), &(), &mut bus).await;
        assert!(matches!(
            result,
            Err(
                ranvier_runtime::streaming_axon::StreamingAxonError::IamRejected(
                    "iam.verification_failed"
                )
            )
        ));
        assert!(
            bus.read::<IamIdentity>().is_none(),
            "new root must clear prior identity, not restore it as a nested parent"
        );
    }
}
