use ranvier_core::{
    Bus, Outcome, Transition,
    saga::{SagaPolicy, SagaStack},
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone)]
struct Step(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Step {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Fault(u8, Arc<tokio::sync::Notify>);
#[async_trait::async_trait]
impl Transition<(), ()> for Fault {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.1.notify_one();
        match self.0 {
            0 => Outcome::Next(()),
            1 => Outcome::Fault("child-fault".into()),
            2 => panic!("child-saga-panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Comp(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Comp {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}

#[tokio::test]
async fn readonly_saga_ingress_rejects_before_effect_and_route_mapping() {
    use ranvier_http::prelude::{Ranvier, TestApp, TestRequest};
    let held = Arc::new(std::sync::Mutex::new(Vec::<Bus>::new()));
    let injected = held.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let compensated = Arc::new(AtomicUsize::new(0));
    let make = || {
        Axon::<(), (), String>::start("readonly-http")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Step(effects.clone()), Comp(compensated.clone()))
            .then(Fault(1, Arc::new(tokio::sync::Notify::new())))
    };
    let app = TestApp::new(
        Ranvier::http::<()>()
            .bus_injector(move |_, bus| {
                let mut stack = SagaStack::new();
                stack.push(
                    "private-parent-task".into(),
                    "private-label".into(),
                    vec![1],
                );
                bus.insert_shared(stack);
                injected.lock().unwrap().push(bus.fork_for_parallel());
            })
            .get("/plain", make())
            .get_json_out("/json", make()),
        (),
    );
    for path in ["/plain", "/json"] {
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            app.send(TestRequest::get(path)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(response.header("cache-control").unwrap(), "no-store");
        let body: serde_json::Value = response.json().unwrap();
        assert_eq!(body["event_type"], "saga.context_readonly");
        assert!(!body.to_string().contains("private-parent-task"));
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(compensated.load(Ordering::SeqCst), 0);
    for bus in held.lock().unwrap().iter() {
        assert_eq!(bus.read::<SagaStack>().unwrap().tasks.len(), 1);
    }
}
