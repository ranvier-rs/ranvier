//! # RanvierService - Hyper Service Adapter
//!
//! Adapts Ranvier Axon execution to Hyper's Service trait.
//! This allows Ranvier circuits to be used with any Hyper-compatible infrastructure.
//!
//! ## Design (Discussion 190)
//!
//! > "ranvier-http is an adapter that converts Ranvier Axon into hyper::service::Service"

use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use ranvier_core::prelude::*;
use ranvier_runtime::{Axon, ExecutionReport};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Maps an Axon execution report plus execution [`Bus`] into an HTTP response.
pub type ResponseMapper<Out, E> =
    Arc<dyn Fn(ExecutionReport<Outcome<Out, E>>, &Bus) -> Response<Full<Bytes>> + Send + Sync>;

/// The foundational logic engine service.
/// Adapts HTTP requests to Axon executions.
#[derive(Clone)]
pub struct RanvierService<In, Out, E, F, Res = ()> {
    axon: Axon<In, Out, E, Res>,
    /// Converts a Request into the Axon's input state and potentially populates the Bus.
    converter: F,
    /// Resources used by the axon
    resources: Arc<Res>,
    /// Converts the Axon's Outcome into an HTTP response.
    response_mapper: ResponseMapper<Out, E>,
}

impl<In, Out, E, F, Res> RanvierService<In, Out, E, F, Res>
where
    Out: serde::Serialize + 'static,
    E: serde::Serialize + std::fmt::Debug + 'static,
{
    pub fn new(axon: Axon<In, Out, E, Res>, converter: F, resources: Res) -> Self {
        Self {
            axon,
            converter,
            resources: Arc::new(resources),
            response_mapper: Arc::new(default_response_mapper::<Out, E>),
        }
    }

    /// Override the default `ExecutionReport -> HTTP` mapping.
    ///
    /// The low-level service keeps ingress conversion and egress conversion
    /// explicit: the converter builds the Axon input and Bus, while this mapper
    /// decides how each Outcome variant is represented at the protocol boundary.
    pub fn with_response_mapper<M>(mut self, mapper: M) -> Self
    where
        M: Fn(ExecutionReport<Outcome<Out, E>>, &Bus) -> Response<Full<Bytes>>
            + Send
            + Sync
            + 'static,
    {
        self.response_mapper = Arc::new(mapper);
        self
    }
}

impl<B, In, Out, E, F, Res> hyper::service::Service<Request<B>>
    for RanvierService<In, Out, E, F, Res>
where
    B: Send + 'static,
    In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    F: Fn(Request<B>, &mut Bus) -> In + Clone + Send + Sync + 'static,
    Res: Send + Sync + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<B>) -> Self::Future {
        let axon = self.axon.clone();
        let converter = self.converter.clone();
        let resources = self.resources.clone();
        let response_mapper = self.response_mapper.clone();

        Box::pin(async move {
            let mut bus = Bus::new();

            // 1. Ingress Adapter: Request -> In + Bus
            let input = converter(req, &mut bus);

            // 2. Run Axon
            let result = axon.execute(input, &resources, &mut bus).await;

            // 3. Egress Adapter: Outcome -> Response
            let response = response_mapper(result, &bus);
            Ok(response)
        })
    }
}

fn default_response_mapper<Out, E>(
    report: ExecutionReport<Outcome<Out, E>>,
    bus: &Bus,
) -> Response<Full<Bytes>>
where
    Out: serde::Serialize,
    E: serde::Serialize + std::fmt::Debug,
{
    if let Some((status, payload)) = crate::response::durability_rejection(&report) {
        return json_value_response(status, payload);
    }
    let completion = match &report {
        ExecutionReport::Recorded {
            completion: ranvier_runtime::CompletionConfirmation::Confirmed,
            ..
        } => Some("confirmed"),
        ExecutionReport::Recorded { .. } => Some("not_requested"),
        _ => None,
    };
    let (outcome, durability) = match report {
        ExecutionReport::Unpersisted { result, .. } => (result, "unpersisted"),
        ExecutionReport::NotRequested { result, .. } => (result, "not_requested"),
        ExecutionReport::Recorded { result, .. } => (result, "recorded"),
        _ => {
            return json_value_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error":"Execution durability requires reconciliation"}),
            );
        }
    };
    let mut response = domain_response_mapper(outcome, bus);
    response.headers_mut().insert(
        "ranvier-durability",
        http::HeaderValue::from_static(durability),
    );
    if let Some(completion) = completion {
        response.headers_mut().insert(
            "ranvier-completion",
            http::HeaderValue::from_static(completion),
        );
    }
    response
}

// Exact framework IAM failure labels are reserved; ordinary application Emit
// events retain their existing representation. Discard untrusted failure payloads.
fn iam_failure_response(event: &str) -> Option<Response<Full<Bytes>>> {
    let (status, message) = match event {
        "iam.missing_token" => (StatusCode::UNAUTHORIZED, "Authentication required"),
        "iam.verification_failed" => (StatusCode::UNAUTHORIZED, "Authentication failed"),
        "iam.policy_denied" => (StatusCode::FORBIDDEN, "Access denied"),
        "iam.context_clear_failed" | "iam.context_write_denied" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication context unavailable",
        ),
        "iam.verifier_panicked" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication verifier failed",
        ),
        _ => return None,
    };
    let mut response = json_value_response(
        status,
        serde_json::json!({
            "kind": "iam", "event_type": event, "error": message
        }),
    );
    response.headers_mut().insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            http::header::WWW_AUTHENTICATE,
            http::HeaderValue::from_static("Bearer"),
        );
    }
    Some(response)
}

fn domain_response_mapper<Out, E>(outcome: Outcome<Out, E>, _bus: &Bus) -> Response<Full<Bytes>>
where
    Out: serde::Serialize,
    E: serde::Serialize + std::fmt::Debug,
{
    match outcome {
        Outcome::Next(output) => json_response(StatusCode::OK, &output),
        Outcome::Fault(error) => {
            let error_value = match serde_json::to_value(&error) {
                Ok(value) => value,
                Err(_) => serde_json::json!({"message": "Fault response could not be encoded"}),
            };
            json_value_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "kind": "fault",
                    "error": error_value
                }),
            )
        }
        Outcome::Emit(event_type, payload) => {
            if let Some(response) = iam_failure_response(&event_type) {
                response
            } else {
                json_value_response(
                    StatusCode::ACCEPTED,
                    serde_json::json!({
                        "kind": "emit", "event_type": event_type, "payload": payload
                    }),
                )
            }
        }
        Outcome::Branch(branch_id, payload) => json_value_response(
            StatusCode::CONFLICT,
            serde_json::json!({
                "kind": "branch",
                "branch_id": branch_id,
                "payload": payload
            }),
        ),
        Outcome::Jump(node_id, payload) => json_value_response(
            StatusCode::CONFLICT,
            serde_json::json!({
                "kind": "jump",
                "node_id": node_id,
                "payload": payload
            }),
        ),
    }
}

fn json_response<T>(status: StatusCode, value: &T) -> Response<Full<Bytes>>
where
    T: serde::Serialize,
{
    match serde_json::to_vec(value) {
        Ok(bytes) => response_with_body(
            Response::builder()
                .status(status)
                .header(CONTENT_TYPE, "application/json"),
            Bytes::from(bytes),
        ),
        Err(_) => json_value_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "kind": "serialization_error",
                "error": "JSON response could not be encoded"
            }),
        ),
    }
}

fn json_value_response(status: StatusCode, value: serde_json::Value) -> Response<Full<Bytes>> {
    json_response(status, &value)
}

fn response_with_body(builder: http::response::Builder, body: Bytes) -> Response<Full<Bytes>> {
    match builder.body(Full::new(body)) {
        Ok(response) => response,
        Err(_) => {
            let fallback = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Full::new(Bytes::from_static(
                    b"HTTP response could not be constructed",
                )));
            match fallback {
                Ok(response) => response,
                Err(_) => Response::new(Full::new(Bytes::from_static(b"Internal error"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::service::Service;
    use ranvier_core::Transition;

    #[tokio::test]
    async fn default_mapper_redacts_serializer_and_debug_failures() {
        #[derive(Debug)]
        struct Broken;
        impl serde::Serialize for Broken {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("private-serializer-probe"))
            }
        }
        struct PrivateFault;
        impl std::fmt::Debug for PrivateFault {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("private-debug-probe")
            }
        }
        impl serde::Serialize for PrivateFault {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("private-fault-serializer-probe"))
            }
        }
        let bus = Bus::new();
        for outcome in [Outcome::Next(Broken), Outcome::Fault(PrivateFault)] {
            let fault = matches!(&outcome, Outcome::Fault(_));
            let response = default_response_mapper(
                ExecutionReport::Unpersisted {
                    result: outcome,
                    cleanup: None,
                },
                &bus,
            );
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            assert_eq!(response.headers()["ranvier-durability"], "unpersisted");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = std::str::from_utf8(&body).unwrap();
            assert!(!text.contains("private-"));
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                value,
                if fault {
                    serde_json::json!({"kind":"fault", "error":{"message":"Fault response could not be encoded"}})
                } else {
                    serde_json::json!({"kind":"serialization_error", "error":"JSON response could not be encoded"})
                }
            );
        }
        let response = default_response_mapper(
            ExecutionReport::Unconfirmed {
                result: Outcome::<Broken, PrivateFault>::Next(Broken),
                operation: ranvier_runtime::PersistenceOperation::Result,
                effect: ranvier_runtime::EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            &bus,
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "error":"Execution report could not be encoded",
                "reconciliation_required":true
            })
        );
        let response = response_with_body(
            Response::builder().header("invalid header", "private-builder-probe"),
            Bytes::from_static(b"unused"),
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"HTTP response could not be constructed");
    }
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestError {
        message: String,
    }

    #[derive(Clone)]
    struct NextTransition;

    #[async_trait::async_trait]
    impl Transition<(), serde_json::Value> for NextTransition {
        type Error = TestError;
        type Resources = ();

        async fn run(
            &self,
            _input: (),
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<serde_json::Value, Self::Error> {
            Outcome::Next(serde_json::json!({ "ok": true }))
        }
    }

    #[derive(Clone)]
    struct FaultTransition;

    #[async_trait::async_trait]
    impl Transition<(), serde_json::Value> for FaultTransition {
        type Error = TestError;
        type Resources = ();

        async fn run(
            &self,
            _input: (),
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<serde_json::Value, Self::Error> {
            Outcome::Fault(TestError {
                message: "boom".to_string(),
            })
        }
    }

    #[derive(Clone)]
    struct EmitTransition;

    #[async_trait::async_trait]
    impl Transition<(), serde_json::Value> for EmitTransition {
        type Error = TestError;
        type Resources = ();

        async fn run(
            &self,
            _input: (),
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<serde_json::Value, Self::Error> {
            Outcome::Emit(
                "order.created".to_string(),
                Some(serde_json::json!({ "id": 7 })),
            )
        }
    }

    async fn response_body_json(response: Response<Full<Bytes>>) -> serde_json::Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn iam_failure_mapping_discards_payload_and_preserves_ordinary_events() {
        for (label, status) in [
            ("iam.missing_token", StatusCode::UNAUTHORIZED),
            ("iam.verification_failed", StatusCode::UNAUTHORIZED),
            ("iam.policy_denied", StatusCode::FORBIDDEN),
            (
                "iam.context_clear_failed",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "iam.context_write_denied",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            ("iam.verifier_panicked", StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            for payload in [
                None,
                Some(serde_json::json!({"token":"private-payload-marker"})),
            ] {
                let response = default_response_mapper::<serde_json::Value, TestError>(
                    ExecutionReport::Unpersisted {
                        result: Outcome::Emit(label.into(), payload),
                        cleanup: None,
                    },
                    &Bus::new(),
                );
                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["ranvier-durability"], "unpersisted");
                if status == StatusCode::UNAUTHORIZED {
                    assert_eq!(response.headers()["www-authenticate"], "Bearer");
                } else {
                    assert!(response.headers().get("www-authenticate").is_none());
                }
                let body = response_body_json(response).await;
                assert_eq!(body["kind"], "iam");
                assert_eq!(body["event_type"], label);
                assert!(body["error"].is_string());
                assert!(body.get("payload").is_none());
                assert!(!body.to_string().contains("private-payload-marker"));
            }
        }
        for label in ["order.created", "iam.application_event"] {
            let payload = serde_json::json!({"event":"ordinary-event-data"});
            let response = default_response_mapper::<serde_json::Value, TestError>(
                ExecutionReport::Unpersisted {
                    result: Outcome::Emit(label.into(), Some(payload.clone())),
                    cleanup: None,
                },
                &Bus::new(),
            );
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert!(response.headers().get("www-authenticate").is_none());
            assert_eq!(
                response_body_json(response).await,
                serde_json::json!({"kind":"emit", "event_type":label, "payload":payload})
            );
        }
    }

    fn request() -> Request<Full<Bytes>> {
        Request::new(Full::new(Bytes::new()))
    }

    #[tokio::test]
    async fn service_maps_next_to_json_200() {
        let axon = Axon::<(), (), TestError>::new("next").then(NextTransition);
        let service =
            RanvierService::new(axon, |_req: Request<Full<Bytes>>, _bus: &mut Bus| (), ());

        let response = service.call(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_body_json(response).await,
            serde_json::json!({ "ok": true })
        );
    }

    #[tokio::test]
    async fn service_maps_fault_to_json_500() {
        let axon = Axon::<(), (), TestError>::new("fault").then(FaultTransition);
        let service =
            RanvierService::new(axon, |_req: Request<Full<Bytes>>, _bus: &mut Bus| (), ());

        let response = service.call(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_body_json(response).await;
        assert_eq!(body["kind"], "fault");
        assert_eq!(body["error"]["message"], "boom");
    }

    #[tokio::test]
    async fn service_maps_emit_to_json_202() {
        let axon = Axon::<(), (), TestError>::new("emit").then(EmitTransition);
        let service =
            RanvierService::new(axon, |_req: Request<Full<Bytes>>, _bus: &mut Bus| (), ());

        let response = service.call(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_body_json(response).await;
        assert_eq!(body["kind"], "emit");
        assert_eq!(body["event_type"], "order.created");
    }

    #[tokio::test]
    async fn service_allows_custom_response_mapper() {
        let axon = Axon::<(), (), TestError>::new("custom").then(NextTransition);
        let service =
            RanvierService::new(axon, |_req: Request<Full<Bytes>>, _bus: &mut Bus| (), ())
                .with_response_mapper(|_outcome, _bus| {
                    response_with_body(
                        Response::builder()
                            .status(StatusCode::CREATED)
                            .header(CONTENT_TYPE, "text/plain; charset=utf-8"),
                        Bytes::from_static(b"created"),
                    )
                });

        let response = service.call(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
