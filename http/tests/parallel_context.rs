use http::StatusCode;
use ranvier_core::prelude::{Bus, IamIdentity, Outcome, Transition};
use ranvier_guard::prelude::{AuthGuard, AuthorizationHeader, RequestId, RequestIdGuard};
use ranvier_http::prelude::{QueryParams, Ranvier, TestApp, TestRequest};
use ranvier_runtime::prelude::{Axon, ParallelBusPolicy, ParallelStrategy};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug)]
struct BranchLocalWrite;

#[derive(Clone)]
struct AdapterContextProbe;

#[async_trait::async_trait]
impl Transition<(), ()> for AdapterContextProbe {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<(), Self::Error> {
        let Some(identity) = bus.read::<IamIdentity>() else {
            return Outcome::fault("missing inherited identity".to_string());
        };
        if identity.subject != "bearer-authenticated" {
            return Outcome::fault("unexpected inherited identity".to_string());
        }

        let Some(request_id) = bus.read::<RequestId>() else {
            return Outcome::fault("missing inherited request id".to_string());
        };
        if request_id.0 != "parallel-context-request" {
            return Outcome::fault("unexpected inherited request id".to_string());
        }

        let Some(query) = bus.read::<QueryParams>() else {
            return Outcome::fault("missing inherited query parameters".to_string());
        };
        if query.get("tenant") != Some("tenant-a") {
            return Outcome::fault("unexpected inherited query parameters".to_string());
        }

        if bus.read::<AuthorizationHeader>().is_some() {
            return Outcome::fault("raw authorization header crossed branch boundary".to_string());
        }

        bus.insert(BranchLocalWrite);
        Outcome::next(())
    }
}

#[derive(Clone)]
struct FanInProbe;

#[derive(Debug, Deserialize, Serialize)]
struct ContextProof {
    identity: String,
    request_id: String,
    tenant: String,
    branch_write_discarded: bool,
}

#[async_trait::async_trait]
impl Transition<(), ContextProof> for FanInProbe {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ContextProof, Self::Error> {
        let identity = bus
            .read::<IamIdentity>()
            .map(|identity| identity.subject.clone())
            .unwrap_or_default();
        let request_id = bus
            .read::<RequestId>()
            .map(|request_id| request_id.0.clone())
            .unwrap_or_default();
        let tenant = bus
            .read::<QueryParams>()
            .and_then(|query| query.get("tenant"))
            .unwrap_or_default()
            .to_string();

        Outcome::next(ContextProof {
            identity,
            request_id,
            tenant,
            branch_write_discarded: bus.read::<BranchLocalWrite>().is_none(),
        })
    }
}

#[tokio::test]
async fn http_context_inherits_only_explicitly_shared_values_across_parallel_branches() {
    let circuit = Axon::<(), (), String, ()>::new("ParallelContext")
        .parallel_with_bus_policy(
            vec![Arc::new(AdapterContextProbe)],
            ParallelStrategy::AllMustSucceed,
            ParallelBusPolicy::InheritShared,
        )
        .then(FanInProbe);
    let ingress = Ranvier::http::<()>()
        .guard(RequestIdGuard::<()>::new())
        .guard(AuthGuard::<()>::bearer(vec!["valid-token".to_string()]))
        .get_json_out("/context", circuit);
    let app = TestApp::new(ingress, ());

    let response = app
        .send(
            TestRequest::get("/context?tenant=tenant-a")
                .header("authorization", "Bearer valid-token")
                .header("x-request-id", "parallel-context-request"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .header("x-request-id")
            .expect("request id response header")
            .to_str()
            .expect("request id should be utf8"),
        "parallel-context-request"
    );

    let proof: ContextProof = response.json().expect("response should be json");
    assert_eq!(proof.identity, "bearer-authenticated");
    assert_eq!(proof.request_id, "parallel-context-request");
    assert_eq!(proof.tenant, "tenant-a");
    assert!(proof.branch_write_discarded);
}
