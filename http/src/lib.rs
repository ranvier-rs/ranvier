use http::{Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::Service;
use ranvier_core::{Bus, Circuit, Module, ModuleResult};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// A simple wrapper to make a Circuit into a Hyper Service
pub struct CircuitService {
    circuit: Arc<Circuit>,
}

impl CircuitService {
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit: Arc::new(circuit),
        }
    }
}

impl Service<Request<hyper::body::Incoming>> for CircuitService {
    type Response = Response<Full<Bytes>>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<hyper::body::Incoming>) -> Self::Future {
        let circuit = self.circuit.clone();

        Box::pin(async move {
            // Built-in introspection endpoint
            if req.uri().path() == "/__ranvier/schema" {
                let meta = circuit.metadata();
                let json = meta.to_json();
                let body = serde_json::to_string(&json).unwrap_or_default();

                let res = Response::builder()
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
                return Ok(res);
            }

            // map hyper request to core bus
            // Note: We are losing the body stream for now in this MVP mapping unless we process it.
            // For MVP, lets just map the parts.
            let (parts, _body) = req.into_parts();
            let req = Request::from_parts(parts, ());

            let mut bus = Bus::new(req);

            match circuit.execute(&mut bus).await {
                Ok(_) => {
                    // Circuit finished successfully (fell through)
                    let res = bus
                        .res
                        .body(Full::new(Bytes::from("Circuit Finished")))
                        .unwrap();
                    Ok(res)
                }
                Err(e) => {
                    // TODO: Handle Terminate vs Error differently?
                    // For now, simple error handling
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
