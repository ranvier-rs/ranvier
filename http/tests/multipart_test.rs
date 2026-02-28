//! Integration tests for the multipart extractor.

#![cfg(feature = "multer")]

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use ranvier_http::extract::multipart::Multipart;
use ranvier_http::extract::FromRequest;

fn multipart_body(boundary: &str, fields: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, data) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        if let Some(fname) = filename {
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(b"Content-Type: application/octet-stream\r\n");
        } else {
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
            );
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

#[tokio::test]
async fn multipart_extracts_text_fields() {
    let boundary = "----TestBoundary123";
    let raw = multipart_body(
        boundary,
        &[
            ("username", None, b"ranvier"),
            ("email", None, b"dev@ranvier.dev"),
        ],
    );

    let mut req = Request::builder()
        .uri("/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Full::new(Bytes::from(raw)))
        .unwrap();

    let mut mp = Multipart::from_request(&mut req).await.unwrap();
    let (text_fields, files) = mp.collect_all().await.unwrap();

    assert_eq!(text_fields.len(), 2);
    assert_eq!(text_fields[0], ("username".into(), "ranvier".into()));
    assert_eq!(text_fields[1], ("email".into(), "dev@ranvier.dev".into()));
    assert!(files.is_empty());
}

#[tokio::test]
async fn multipart_extracts_file_uploads() {
    let boundary = "----FileBoundary456";
    let file_data = b"hello world file content";
    let raw = multipart_body(
        boundary,
        &[("document", Some("readme.txt"), file_data)],
    );

    let mut req = Request::builder()
        .uri("/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Full::new(Bytes::from(raw)))
        .unwrap();

    let mut mp = Multipart::from_request(&mut req).await.unwrap();
    let files = mp.collect_files().await.unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].field_name, "document");
    assert_eq!(files[0].file_name, Some("readme.txt".into()));
    assert_eq!(files[0].data, Bytes::from_static(file_data));
    assert_eq!(files[0].size(), file_data.len());
}

#[tokio::test]
async fn multipart_extracts_mixed_fields_and_files() {
    let boundary = "----MixedBoundary789";
    let raw = multipart_body(
        boundary,
        &[
            ("title", None, b"My Document"),
            ("attachment", Some("data.bin"), &[0xDE, 0xAD, 0xBE, 0xEF]),
            ("description", None, b"A test upload"),
        ],
    );

    let mut req = Request::builder()
        .uri("/upload")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Full::new(Bytes::from(raw)))
        .unwrap();

    let mut mp = Multipart::from_request(&mut req).await.unwrap();
    let (text_fields, files) = mp.collect_all().await.unwrap();

    assert_eq!(text_fields.len(), 2);
    assert_eq!(text_fields[0].0, "title");
    assert_eq!(text_fields[1].0, "description");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].field_name, "attachment");
    assert_eq!(files[0].file_name, Some("data.bin".into()));
    assert_eq!(files[0].data, Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[tokio::test]
async fn multipart_rejects_missing_boundary() {
    let mut req = Request::builder()
        .uri("/upload")
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = Multipart::from_request(&mut req).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn multipart_rejects_missing_content_type() {
    let mut req = Request::builder()
        .uri("/upload")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = Multipart::from_request(&mut req).await;
    assert!(result.is_err());
}
