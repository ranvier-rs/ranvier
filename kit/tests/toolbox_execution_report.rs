use ranvier::prelude::*;

#[derive(Clone, serde::Deserialize)]
struct BranchRequest {
    category: String,
}

impl serde::Serialize for BranchRequest {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("private-toolbox-serializer"))
    }
}

#[transition]
async fn route_by_type(input: BranchRequest) -> Outcome<String, String> {
    match input.category.as_str() {
        "urgent" => match serde_json::to_value(&input) {
            Ok(payload) => Outcome::Branch("urgent_path".into(), Some(payload)),
            Err(_) => Outcome::Fault("Branch input could not be encoded".to_string()),
        },
        "normal" => Outcome::Next(input.category),
        _ => Outcome::Fault("Unknown category".into()),
    }
}

#[tokio::test]
async fn toolbox_branch_encoding_failure_is_explicit_fault() {
    let mut bus = Bus::new();
    let outcome = route_by_type.run(BranchRequest { category: "urgent".into() }, &(), &mut bus).await;
    assert!(matches!(outcome, Outcome::Fault(ref error) if error == "Branch input could not be encoded"));
    assert!(!format!("{outcome:?}").contains("private-toolbox-serializer"));
}

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
