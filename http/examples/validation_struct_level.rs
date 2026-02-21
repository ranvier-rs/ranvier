#![cfg_attr(not(feature = "validation"), allow(dead_code, unused_imports))]

#[cfg(feature = "validation")]
use bytes::Bytes;
#[cfg(feature = "validation")]
use http::Request;
#[cfg(feature = "validation")]
use http_body_util::Full;
#[cfg(feature = "validation")]
use ranvier_http::extract::FromRequest;
#[cfg(feature = "validation")]
use ranvier_http::extract::Json;
#[cfg(feature = "validation")]
use serde::Deserialize;
#[cfg(feature = "validation")]
use validator::Validate;

#[cfg(feature = "validation")]
#[derive(Debug, Deserialize, Validate)]
#[validate(schema(function = "validate_total"))]
struct CheckoutRequest {
    #[validate(length(min = 3, message = "order_id must be at least 3 chars"))]
    order_id: String,
    subtotal: i64,
    tax: i64,
    total: i64,
}

#[cfg(feature = "validation")]
fn validate_total(input: &CheckoutRequest) -> Result<(), validator::ValidationError> {
    if input.subtotal + input.tax != input.total {
        return Err(validator::ValidationError::new("total_mismatch"));
    }
    Ok(())
}

#[cfg(feature = "validation")]
#[tokio::main]
async fn main() {
    let payload = br#"{"order_id":"ab","subtotal":100,"tax":10,"total":50}"#;
    let mut request = Request::builder()
        .uri("/checkout")
        .body(Full::new(Bytes::from_static(payload)))
        .expect("request should build");

    match Json::<CheckoutRequest>::from_request(&mut request).await {
        Ok(Json(valid)) => {
            println!("validated payload: {}", valid.order_id);
        }
        Err(error) => {
            let response = error.into_http_response();
            println!("validation failed with status {}", response.status());
        }
    }
}

#[cfg(not(feature = "validation"))]
fn main() {
    eprintln!(
        "This example requires the `validation` feature.\nRun: cargo run -p ranvier-http --example validation_struct_level --features validation"
    );
}