//! Sanitization utilities for input handling.
//!
//! Lightweight helpers for preventing XSS, path traversal, and header injection.
//! These are defense-in-depth measures — typed extractors (`Json<T>`, `Query<T>`)
//! already reject malformed input at the parsing layer.

/// Sanitize an HTML string by escaping dangerous characters.
///
/// Prevents XSS by replacing `<`, `>`, `&`, `"`, `'` with HTML entities.
///
/// ```
/// use ranvier_guard::sanitize::escape_html;
/// assert_eq!(escape_html("<script>alert('xss')</script>"), "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;&#x2F;script&gt;");
/// ```
pub fn escape_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#x27;"),
            '/' => output.push_str("&#x2F;"),
            _ => output.push(ch),
        }
    }
    output
}

/// Canonicalize a file path and verify it stays within the given base directory.
///
/// Returns `None` if the path escapes the base via `..` or is otherwise invalid.
///
/// ```no_run
/// use ranvier_guard::sanitize::safe_path;
/// assert!(safe_path("/srv/uploads", "file.txt").is_some());
/// assert!(safe_path("/srv/uploads", "../etc/passwd").is_none());
/// assert!(safe_path("/srv/uploads", "subdir/file.txt").is_some());
/// ```
pub fn safe_path(base: &str, user_path: &str) -> Option<std::path::PathBuf> {
    use std::path::Path;

    let base = Path::new(base).canonicalize().ok()?;
    let joined = base.join(user_path);

    // Reject any path containing .. components before canonicalization
    for component in std::path::Path::new(user_path).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return None;
        }
    }

    // The joined path must start with base
    if joined.starts_with(&base) {
        Some(joined)
    } else {
        None
    }
}

/// Strip potentially dangerous characters from a header value.
///
/// Removes CR, LF, and null bytes that could enable header injection attacks.
///
/// ```
/// use ranvier_guard::sanitize::clean_header_value;
/// assert_eq!(clean_header_value("normal-value"), "normal-value");
/// assert_eq!(clean_header_value("inject\r\nX-Evil: yes"), "injectX-Evil: yes");
/// ```
pub fn clean_header_value(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escaping() {
        assert_eq!(escape_html("hello"), "hello");
        assert_eq!(escape_html("<b>bold</b>"), "&lt;b&gt;bold&lt;&#x2F;b&gt;");
        assert_eq!(escape_html("a&b"), "a&amp;b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(escape_html("it's"), "it&#x27;s");
    }

    #[test]
    fn path_traversal_blocked() {
        assert!(safe_path(".", "normal.txt").is_some());
        assert!(safe_path(".", "../../../etc/passwd").is_none());
        assert!(safe_path(".", "..\\..\\windows\\system32").is_none());
    }

    #[test]
    fn header_injection_stripped() {
        assert_eq!(clean_header_value("ok"), "ok");
        assert_eq!(clean_header_value("value\r\nInjected: header"), "valueInjected: header");
        assert_eq!(clean_header_value("null\0byte"), "nullbyte");
    }
}
