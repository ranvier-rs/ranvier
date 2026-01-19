/// Splits the path into the next segment and the remainder.
///
/// Use this for dynamic routing where you need to extract a variable part of the path.
///
/// # Example
/// ```
/// use ranvier::routing::next_segment;
///
/// let path = "users/123/posts";
/// let (segment, rest) = next_segment(path);
/// assert_eq!(segment, "users");
/// assert_eq!(rest, Some("123/posts"));
///
/// let path = "last";
/// let (segment, rest) = next_segment(path);
/// assert_eq!(segment, "last");
/// assert_eq!(rest, None);
/// ```
pub fn next_segment(path: &str) -> (&str, Option<&str>) {
    match path.split_once('/') {
        Some((segment, rest)) => (segment, Some(rest)),
        None => (path, None),
    }
}
