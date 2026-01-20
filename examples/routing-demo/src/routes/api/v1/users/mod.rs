use hyper::body::Incoming;
use ranvier::prelude::*;
use std::convert::Infallible;

pub mod by_id;

pub async fn handler(req: Request<Incoming>, path: &str) -> Result<Response<Full<Bytes>>, Infallible> {
    // path comes in relative to /api/v1/users (e.g. "" or "/123" or "/123/posts")
    
    // Check for dynamic segment (User ID)
    let (segment, rest) = next_segment(path);

    if !segment.is_empty() {
        // If segment is numeric, treat it as User ID
        // (In a real app, logic might be more complex)
        if segment.chars().all(char::is_numeric) {
            // Delegate to by_id handler
            // rest is Some("...") or None
            let subpath = rest.unwrap_or("");
            return by_id::handler(req, segment, subpath).await;
        }
    }

    // Static routes for /api/v1/users
    match (req.method(), path) {
        (&Method::GET, "") | (&Method::GET, "/") => Ok(json(&vec!["User1", "User2"])),
        (&Method::POST, "") | (&Method::POST, "/") => Ok(text("User Created")),
        _ => Ok(not_found())
    }
}
