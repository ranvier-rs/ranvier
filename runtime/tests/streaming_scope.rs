#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, BusAccessError, BusAccessPolicy, BusTypeRef, CancellationReason, CancellationToken,
    streaming::StreamingTransition,
};
use ranvier_runtime::Axon;
use std::{pin::Pin, sync::Arc};
#[derive(Clone)]
struct Init {
    mode: u8,
    entered: Arc<tokio::sync::Notify>,
}
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
        assert!(
            bus.get::<String>().is_ok(),
            "framework must suspend caller guard"
        );
        bus.set_cancellation_token(CancellationToken::new());
        bus.set_access_policy("initializer", Some(BusAccessPolicy::allow_only(vec![])));
        self.entered.notify_one();
        match self.mode {
            0 => Ok(Box::pin(futures_util::stream::iter(vec!["ok".into()]))),
            1 => Err("synthetic-init-error".into()),
            2 => panic!("synthetic-init-panic"),
            _ => std::future::pending().await,
        }
    }
}
#[tokio::test]
async fn stream_initialization_restores_caller_control_after_return_unwind_or_drop() {
    use futures_util::FutureExt;
    for cancellable in [false, true] {
        for present in [false, true] {
            for mode in 0..4 {
                let entered = Arc::new(tokio::sync::Notify::new());
                let circuit = Axon::<(), (), String>::start("scope").then_stream(Init {
                    mode,
                    entered: entered.clone(),
                });
                let mut bus = Bus::new();
                bus.insert("private".to_string());
                let caller = CancellationToken::new();
                if present {
                    bus.set_cancellation_token(caller.clone());
                }
                bus.set_access_policy(
                    "caller",
                    Some(BusAccessPolicy::deny_only(vec![BusTypeRef::of::<String>()])),
                );
                {
                    let execution = async {
                        if cancellable {
                            circuit
                                .execute_cancellable((), &(), &mut bus, CancellationToken::new())
                                .await
                                .map_err(|_| ())
                        } else {
                            circuit.execute((), &(), &mut bus).await.map_err(|_| ())
                        }
                    };
                    let caught = std::panic::AssertUnwindSafe(execution).catch_unwind();
                    tokio::pin!(caught);
                    if mode == 3 {
                        tokio::select! {
                            _ = entered.notified() => {},
                            _ = &mut caught => panic!("pending initializer unexpectedly ended"),
                            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("initializer did not enter"),
                        }
                    } else {
                        let result = caught.await;
                        assert_eq!(result.is_err(), mode == 2);
                        if let Ok(initialized) = result {
                            assert_eq!(initialized.is_err(), mode == 1);
                        }
                    }
                }
                assert!(
                    matches!(bus.get::<String>(), Err(BusAccessError::Unauthorized {transition, ..}) if transition == "caller")
                );
                assert_eq!(bus.cancellation_token().is_some(), present);
                caller.cancel(CancellationReason::Explicit);
                if present {
                    assert!(bus.cancellation_token().unwrap().is_cancelled());
                }
            }
        }
    }
}

#[tokio::test]
async fn init_cancellation_restores_caller_token_and_guard_after_mutation() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let circuit = Axon::<(), (), String>::start("cancel-scope").then_stream(Init {
        mode: 3,
        entered: entered.clone(),
    });
    let mut bus = Bus::new();
    bus.insert("private".to_string());
    let caller = CancellationToken::new();
    bus.set_cancellation_token(caller.clone());
    bus.set_access_policy(
        "caller",
        Some(BusAccessPolicy::deny_only(vec![BusTypeRef::of::<String>()])),
    );
    let token = CancellationToken::new();
    let cancel = async {
        entered.notified().await;
        token.cancel(CancellationReason::Explicit);
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(
            circuit.execute_cancellable((), &(), &mut bus, token.clone()),
            cancel
        )
    })
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(ranvier_runtime::streaming_axon::CancellableStreamingError::Cancelled(_))
    ));
    assert!(!caller.is_cancelled());
    assert!(!bus.cancellation_token().unwrap().is_cancelled());
    caller.cancel(CancellationReason::OperatorShutdown);
    assert!(bus.cancellation_token().unwrap().is_cancelled());
    assert!(
        matches!(bus.get::<String>(), Err(BusAccessError::Unauthorized {transition, ..}) if transition == "caller")
    );
}

#[tokio::test]
async fn initialized_stream_keeps_execution_token_after_caller_bus_restoration() {
    use futures_util::StreamExt;
    let circuit = Axon::<(), (), String>::start("returned-scope").then_stream(Init {
        mode: 0,
        entered: Arc::new(tokio::sync::Notify::new()),
    });
    let mut bus = Bus::new();
    bus.insert("private".to_string());
    let caller = CancellationToken::new();
    bus.set_cancellation_token(caller.clone());
    let execution_token = CancellationToken::new();
    let mut stream = circuit
        .execute_cancellable((), &(), &mut bus, execution_token.clone())
        .await
        .unwrap();
    caller.cancel(CancellationReason::Explicit);
    assert!(!execution_token.is_cancelled());
    assert_eq!(stream.next().await, Some("ok".into()));
    let mut stream = circuit
        .execute_cancellable((), &(), &mut bus, execution_token.clone())
        .await
        .unwrap();
    execution_token.cancel(CancellationReason::ClientDisconnected);
    assert!(stream.next().await.is_none());
    assert_eq!(
        bus.cancellation_token().unwrap().context().unwrap().reason,
        CancellationReason::Explicit
    );
}
