//! Pipeline Demo - Demonstrating Ranvier's pipe() API
//!
//! This example shows the core pipeline chaining feature inspired by 006.md.

use std::sync::Arc;
use ranvier::prelude::*;

// --- Domain Types ---
#[derive(Debug)]
struct HttpRequest {
    url: String,
}

#[derive(Debug)]
struct ParsedId(i32);

#[derive(Debug)]
struct UserData {
    name: String,
    role: String,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: String,
}

// --- Step Functions (Pure Business Logic) ---

async fn parse_path(req: HttpRequest, _ctx: Arc<Context>) -> Result<ParsedId, Error> {
    // Parse user ID from URL like "/users/101"
    let id = req
        .url
        .split('/')
        .last()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::BadRequest("Invalid URL".into()))?;
    
    println!("  [parse_path] Parsed ID: {}", id);
    Ok(ParsedId(id))
}

async fn fetch_db(id: ParsedId, ctx: Arc<Context>) -> Result<UserData, Error> {
    // Simulate database fetch
    println!("  [fetch_db] Fetching user {} from context", id.0);
    
    // In real code, use ctx to access database
    Ok(UserData {
        name: "Naru".into(),
        role: "admin".into(),
    })
}

async fn check_permission(user: UserData, _ctx: Arc<Context>) -> Result<UserData, Error> {
    println!("  [check_permission] Checking role: {}", user.role);
    
    if user.role == "admin" {
        Ok(user)
    } else {
        Err(Error::Unauthorized)
    }
}

async fn render_json(user: UserData, _ctx: Arc<Context>) -> Result<HttpResponse, Error> {
    let body = format!(r#"{{ "name": "{}" }}"#, user.name);
    println!("  [render_json] Rendering response");
    
    Ok(HttpResponse {
        status: 200,
        body,
    })
}

#[tokio::main]
async fn main() {
    println!("=== Ranvier Pipeline Demo ===\n");

    // Build the pipeline by chaining steps
    let app = Pipeline::new(parse_path)
        .pipe(fetch_db)          // ParsedId -> UserData
        .pipe(check_permission)  // UserData -> UserData (validation)
        .pipe(render_json);      // UserData -> HttpResponse

    // Create execution context
    let ctx = Arc::new(Context::new());

    // Simulate a request
    let req = HttpRequest {
        url: "/users/101".into(),
    };

    println!("Request: {:?}\n", req);
    println!("Pipeline execution:");

    // Execute the pipeline
    match app.execute(req, ctx).await {
        Ok(res) => println!("\n✅ Response: {} {}", res.status, res.body),
        Err(e) => println!("\n❌ Error: {:?}", e),
    }
}
