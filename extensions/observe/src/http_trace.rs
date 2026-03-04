use std::future::Future;
use std::pin::Pin;
use std::sync::Once;
use std::task::{Context as TaskContext, Poll};

use http::{HeaderMap, Request};
use opentelemetry::Context;
use opentelemetry::global;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tower::{Layer, Service};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

static TRACE_PROPAGATOR_INIT: Once = Once::new();

/// Parsed W3C trace context propagated from inbound HTTP headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingTraceContext {
    trace_id: String,
    span_id: String,
    tracestate: Option<String>,
}

impl IncomingTraceContext {
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn span_id(&self) -> &str {
        &self.span_id
    }

    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }
}

struct HeaderExtractor<'a> {
    headers: &'a HeaderMap,
}

impl<'a> HeaderExtractor<'a> {
    fn new(headers: &'a HeaderMap) -> Self {
        Self { headers }
    }
}

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.keys().map(|name| name.as_str()).collect()
    }
}

/// Extract OpenTelemetry context from inbound W3C trace headers.
pub fn extract_trace_context(headers: &HeaderMap) -> Context {
    ensure_trace_context_propagator();
    global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor::new(headers)))
}

/// Extract a lightweight snapshot of inbound W3C trace context.
pub fn extract_trace_context_snapshot(headers: &HeaderMap) -> Option<IncomingTraceContext> {
    let context = extract_trace_context(headers);
    let span = context.span();
    let span_context = span.span_context();

    if !span_context.is_valid() {
        return None;
    }

    let tracestate = headers
        .get("tracestate")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    Some(IncomingTraceContext {
        trace_id: span_context.trace_id().to_string(),
        span_id: span_context.span_id().to_string(),
        tracestate,
    })
}

fn ensure_trace_context_propagator() {
    TRACE_PROPAGATOR_INIT.call_once(|| {
        global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// Tower layer that extracts W3C trace context and sets it as parent context.
#[derive(Clone, Default)]
pub struct TraceContextLayer;

impl TraceContextLayer {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Clone)]
pub struct TraceContextService<S> {
    inner: S,
}

impl<S> Layer<S> for TraceContextLayer {
    type Service = TraceContextService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TraceContextService { inner }
    }
}

impl<S, B> Service<Request<B>> for TraceContextService<S>
where
    S: Service<Request<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let parent_context = extract_trace_context(req.headers());
        if let Some(snapshot) = extract_trace_context_snapshot(req.headers()) {
            req.extensions_mut().insert(snapshot);
        }

        let mut inner = self.inner.clone();
        let span = tracing::info_span!("ranvier.observe.trace_context");
        span.set_parent(parent_context);

        Box::pin(async move { inner.call(req).instrument(span).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Response;
    use std::convert::Infallible;
    use tower::{Service, service_fn};

    #[test]
    fn extract_trace_context_snapshot_parses_traceparent_header() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            traceparent.parse().expect("traceparent header"),
        );
        headers.insert(
            "tracestate",
            "vendorname=opaqueValue".parse().expect("tracestate header"),
        );

        let snapshot =
            extract_trace_context_snapshot(&headers).expect("trace context should be present");

        assert_eq!(snapshot.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(snapshot.span_id(), "00f067aa0ba902b7");
        assert_eq!(snapshot.tracestate(), Some("vendorname=opaqueValue"));
    }

    #[tokio::test]
    async fn trace_context_layer_injects_snapshot_into_request_extensions() {
        let layer = TraceContextLayer::new();
        let mut service = layer.layer(service_fn(|req: Request<()>| async move {
            let trace = req
                .extensions()
                .get::<IncomingTraceContext>()
                .expect("trace snapshot extension should exist");
            let body = format!("trace_id={}", trace.trace_id());
            Ok::<_, Infallible>(Response::builder().status(200).body(body).unwrap())
        }));

        let mut request = Request::builder()
            .uri("http://localhost/test")
            .body(())
            .expect("request");
        request.headers_mut().insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
                .parse()
                .expect("traceparent"),
        );

        let response = service.call(request).await.expect("service response");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), "trace_id=4bf92f3577b34da6a3ce929d0e0e4736");
    }
}
