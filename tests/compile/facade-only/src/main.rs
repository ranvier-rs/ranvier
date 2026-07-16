use ranvier::prelude::*;

#[transition]
async fn greet(_input: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    Outcome::next("Hello, Ranvier!".to_string())
}

fn verify_resolved_profile_contract() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let profile = "production".parse::<RuntimeProfile>()?;
    let _load_for: fn(RuntimeProfile) -> Result<ResolvedRuntimeConfig, ResolvedConfigError> =
        ResolvedRuntimeConfig::load_for;
    assert_eq!(profile, RuntimeProfile::Production);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    verify_resolved_profile_contract()?;
    let hello = Axon::<(), (), String>::new("hello").then(greet);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await?;

    Ok(())
}
