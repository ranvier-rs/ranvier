// Actual catalog string with the real demo schema and Transition.
#[rustfmt::skip]
#[tokio::test]
async fn public_graphql_catalog_preserves_envelope() -> anyhow::Result<()> {
    let schema = demo_schema();
    let axon = Axon::<String, String, String>::new("catalog-graphql").then(ExecuteGraphQl);
    let mut bus = Bus::new();
bus.insert(GraphQlSchema(schema));
let report = axon.execute(
    "{ users { id name } }".into(),
    &(), &mut bus
).await;
// Local workflow: preserve GraphQL data/errors separately from durability.
if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
    anyhow::bail!("Execution requires reconciliation before retry");
}
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(response), .. } => {
        assert_eq!(response, serde_json::json!({ "data": { "users": [{ "id": 1, "name": "Alice" }, { "id": 2, "name": "Bob" }] } }));
    }
    _ => anyhow::bail!("Execution requires reconciliation before retry"),
}
    Ok(())
}
