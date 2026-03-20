//! # Tier 3 Advanced Guards (feature: `advanced`)
//!
//! These Guards handle less common but important HTTP patterns:
//! request body decompression, conditional requests (304), and redirects.

use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

// ---------------------------------------------------------------------------
// DecompressionGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the raw request body bytes.
///
/// The HTTP ingress layer writes this before the Guard executes.
/// If the request has `Content-Encoding: gzip`, the Guard decompresses
/// the body and replaces this value in the Bus.
#[derive(Debug, Clone)]
pub struct RequestBody(pub Vec<u8>);

/// Decompression guard — decompresses request bodies encoded with gzip.
///
/// Reads `RequestBody` and the `Content-Encoding` header value from the Bus.
/// If the encoding is `gzip`, decompresses using flate2. Other encodings
/// (brotli, zstd) are passed through unchanged.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .guard(DecompressionGuard::new())
///     .post("/api/upload", upload_circuit)
/// ```
#[derive(Debug, Clone)]
pub struct DecompressionGuard<T> {
    _marker: PhantomData<T>,
}

impl<T> DecompressionGuard<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for DecompressionGuard<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Bus-injectable type representing the request's `Content-Encoding` header.
#[derive(Debug, Clone)]
pub struct RequestContentEncoding(pub String);

#[async_trait]
impl<T> Transition<T, T> for DecompressionGuard<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        let Some(encoding) = bus.read::<RequestContentEncoding>().map(|e| e.0.clone()) else {
            return Outcome::next(input);
        };

        let encoding_lower = encoding.to_lowercase();
        if encoding_lower != "gzip" {
            // Only gzip is supported; other encodings pass through
            return Outcome::next(input);
        }

        let Some(body) = bus.read::<RequestBody>().map(|b| b.0.clone()) else {
            return Outcome::next(input);
        };

        // Decompress gzip
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                bus.insert(RequestBody(decompressed));
                Outcome::next(input)
            }
            Err(e) => Outcome::fault(format!(
                "400 Bad Request: failed to decompress gzip body: {}",
                e
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// ConditionalRequestGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type for the `If-None-Match` header value.
#[derive(Debug, Clone)]
pub struct IfNoneMatch(pub String);

/// Bus-injectable type for the `If-Modified-Since` header value.
#[derive(Debug, Clone)]
pub struct IfModifiedSince(pub String);

/// Bus-injectable type for the current resource ETag.
///
/// Set by application code (e.g., a Transition that computes a hash).
/// The Guard compares this against `If-None-Match`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ETag(pub String);

/// Bus-injectable type for the current resource's last modification time.
///
/// Set by application code. The Guard compares this against `If-Modified-Since`.
/// Format: HTTP-date (RFC 7231, e.g., "Sun, 06 Nov 1994 08:49:37 GMT").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastModified(pub String);

/// Conditional request guard — returns 304 Not Modified when appropriate.
///
/// Implements RFC 7232 conditional request handling:
/// - Compares `If-None-Match` against [`ETag`] in the Bus
/// - Compares `If-Modified-Since` against [`LastModified`] in the Bus
///
/// If conditions indicate the resource hasn't changed, returns a Fault
/// with "304 Not Modified" which the HTTP ingress translates to a 304 response.
///
/// **Important:** Application code must set [`ETag`] and/or [`LastModified`]
/// in the Bus (typically via a Transition that runs before this Guard in
/// a per-route configuration).
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .get_with_guards("/api/resource", resource_circuit, guards![
///         ConditionalRequestGuard::new(),
///     ])
/// ```
#[derive(Debug, Clone)]
pub struct ConditionalRequestGuard<T> {
    _marker: PhantomData<T>,
}

impl<T> ConditionalRequestGuard<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for ConditionalRequestGuard<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T> Transition<T, T> for ConditionalRequestGuard<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        // Check If-None-Match vs ETag
        if let (Some(if_none_match), Some(etag)) =
            (bus.read::<IfNoneMatch>(), bus.read::<ETag>())
        {
            let client_etag = if_none_match.0.trim().trim_matches('"');
            let server_etag = etag.0.trim().trim_matches('"');
            if client_etag == server_etag || client_etag == "*" {
                return Outcome::fault("304 Not Modified".to_string());
            }
        }

        // Check If-Modified-Since vs LastModified
        if let (Some(if_modified), Some(last_modified)) =
            (bus.read::<IfModifiedSince>(), bus.read::<LastModified>())
        {
            // Simple string comparison — both should be in HTTP-date format
            // A more robust implementation would parse dates
            if if_modified.0.trim() == last_modified.0.trim() {
                return Outcome::fault("304 Not Modified".to_string());
            }
        }

        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// RedirectGuard
// ---------------------------------------------------------------------------

/// A redirect rule mapping a source path to a target URL with a status code.
#[derive(Debug, Clone)]
pub struct RedirectRule {
    /// Source path to match (exact match).
    pub from: String,
    /// Target URL to redirect to.
    pub to: String,
    /// HTTP status code (301 or 302).
    pub status: u16,
}

impl RedirectRule {
    /// Create a 301 permanent redirect.
    pub fn permanent(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            status: 301,
        }
    }

    /// Create a 302 temporary redirect.
    pub fn temporary(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            status: 302,
        }
    }
}

/// Bus-injectable type for the current request path (used by RedirectGuard).
#[derive(Debug, Clone)]
pub struct RedirectRequestPath(pub String);

/// Redirect guard — applies registered redirect rules.
///
/// Checks the request path against registered [`RedirectRule`]s.
/// On match, returns a Fault with "301 Location: ..." or "302 Location: ..."
/// which the HTTP ingress translates to a redirect response.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .guard(RedirectGuard::new(vec![
///         RedirectRule::permanent("/old-page", "/new-page"),
///         RedirectRule::temporary("/seasonal", "/summer-sale"),
///     ]))
///     .get("/new-page", page_circuit)
/// ```
#[derive(Debug, Clone)]
pub struct RedirectGuard<T> {
    rules: Vec<RedirectRule>,
    _marker: PhantomData<T>,
}

impl<T> RedirectGuard<T> {
    /// Create with redirect rules.
    pub fn new(rules: Vec<RedirectRule>) -> Self {
        Self {
            rules,
            _marker: PhantomData,
        }
    }

    /// Returns the redirect rules.
    pub fn rules(&self) -> &[RedirectRule] {
        &self.rules
    }
}

#[async_trait]
impl<T> Transition<T, T> for RedirectGuard<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        let path = bus
            .read::<RedirectRequestPath>()
            .map(|p| p.0.clone())
            .unwrap_or_default();

        for rule in &self.rules {
            if rule.from == path {
                return Outcome::fault(format!(
                    "{} Location: {}",
                    rule.status, rule.to
                ));
            }
        }

        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- DecompressionGuard tests ---

    #[tokio::test]
    async fn decompression_no_encoding_passes_through() {
        let guard = DecompressionGuard::<String>::new();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn decompression_gzip_decompresses() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let guard = DecompressionGuard::<String>::new();

        // Compress some data
        let original = b"Hello, decompression!";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut bus = Bus::new();
        bus.insert(RequestContentEncoding("gzip".into()));
        bus.insert(RequestBody(compressed));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));

        let decompressed = bus.read::<RequestBody>().unwrap();
        assert_eq!(decompressed.0, original);
    }

    #[tokio::test]
    async fn decompression_invalid_gzip_faults() {
        let guard = DecompressionGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(RequestContentEncoding("gzip".into()));
        bus.insert(RequestBody(vec![0xFF, 0xFE, 0xFD])); // invalid gzip

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("400")));
    }

    #[tokio::test]
    async fn decompression_non_gzip_passes_through() {
        let guard = DecompressionGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(RequestContentEncoding("br".into()));
        bus.insert(RequestBody(b"some data".to_vec()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    // --- ConditionalRequestGuard tests ---

    #[tokio::test]
    async fn conditional_etag_match_returns_304() {
        let guard = ConditionalRequestGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(IfNoneMatch("\"abc123\"".into()));
        bus.insert(ETag("\"abc123\"".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("304")));
    }

    #[tokio::test]
    async fn conditional_etag_mismatch_passes() {
        let guard = ConditionalRequestGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(IfNoneMatch("\"abc123\"".into()));
        bus.insert(ETag("\"def456\"".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn conditional_wildcard_etag_returns_304() {
        let guard = ConditionalRequestGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(IfNoneMatch("*".into()));
        bus.insert(ETag("\"any-etag\"".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("304")));
    }

    #[tokio::test]
    async fn conditional_modified_since_match_returns_304() {
        let guard = ConditionalRequestGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(IfModifiedSince("Sun, 06 Nov 1994 08:49:37 GMT".into()));
        bus.insert(LastModified("Sun, 06 Nov 1994 08:49:37 GMT".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("304")));
    }

    #[tokio::test]
    async fn conditional_no_headers_passes() {
        let guard = ConditionalRequestGuard::<String>::new();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    // --- RedirectGuard tests ---

    #[tokio::test]
    async fn redirect_matches_rule() {
        let guard = RedirectGuard::<String>::new(vec![
            RedirectRule::permanent("/old", "/new"),
        ]);
        let mut bus = Bus::new();
        bus.insert(RedirectRequestPath("/old".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("301") && e.contains("/new")));
    }

    #[tokio::test]
    async fn redirect_temporary() {
        let guard = RedirectGuard::<String>::new(vec![
            RedirectRule::temporary("/temp", "/target"),
        ]);
        let mut bus = Bus::new();
        bus.insert(RedirectRequestPath("/temp".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("302")));
    }

    #[tokio::test]
    async fn redirect_no_match_passes() {
        let guard = RedirectGuard::<String>::new(vec![
            RedirectRule::permanent("/old", "/new"),
        ]);
        let mut bus = Bus::new();
        bus.insert(RedirectRequestPath("/other".into()));

        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }
}
