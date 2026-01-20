use hyper::body::Incoming;
use ranvier::prelude::*;
use std::convert::Infallible;

pub mod users;

pub async fn handler(req: Request<Incoming>, path: &str) -> Result<Response<Full<Bytes>>, Infallible> {
    // path relative to /api/v1 (e.g. "/users")

    if let Some(rest) = path.strip_prefix("/users") {
        return users::handler(req, rest).await;
    }

    match (req.method(), path) {
        (&Method::GET, "/") => Ok(text("API v1 Root")),
        _ => Ok(not_found())
    }
}
