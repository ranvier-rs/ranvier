use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::service::RanvierService;
use ranvier_runtime::Axon;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(serde::Deserialize)]
struct Broken;

#[derive(Clone)]
struct Business(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<(), Broken> for Business {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<Broken, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(Broken)
    }
}

impl serde::Serialize for Broken {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "private-service-serializer-probe",
        ))
    }
}

struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn service_encoding_failure_over_tcp_preserves_effect_boundary() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<(), (), String>::new("service-encoding-tcp")
            .then(Business(effects.clone()));
        let service = RanvierService::new(
            axon, |_: http::Request<hyper::body::Incoming>, _: &mut Bus| (), (),
        );
        // Keep the listener bound through accept: there is no free-port race.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut server = AbortTask(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service).await.unwrap();
        }));
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
        let mut driver = AbortTask(tokio::spawn(connection));
        let response = sender.send_request(http::Request::builder()
            .uri("/execute").header("host", "fixture").header("connection", "close")
            .body(Full::new(Bytes::new())).unwrap()).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.headers()["ranvier-durability"], "unpersisted");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"kind":"serialization_error", "error":"JSON response could not be encoded"}));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        drop(sender);
        (&mut driver.0).await.unwrap().unwrap();
        (&mut server.0).await.unwrap();
    }).await.expect("bounded service TCP fixture");
}
