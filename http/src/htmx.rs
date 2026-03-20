//! htmx integration types for Ranvier HTTP.
//!
//! Provides typed Bus values extracted from htmx request headers (HX-*)
//! and response header injection via `HxResponseHeaders`.
//!
//! Enable with the `htmx` feature flag and call `HttpIngress::htmx_support()`.

use serde::Serialize;

// ── Request-side Bus types ──────────────────────────────────────────

/// Indicates the request was made by htmx (`HX-Request: true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HxRequest(pub bool);

/// The `id` of the target element (`HX-Target` header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HxTarget(pub Option<String>);

/// The `id` of the element that triggered the request (`HX-Trigger` header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HxTrigger(pub Option<String>);

/// The current URL of the browser (`HX-Current-URL` header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HxCurrentUrl(pub Option<String>);

/// Whether the request is via `hx-boost` (`HX-Boosted: true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HxBoosted(pub bool);

// ── Response-side Bus type ──────────────────────────────────────────

/// Response headers to send back to htmx.
///
/// Insert into the Bus and they will be applied to the HTTP response
/// when `htmx_support()` is enabled.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HxResponseHeaders {
    /// HX-Redirect: client-side redirect.
    pub redirect: Option<String>,
    /// HX-Refresh: full page refresh.
    pub refresh: Option<bool>,
    /// HX-Trigger: trigger client-side events.
    pub trigger: Option<String>,
    /// HX-Trigger-After-Swap: trigger after swap.
    pub trigger_after_swap: Option<String>,
    /// HX-Trigger-After-Settle: trigger after settle.
    pub trigger_after_settle: Option<String>,
    /// HX-Retarget: change the target element.
    pub retarget: Option<String>,
    /// HX-Reswap: change the swap method.
    pub reswap: Option<String>,
}

/// Extract htmx request headers from HTTP parts and inject into Bus.
pub(crate) fn inject_htmx_headers(parts: &http::request::Parts, bus: &mut ranvier_core::Bus) {
    let headers = &parts.headers;

    let is_htmx = headers
        .get("hx-request")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);
    bus.insert(HxRequest(is_htmx));

    let target = headers
        .get("hx-target")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    bus.insert(HxTarget(target));

    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    bus.insert(HxTrigger(trigger));

    let current_url = headers
        .get("hx-current-url")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    bus.insert(HxCurrentUrl(current_url));

    let boosted = headers
        .get("hx-boosted")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);
    bus.insert(HxBoosted(boosted));
}

/// Extract htmx response headers from Bus and apply to HTTP response headers.
pub(crate) fn extract_htmx_response_headers(
    bus: &ranvier_core::Bus,
    headers: &mut http::HeaderMap,
) {
    let Some(hx) = bus.read::<HxResponseHeaders>() else {
        return;
    };

    if let Some(ref redirect) = hx.redirect {
        if let Ok(v) = http::HeaderValue::from_str(redirect) {
            headers.insert("hx-redirect", v);
        }
    }
    if let Some(true) = hx.refresh {
        if let Ok(v) = http::HeaderValue::from_str("true") {
            headers.insert("hx-refresh", v);
        }
    }
    if let Some(ref trigger) = hx.trigger {
        if let Ok(v) = http::HeaderValue::from_str(trigger) {
            headers.insert("hx-trigger", v);
        }
    }
    if let Some(ref trigger) = hx.trigger_after_swap {
        if let Ok(v) = http::HeaderValue::from_str(trigger) {
            headers.insert("hx-trigger-after-swap", v);
        }
    }
    if let Some(ref trigger) = hx.trigger_after_settle {
        if let Ok(v) = http::HeaderValue::from_str(trigger) {
            headers.insert("hx-trigger-after-settle", v);
        }
    }
    if let Some(ref retarget) = hx.retarget {
        if let Ok(v) = http::HeaderValue::from_str(retarget) {
            headers.insert("hx-retarget", v);
        }
    }
    if let Some(ref reswap) = hx.reswap {
        if let Ok(v) = http::HeaderValue::from_str(reswap) {
            headers.insert("hx-reswap", v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::Bus;

    #[test]
    fn inject_htmx_headers_extracts_all_fields() {
        let req = http::Request::builder()
            .header("hx-request", "true")
            .header("hx-target", "#content")
            .header("hx-trigger", "btn-click")
            .header("hx-current-url", "http://example.com/page")
            .header("hx-boosted", "true")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        let mut bus = Bus::new();
        inject_htmx_headers(&parts, &mut bus);

        assert_eq!(bus.read::<HxRequest>(), Some(&HxRequest(true)));
        assert_eq!(
            bus.read::<HxTarget>(),
            Some(&HxTarget(Some("#content".to_string())))
        );
        assert_eq!(
            bus.read::<HxTrigger>(),
            Some(&HxTrigger(Some("btn-click".to_string())))
        );
        assert_eq!(
            bus.read::<HxCurrentUrl>(),
            Some(&HxCurrentUrl(Some("http://example.com/page".to_string())))
        );
        assert_eq!(bus.read::<HxBoosted>(), Some(&HxBoosted(true)));
    }

    #[test]
    fn inject_htmx_headers_defaults_when_missing() {
        let req = http::Request::builder().body(()).unwrap();
        let (parts, _) = req.into_parts();
        let mut bus = Bus::new();
        inject_htmx_headers(&parts, &mut bus);

        assert_eq!(bus.read::<HxRequest>(), Some(&HxRequest(false)));
        assert_eq!(bus.read::<HxTarget>(), Some(&HxTarget(None)));
        assert_eq!(bus.read::<HxBoosted>(), Some(&HxBoosted(false)));
    }

    #[test]
    fn extract_response_headers_applies_all_fields() {
        let mut bus = Bus::new();
        bus.insert(HxResponseHeaders {
            redirect: Some("/new-page".to_string()),
            refresh: Some(true),
            trigger: Some("showMessage".to_string()),
            trigger_after_swap: Some("afterSwap".to_string()),
            trigger_after_settle: Some("afterSettle".to_string()),
            retarget: Some("#other".to_string()),
            reswap: Some("innerHTML".to_string()),
        });

        let mut headers = http::HeaderMap::new();
        extract_htmx_response_headers(&bus, &mut headers);

        assert_eq!(headers.get("hx-redirect").unwrap(), "/new-page");
        assert_eq!(headers.get("hx-refresh").unwrap(), "true");
        assert_eq!(headers.get("hx-trigger").unwrap(), "showMessage");
        assert_eq!(headers.get("hx-trigger-after-swap").unwrap(), "afterSwap");
        assert_eq!(
            headers.get("hx-trigger-after-settle").unwrap(),
            "afterSettle"
        );
        assert_eq!(headers.get("hx-retarget").unwrap(), "#other");
        assert_eq!(headers.get("hx-reswap").unwrap(), "innerHTML");
    }

    #[test]
    fn extract_response_headers_noop_when_absent() {
        let bus = Bus::new();
        let mut headers = http::HeaderMap::new();
        extract_htmx_response_headers(&bus, &mut headers);
        assert!(headers.is_empty());
    }
}
