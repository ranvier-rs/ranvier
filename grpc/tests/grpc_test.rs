//! Integration tests for ranvier-grpc crate.

#[cfg(test)]
mod tests {
    use ranvier_grpc::error::GrpcError;
    use ranvier_grpc::extract::{GrpcContext, extract_metadata};
    use ranvier_grpc::response::IntoGrpcResponse;
    use ranvier_grpc::stream;
    use tonic::Status;
    use tonic::metadata::MetadataMap;

    // -----------------------------------------------------------------------
    // Error mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn grpc_error_maps_to_correct_status_codes() {
        let cases: Vec<(GrpcError, tonic::Code)> = vec![
            (
                GrpcError::InvalidArgument("bad".into()),
                tonic::Code::InvalidArgument,
            ),
            (GrpcError::NotFound("missing".into()), tonic::Code::NotFound),
            (
                GrpcError::PermissionDenied("no".into()),
                tonic::Code::PermissionDenied,
            ),
            (
                GrpcError::Unauthenticated("who".into()),
                tonic::Code::Unauthenticated,
            ),
            (GrpcError::Internal("oops".into()), tonic::Code::Internal),
            (
                GrpcError::Unimplemented("todo".into()),
                tonic::Code::Unimplemented,
            ),
            (
                GrpcError::Unavailable("down".into()),
                tonic::Code::Unavailable,
            ),
            (
                GrpcError::DeadlineExceeded("slow".into()),
                tonic::Code::DeadlineExceeded,
            ),
            (
                GrpcError::AlreadyExists("dup".into()),
                tonic::Code::AlreadyExists,
            ),
            (
                GrpcError::FailedPrecondition("pre".into()),
                tonic::Code::FailedPrecondition,
            ),
        ];

        for (error, expected_code) in cases {
            let status: Status = error.into();
            assert_eq!(status.code(), expected_code);
        }
    }

    #[test]
    fn grpc_error_preserves_message() {
        let error = GrpcError::NotFound("user 42 not found".into());
        let status: Status = error.into();
        assert_eq!(status.message(), "user 42 not found");
    }

    // -----------------------------------------------------------------------
    // Response conversion tests
    // -----------------------------------------------------------------------

    #[test]
    fn ok_result_converts_to_grpc_response() {
        let result: Result<String, GrpcError> = Ok("success".into());
        let response = IntoGrpcResponse::<String>::into_grpc_response(result).unwrap();
        assert_eq!(response.get_ref(), "success");
    }

    #[test]
    fn err_result_converts_to_grpc_status() {
        let result: Result<String, GrpcError> = Err(GrpcError::NotFound("gone".into()));
        let status = IntoGrpcResponse::<String>::into_grpc_response(result).unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    // -----------------------------------------------------------------------
    // Metadata extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn extract_metadata_captures_ascii_entries() {
        let mut map = MetadataMap::new();
        map.insert("x-request-id", "abc-123".parse().unwrap());
        map.insert("authorization", "Bearer tok_xyz".parse().unwrap());

        let extracted = extract_metadata(&map);
        assert_eq!(extracted.get("x-request-id").unwrap(), "abc-123");
        assert_eq!(extracted.get("authorization").unwrap(), "Bearer tok_xyz");
    }

    #[test]
    fn grpc_context_exposes_authorization() {
        let mut map = MetadataMap::new();
        map.insert("authorization", "Bearer my-token".parse().unwrap());

        let mut request = tonic::Request::new(());
        *request.metadata_mut() = map;

        let ctx = GrpcContext::from_request(&request);
        assert_eq!(ctx.authorization(), Some("Bearer my-token"));
    }

    // -----------------------------------------------------------------------
    // Streaming tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn from_iter_produces_stream_items() {
        use futures_util::StreamExt;

        let items = vec![Ok(1i32), Ok(2), Ok(3)];
        let mut s = stream::from_iter(items);

        let v1 = s.next().await.unwrap().unwrap();
        let v2 = s.next().await.unwrap().unwrap();
        let v3 = s.next().await.unwrap().unwrap();
        assert_eq!((v1, v2, v3), (1, 2, 3));
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_event_stream_produces_items() {
        use futures_util::StreamExt;

        let mut s = stream::from_event_stream(4, |tx| async move {
            for i in 0..3 {
                let _ = tx.send(Ok(i)).await;
            }
        });

        let v1 = s.next().await.unwrap().unwrap();
        let v2 = s.next().await.unwrap().unwrap();
        let v3 = s.next().await.unwrap().unwrap();
        assert_eq!((v1, v2, v3), (0, 1, 2));
    }
}
