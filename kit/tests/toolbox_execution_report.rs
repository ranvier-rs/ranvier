use ranvier::prelude::*;

#[transition]
async fn validate(input: String) -> Outcome<String, String> {
    Outcome::Next(input)
}

#[transition]
async fn transform(input: String) -> Outcome<String, String> {
    Outcome::Next(input.to_uppercase())
}

#[transition]
async fn persist(input: String) -> Outcome<String, String> {
    Outcome::Next(input)
}

/// Instantiates the VSCode multi-step toolbox placeholders against the facade.
#[tokio::test]
async fn toolbox_multi_step_returns_inspectable_report() {
    let axon = Axon::<String, String, String>::new("toolbox-report")
        .then(validate)
        .then(transform)
        .then(persist);
    let mut bus = Bus::new();
    let report = axon.execute("sample".to_string(), &(), &mut bus).await;
    let rendered = format!("Execution report: {:?}", report);
    assert!(rendered.contains("Unpersisted"));
    match report {
        ranvier::ExecutionReport::Unpersisted { result: Outcome::Next(output), .. } => {
            assert_eq!(output, "SAMPLE");
        }
        other => panic!("unexpected toolbox report: {other:?}"),
    }
}
