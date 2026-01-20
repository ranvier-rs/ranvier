use hyper::body::Incoming;
use ranvier::prelude::*;
use std::convert::Infallible;

pub mod api;

pub async fn main_handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path_str = req.uri().path().to_owned();
    let path = path_str.as_str();

    // Route to /api/...
    if let Some(rest) = path.strip_prefix("/api") {
        return api::handler(req, rest).await;
    }

    match (req.method(), path) {
        (&Method::GET, "/") => Ok(text("Routing Demo Root")),
        _ => Ok(not_found()),
    }
}
