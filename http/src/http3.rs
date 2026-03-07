//! HTTP/3 and QUIC transport support using `quinn` and `h3`.
//!
//! Available when the `http3` feature is enabled.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::{Buf, Bytes};
use h3::server::RequestStream;
use h3_quinn::quinn::{Endpoint, ServerConfig};
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use hyper::service::Service;
use tracing::{debug, error, info, trace};

/// Configuration for the HTTP/3 QUIC server.
#[derive(Clone)]
pub struct Http3Config {
    pub bind_addr: SocketAddr,
    pub cert_chain: Vec<Vec<u8>>,
    pub private_key: Vec<u8>,
}

impl Http3Config {
    /// Create a new configuration with explicit certificates (DER encoded).
    pub fn new(bind_addr: SocketAddr, cert_chain: Vec<Vec<u8>>, private_key: Vec<u8>) -> Self {
        Self {
            bind_addr,
            cert_chain,
            private_key,
        }
    }

    /// Helper to generate a self-signed certificate for local testing and examples.
    /// NEVER USE IN PRODUCTION.
    pub fn generate_self_signed(
        bind_addr: SocketAddr,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync + 'static>> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;

        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.key_pair.serialize_der();

        Ok(Self {
            bind_addr,
            cert_chain: vec![cert_der],
            private_key: key_der,
        })
    }
}

/// Runs the HTTP/3 server with the given configuration and Hyper service.
pub async fn serve<S, B>(
    config: Http3Config,
    service: S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    S: Service<Request<Full<Bytes>>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // 1. Setup TLS for QUIC
    let certs: Vec<CertificateDer<'static>> = config
        .cert_chain
        .into_iter()
        .map(|c| CertificateDer::from(c))
        .collect();

    let key = PrivateKeyDer::try_from(config.private_key)?;

    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    // Enable ALPN for h3
    crypto.alpn_protocols = vec![b"h3".to_vec()];

    let quic_crypto = h3_quinn::quinn::crypto::rustls::QuicServerConfig::try_from(crypto)?;
    let server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));

    // 2. Bind UDP Endpoint
    let endpoint = Endpoint::server(server_config, config.bind_addr)?;

    info!("HTTP/3 server listening on {}", endpoint.local_addr()?);

    // 3. Accept loop
    while let Some(incoming) = endpoint.accept().await {
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming, service).await {
                error!("HTTP/3 connection error: {}", e);
            }
        });
    }

    Ok(())
}

async fn handle_connection<S, B>(
    incoming: h3_quinn::quinn::Incoming,
    service: S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    S: Service<Request<Full<Bytes>>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let connection = incoming.await?;

    let peer_addr = connection.remote_address();
    trace!("QUIC connection accepted from {}", peer_addr);

    let h3_conn = h3_quinn::Connection::new(connection);
    let mut h3_server = h3::server::Connection::new(h3_conn).await?;

    loop {
        match h3_server.accept().await {
            Ok(Some(resolver)) => match resolver.resolve_request().await {
                Ok((req, stream)) => {
                    let service = service.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_request(req, stream, service).await {
                            debug!("HTTP/3 request error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    debug!("HTTP/3 resolve error: {}", e);
                }
            },
            Ok(None) => {
                break; // Connection gracefully closed
            }
            Err(e) => {
                debug!("HTTP/3 accept error: {}", e);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_request<S, B>(
    req: Request<()>,
    mut stream: RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    service: S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    S: Service<Request<Full<Bytes>>, Response = Response<B>>,
    S::Error: std::error::Error + Send + Sync + 'static,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Read request body (simplification: collect into Full<Bytes>)
    let mut body_bytes = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await? {
        while chunk.has_remaining() {
            let data = chunk.chunk();
            body_bytes.extend_from_slice(data);
            let len = data.len();
            chunk.advance(len);
        }
    }

    let (parts, _) = req.into_parts();
    let http_req = Request::from_parts(parts, Full::new(Bytes::from(body_bytes)));

    // Process via Hyper service
    match service.call(http_req).await {
        Ok(res) => {
            let (parts, body) = res.into_parts();
            let http_res = Response::from_parts(parts, ());

            // Send headers
            stream.send_response(http_res).await?;

            // Send body stream
            let mut body_stream = std::pin::pin!(body);
            while let Some(frame) = body_stream.frame().await {
                if let Ok(frame) = frame {
                    if let Ok(data) = frame.into_data() {
                        let bytes: Bytes = data;
                        stream.send_data(bytes).await?;
                    }
                }
            }
            stream.finish().await?;
        }
        Err(e) => {
            error!("HTTP/3 service error: {}", e);
            // Internal Server Error
            let res = Response::builder().status(500).body(()).expect("valid HTTP response construction");
            stream.send_response(res).await?;
            stream.finish().await?;
        }
    }

    Ok(())
}
