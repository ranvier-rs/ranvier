use ranvier::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let profile = "production".parse::<RuntimeProfile>()?;
    let _load_for: fn(RuntimeProfile) -> Result<ResolvedRuntimeConfig, ResolvedConfigError> =
        ResolvedRuntimeConfig::load_for;
    assert_eq!(profile, RuntimeProfile::Production);
    Ok(())
}
