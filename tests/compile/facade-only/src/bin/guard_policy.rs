use ranvier::prelude::*;

fn verify_guard_policy_provider_contract(config: &ResolvedRuntimeConfig) {
    let guard = RateLimitGuard::<String>::new(100, 60_000)
        .with_bucket_ttl(std::time::Duration::from_secs(15 * 60));
    let _report = config.validate_startup(&[&guard]);
}

fn main() {
    let _provider_contract: fn(&ResolvedRuntimeConfig) = verify_guard_policy_provider_contract;
}
