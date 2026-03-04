//! Multipart upload demo for Ranvier HTTP.
//!
//! This example demonstrates handling multipart/form-data file uploads
//! using the `Multipart` extractor from `ranvier-http`.
//!
//! ## Usage
//!
//! 1. Start the server:
//!    ```sh
//!    cargo run -p multipart-upload-demo
//!    ```
//!
//! 2. Send a multipart request:
//!    ```sh
//!    curl -F "username=ranvier" -F "avatar=@photo.jpg" http://localhost:3000/upload
//!    ```

use bytes::Bytes;
use http_body_util::Full;
use ranvier_http::extract::multipart::Multipart;
use ranvier_http::extract::FromRequest;

/// Simulates a handler that processes multipart uploads.
async fn handle_upload(mut mp: Multipart) -> String {
    let (text_fields, files) = match mp.collect_all().await {
        Ok(result) => result,
        Err(e) => return format!("Error processing upload: {e}"),
    };

    let mut response = String::new();
    response.push_str("=== Upload Results ===\n\n");

    response.push_str(&format!("Text fields ({}): \n", text_fields.len()));
    for (name, value) in &text_fields {
        response.push_str(&format!("  {name} = {value}\n"));
    }

    response.push_str(&format!("\nFiles ({}): \n", files.len()));
    for file in &files {
        response.push_str(&format!(
            "  field={}, filename={}, content_type={}, size={} bytes\n",
            file.field_name,
            file.file_name.as_deref().unwrap_or("(unnamed)"),
            file.content_type.as_deref().unwrap_or("(unknown)"),
            file.size(),
        ));
    }

    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Multipart Upload Demo");
    println!("=====================");
    println!();
    println!("This demo shows how to extract multipart form data.");
    println!("Send a request with: curl -F 'name=ranvier' -F 'file=@test.txt' http://localhost:3000/upload");
    println!();

    // Simulate a multipart request for demonstration
    let boundary = "----DemoBoundary";
    let body = build_demo_body(boundary);

    let mut req = http::Request::builder()
        .uri("/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Full::new(Bytes::from(body)))
        .unwrap();

    let mp = Multipart::from_request(&mut req).await.unwrap();
    let result = handle_upload(mp).await;
    println!("{result}");

    Ok(())
}

fn build_demo_body(boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    // Text field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"username\"\r\n\r\n");
    body.extend_from_slice(b"ranvier-developer\r\n");
    // Text field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"project\"\r\n\r\n");
    body.extend_from_slice(b"ranvier-framework\r\n");
    // File upload
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"config\"; filename=\"ranvier.toml\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: application/toml\r\n\r\n");
    body.extend_from_slice(b"[server]\nbind = \"0.0.0.0:3000\"\n\n[log]\nlevel = \"info\"\n\r\n");
    // End
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}
