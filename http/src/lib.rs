use http::{Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::Service;
use ranvier_core::{Context, Pipeline, Step, StepResult};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// A simple wrapper to make a Pipeline into a Hyper Service
pub struct RanvierService {
    pipeline: Arc<Pipeline>,
}

impl RanvierService {
    pub fn new(pipeline: Pipeline) -> Self {
        Self {
            pipeline: Arc::new(pipeline),
        }
    }
}

impl Service<Request<hyper::body::Incoming>> for RanvierService {
    type Response = Response<Full<Bytes>>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<hyper::body::Incoming>) -> Self::Future {
        let pipeline = self.pipeline.clone();

        Box::pin(async move {
            // Built-in introspection endpoint
            if req.uri().path() == "/__ranvier/schema" {
                let meta = pipeline.metadata();
                let json = meta.to_json();
                let body = serde_json::to_string(&json).unwrap_or_default();

                let res = Response::builder()
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
                return Ok(res);
            }

            // map hyper request to core context
            // Note: We are losing the body stream for now in this MVP mapping unless we process it.
            // For MVP, lets just map the parts.
            let (parts, _body) = req.into_parts();
            let req = Request::from_parts(parts, ());

            let mut ctx = Context::new(req);

            match pipeline.execute(&mut ctx).await {
                StepResult::Next => {
                    // Pipeline finished successfully (fell through)
                    let res = ctx
                        .res
                        .body(Full::new(Bytes::from("Pipeline Finished")))
                        .unwrap();
                    Ok(res)
                }
                StepResult::Terminate => {
                    let res = ctx.res.body(Full::new(Bytes::from("Terminated"))).unwrap();
                    Ok(res)
                }
                StepResult::Error(e) => {
                    let res = Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Full::new(Bytes::from(format!("Error: {}", e))))
                        .unwrap();
                    Ok(res)
                }
            }
        })
    }
}
