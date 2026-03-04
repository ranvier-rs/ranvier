use anyhow::{Context, Result};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct OrderPath {
    id: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct OrderQuery {
    page: u32,
    size: u32,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct CreateOrder {
    sku: String,
    qty: u32,
}

#[derive(Debug, Serialize, Deserialize)]
enum DemoRouteError {
    Unauthorized,
    Missing,
    Internal,
}

#[transition]
async fn order_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, String> {
    Outcome::Next("route:/orders/:id".to_string())
}

#[transition]
async fn asset_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, String> {
    Outcome::Next("route:/assets/*path".to_string())
}

#[transition]
async fn not_found_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, String> {
    Outcome::Next("route:fallback".to_string())
}

fn order_circuit() -> Axon<(), String, String> {
    Axon::<(), (), String>::new("OrderRoute").then(order_route)
}

fn asset_circuit() -> Axon<(), String, String> {
    Axon::<(), (), String>::new("AssetRoute").then(asset_route)
}

fn fallback_circuit() -> Axon<(), String, String> {
    Axon::<(), (), String>::new("FallbackRoute").then(not_found_route)
}

#[transition]
async fn unauthorized_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, DemoRouteError> {
    Outcome::Fault(DemoRouteError::Unauthorized)
}

#[transition]
async fn missing_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, DemoRouteError> {
    Outcome::Fault(DemoRouteError::Missing)
}

#[transition]
async fn internal_route(
    _state: (),
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, DemoRouteError> {
    Outcome::Fault(DemoRouteError::Internal)
}

fn unauthorized_circuit() -> Axon<(), String, DemoRouteError> {
    Axon::<(), (), DemoRouteError>::new("UnauthorizedRoute").then(unauthorized_route)
}

fn missing_circuit() -> Axon<(), String, DemoRouteError> {
    Axon::<(), (), DemoRouteError>::new("MissingRoute").then(missing_route)
}

fn internal_circuit() -> Axon<(), String, DemoRouteError> {
    Axon::<(), (), DemoRouteError>::new("InternalRoute").then(internal_route)
}

fn custom_error_response(error: &DemoRouteError) -> HttpResponse {
    match error {
        DemoRouteError::Unauthorized => {
            json_error_response(http::StatusCode::UNAUTHORIZED, "unauthorized")
        }
        DemoRouteError::Missing => json_error_response(http::StatusCode::NOT_FOUND, "not_found"),
        DemoRouteError::Internal => {
            json_error_response(http::StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

async fn wait_for_server(addr: &str) -> Result<()> {
    for _ in 0..40 {
        if TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow::anyhow!("server did not start on {addr}"))
}

async fn send_http_get(addr: &str, path: &str) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect failed: {addr}"))?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .context("write request failed")?;

    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .await
        .context("read response failed")?;

    let response = String::from_utf8(buffer).context("response was not utf-8")?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("invalid HTTP response format")?;
    let status_line = head.lines().next().context("missing status line")?;
    let code = status_line
        .split_whitespace()
        .nth(1)
        .context("missing status code")?
        .parse::<u16>()
        .context("invalid status code")?;

    Ok((code, body.to_string()))
}

async fn demo_dynamic_routes() -> Result<()> {
    let probe = StdTcpListener::bind("127.0.0.1:0").context("bind probe listener")?;
    let addr = probe.local_addr().context("resolve probe local addr")?;
    drop(probe);
    let addr_text = addr.to_string();

    let ingress = Ranvier::http()
        .bind(addr_text.clone())
        .get("/orders/:id", order_circuit())
        .get("/assets/*path", asset_circuit())
        .fallback(fallback_circuit());

    let server = tokio::spawn(async move {
        let _ = ingress.run(()).await;
    });

    wait_for_server(&addr_text).await?;

    let (order_status, order_body) = send_http_get(&addr_text, "/orders/42").await?;
    let (asset_status, asset_body) = send_http_get(&addr_text, "/assets/css/theme.css").await?;
    let (fallback_status, fallback_body) = send_http_get(&addr_text, "/unknown").await?;

    assert_eq!(order_status, 200);
    assert_eq!(order_body, "route:/orders/:id");
    assert_eq!(asset_status, 200);
    assert_eq!(asset_body, "route:/assets/*path");
    assert_eq!(fallback_status, 404);
    assert_eq!(fallback_body, "route:fallback");

    server.abort();
    let _ = server.await;

    println!("Dynamic route matching OK: /orders/:id, /assets/*path, fallback");
    Ok(())
}

async fn demo_extractors() -> Result<()> {
    let mut req = Request::builder()
        .uri("/orders/42?page=3&size=10")
        .body(Full::new(Bytes::from_static(br#"{"sku":"book","qty":2}"#)))
        .context("build request")?;

    let mut params = HashMap::new();
    params.insert("id".to_string(), "42".to_string());
    req.extensions_mut().insert(PathParams::new(params));

    let Path(path): Path<OrderPath> = Path::from_request(&mut req).await?;
    let Query(query): Query<OrderQuery> = Query::from_request(&mut req).await?;
    let Json(payload): Json<CreateOrder> = Json::from_request(&mut req).await?;

    assert_eq!(path, OrderPath { id: 42 });
    assert_eq!(query, OrderQuery { page: 3, size: 10 });
    assert_eq!(
        payload,
        CreateOrder {
            sku: "book".to_string(),
            qty: 2,
        }
    );

    println!("Request extractors OK: Path<OrderPath>, Query<OrderQuery>, Json<CreateOrder>");
    Ok(())
}

async fn demo_custom_error_mapping() -> Result<()> {
    let probe = StdTcpListener::bind("127.0.0.1:0").context("bind probe listener")?;
    let addr = probe.local_addr().context("resolve probe local addr")?;
    drop(probe);
    let addr_text = addr.to_string();

    let ingress = Ranvier::http()
        .bind(addr_text.clone())
        .get_with_error("/secure", unauthorized_circuit(), custom_error_response)
        .get_with_error("/missing", missing_circuit(), custom_error_response)
        .get_with_error("/explode", internal_circuit(), custom_error_response);

    let server = tokio::spawn(async move {
        let _ = ingress.run(()).await;
    });

    wait_for_server(&addr_text).await?;

    let (unauthorized_status, unauthorized_body) = send_http_get(&addr_text, "/secure").await?;
    let (missing_status, missing_body) = send_http_get(&addr_text, "/missing").await?;
    let (internal_status, internal_body) = send_http_get(&addr_text, "/explode").await?;

    assert_eq!(unauthorized_status, 401);
    assert!(unauthorized_body.contains("\"error\":\"unauthorized\""));
    assert_eq!(missing_status, 404);
    assert!(missing_body.contains("\"error\":\"not_found\""));
    assert_eq!(internal_status, 500);
    assert!(internal_body.contains("\"error\":\"internal_error\""));

    server.abort();
    let _ = server.await;

    println!("Custom error mapping OK: 401/404/500 JSON responses");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    demo_dynamic_routes().await?;
    demo_extractors().await?;
    demo_custom_error_mapping().await?;
    println!("routing-params-demo complete.");
    Ok(())
}
