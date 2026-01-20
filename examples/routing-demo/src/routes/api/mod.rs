use hyper::body::Incoming;
use ranvier::prelude::*;
use std::convert::Infallible;

pub mod v1;

pub async fn handler(req: Request<Incoming>, path: &str) -> Result<Response<Full<Bytes>>, Infallible> {
    // path comes in relative to /api (e.g. "/v1/users")
    
    if let Some(rest) = path.strip_prefix("/v1") {
        return v1::handler(req, rest).await;
    }

    Ok(not_found())
}
