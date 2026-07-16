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

fn verify_guard_policy_provider_contract(config: &ResolvedRuntimeConfig) {
    let guard = RateLimitGuard::<String>::new(100, 60_000)
        .with_bucket_ttl(std::time::Duration::from_secs(15 * 60));
    let _report = config.validate_startup(&[&guard]);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    verify_resolved_profile_contract()?;
    let _provider_contract: fn(&ResolvedRuntimeConfig) = verify_guard_policy_provider_contract;
    let hello = Axon::<(), (), String>::new("hello").then(greet);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await?;

    Ok(())
}
