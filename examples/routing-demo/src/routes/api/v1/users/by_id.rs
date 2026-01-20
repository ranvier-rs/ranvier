use hyper::body::Incoming;
use ranvier::prelude::*;
use std::convert::Infallible;

/// Handler for /users/:id/*
pub async fn handler(
    req: Request<Incoming>,
    user_id: &str, // Extracted ID passed from parent
    path: &str,    // Remaining path
) -> Result<Response<Full<Bytes>>, Infallible> {
    
    match (req.method(), path) {
        (&Method::GET, "") | (&Method::GET, "/") => {
            Ok(json(&format!("Details for User ID: {}", user_id)))
        }

        (&Method::GET, "/posts") => {
            Ok(json(&vec![
                format!("Post 1 by user {}", user_id),
                format!("Post 2 by user {}", user_id),
            ]))
        }
        
        _ => Ok(not_found())
    }
}
