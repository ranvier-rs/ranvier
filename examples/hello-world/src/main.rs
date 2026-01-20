use ranvier::prelude::*;

#[tokio::main]
async fn main() {
    ranvier::serve(3000, |req| async move {
        match (req.method(), req.uri().path()) {
            (&Method::GET, "/") => Ok(text("Hello, Ranvier!")),
            (&Method::GET, "/status") => Ok(text("Server is running")),
            _ => Ok(not_found()),
        }
    })
    .await
    .unwrap();
}
