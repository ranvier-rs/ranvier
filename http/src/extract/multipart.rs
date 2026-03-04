//! Multipart form-data extractor for Ranvier HTTP.
//!
//! This module provides a convenience wrapper over the `multer` crate
//! to extract multipart/form-data fields and uploaded files from HTTP requests.
//!
//! # Example
//!
//! ```rust,ignore
//! use ranvier_http::extract::multipart::Multipart;
//!
//! async fn handle_upload(mut mp: Multipart) -> String {
//!     while let Some(field) = mp.next_field().await.unwrap() {
//!         let name = field.name().unwrap_or("unknown").to_string();
//!         let data = field.bytes().await.unwrap();
//!         println!("{name}: {} bytes", data.len());
//!     }
//!     "uploaded".into()
//! }
//! ```

use super::{ExtractError, FromRequest};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::Stream;
use http::Request;
use http_body::Body;
use multer::Multipart as MulterMultipart;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Default maximum size for the entire multipart body (10 MB).
pub const DEFAULT_MULTIPART_LIMIT: usize = 10 * 1024 * 1024;

/// Default maximum size for a single field (2 MB).
pub const DEFAULT_FIELD_LIMIT: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// BodyStream — bridges `http_body::Body` → `futures::Stream` for `multer`
// ---------------------------------------------------------------------------

/// Wraps an `http_body::Body` into a `futures::Stream<Item = Result<Bytes, io::Error>>`
/// so that `multer::Multipart` can consume it.
pub struct BodyStream<B> {
    inner: B,
}

impl<B> Stream for BodyStream<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    Poll::Ready(Some(Ok(data)))
                } else {
                    // Non-data frames (e.g. trailers) — re-poll
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(std::io::Error::other(e.to_string()))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// UploadedFile — a fully-buffered file upload result
// ---------------------------------------------------------------------------

/// Represents a fully-buffered uploaded file extracted from a multipart field.
#[derive(Debug, Clone)]
pub struct UploadedFile {
    /// The form field name (e.g. `"avatar"`).
    pub field_name: String,
    /// The original filename sent by the client (e.g. `"photo.jpg"`).
    pub file_name: Option<String>,
    /// The `Content-Type` of the uploaded file (e.g. `"image/png"`).
    pub content_type: Option<String>,
    /// The raw bytes of the file.
    pub data: Bytes,
}

impl UploadedFile {
    /// Returns the size of the uploaded file in bytes.
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

// ---------------------------------------------------------------------------
// Multipart — the main extractor type
// ---------------------------------------------------------------------------

/// A convenience extractor for `multipart/form-data` requests.
///
/// This type wraps `multer::Multipart` and provides ergonomic helpers for
/// extracting text fields and uploaded files with size-limit enforcement.
///
/// Implements `FromRequest` so it can be used directly in Axon handlers.
pub struct Multipart {
    inner: MulterMultipart<'static>,
    field_size_limit: usize,
}

impl Multipart {
    /// Create a new `Multipart` from a raw `multer::Multipart` instance.
    pub fn new(inner: MulterMultipart<'static>) -> Self {
        Self {
            inner,
            field_size_limit: DEFAULT_FIELD_LIMIT,
        }
    }

    /// Override the per-field size limit (default: 2 MB).
    pub fn with_field_size_limit(mut self, limit: usize) -> Self {
        self.field_size_limit = limit;
        self
    }

    /// Try to get the next raw `multer::Field` from the multipart stream.
    pub async fn next_field(&mut self) -> Result<Option<multer::Field<'static>>, ExtractError> {
        self.inner
            .next_field()
            .await
            .map_err(|e| ExtractError::MultipartError(e.to_string()))
    }

    /// Consume all remaining fields and collect them into text fields and uploaded files.
    ///
    /// Text fields (those without a filename) are returned as `(name, value)` pairs.
    /// File fields (those with a filename) are returned as `UploadedFile` structs.
    ///
    /// Each field's content is limited by the configured `field_size_limit`.
    pub async fn collect_all(
        &mut self,
    ) -> Result<(Vec<(String, String)>, Vec<UploadedFile>), ExtractError> {
        let mut text_fields = Vec::new();
        let mut files = Vec::new();
        let limit = self.field_size_limit;

        while let Some(field) = self.next_field().await? {
            let name = field.name().unwrap_or("").to_string();
            let file_name = field.file_name().map(|s| s.to_string());
            let content_type = field.content_type().map(|m| m.to_string());

            let data = field
                .bytes()
                .await
                .map_err(|e| ExtractError::MultipartError(e.to_string()))?;

            if data.len() > limit {
                return Err(ExtractError::BodyTooLarge {
                    limit,
                    actual: data.len(),
                });
            }

            if file_name.is_some() {
                files.push(UploadedFile {
                    field_name: name,
                    file_name,
                    content_type,
                    data,
                });
            } else {
                let value = String::from_utf8(data.to_vec()).map_err(|e| {
                    ExtractError::MultipartError(format!("non-UTF8 text field '{name}': {e}"))
                })?;
                text_fields.push((name, value));
            }
        }

        Ok((text_fields, files))
    }

    /// Convenience: extract only the text fields, ignoring file uploads.
    pub async fn collect_text_fields(&mut self) -> Result<Vec<(String, String)>, ExtractError> {
        let (text, _) = self.collect_all().await?;
        Ok(text)
    }

    /// Convenience: extract only the uploaded files, ignoring text fields.
    pub async fn collect_files(&mut self) -> Result<Vec<UploadedFile>, ExtractError> {
        let (_, files) = self.collect_all().await?;
        Ok(files)
    }
}

// ---------------------------------------------------------------------------
// FromRequest implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl<B> FromRequest<B> for Multipart
where
    B: Body<Data = Bytes> + Send + Unpin + Default + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let content_type = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|val| val.to_str().ok())
            .ok_or_else(|| ExtractError::MultipartError("missing content-type header".into()))?;

        let boundary = multer::parse_boundary(content_type).map_err(|_| {
            ExtractError::MultipartError("no multipart boundary found in content-type".into())
        })?;

        // Take the body out of the request so we own it ('static lifetime).
        let body = std::mem::take(req.body_mut());

        let size_limit = multer::SizeLimit::new()
            .whole_stream(DEFAULT_MULTIPART_LIMIT as u64)
            .per_field(DEFAULT_FIELD_LIMIT as u64);

        let constraints = multer::Constraints::new().size_limit(size_limit);

        let stream = BodyStream { inner: body };
        let inner = MulterMultipart::with_constraints(stream, boundary, constraints);

        Ok(Multipart::new(inner))
    }
}
