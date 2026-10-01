// Actual exported handler with the demo's named types and policy helpers.
#[rustfmt::skip]
mod public_catalog {
    use super::*;
// Greeter::say_hello delegates to this handler.
// validate_name and map_local_report are defined in grpc-tonic-demo.
async fn handle_hello(
    axon: &Axon<GreetInput, GreetOutput, String>,
    request: Request<HelloRequest>,
) -> Result<Response<HelloReply>, Status> {
    let name = request.into_inner().name;
    validate_name(&name)?;
    let mut bus = Bus::new();
    let report = axon.execute(GreetInput { name }, &(), &mut bus).await;
    // Local Unpersisted + Next only; reject incomplete cleanup and
    // uncertain durability with fixed statuses, without automatic retry.
    map_local_report(report).map(Response::new)
}
    #[tokio::test]
    async fn actual_catalog_handler_validates_and_maps_reply() {
        let axon = Axon::<GreetInput, GreetInput, String>::new("catalog-greeting").then(BuildGreeting);
        let response = handle_hello(&axon, Request::new(HelloRequest { name: "Catalog".into() })).await.unwrap();
        assert_eq!(response.into_inner().message, "Hello, Catalog! (from Ranvier Axon pipeline)");
        let status = handle_hello(&axon, Request::new(HelloRequest { name: " ".into() })).await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
