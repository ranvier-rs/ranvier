use ranvier_core::schematic::Schematic;
use ranvier_inspector::{
    Inspector,
    relay::{RelayConfig, RelayConfigError, RelayState},
};

// These functions compile in a distinct crate and bind public Result signatures.
// They are not run: environment-dependent startup is tested separately.
fn configure(target: String) -> Result<Inspector, RelayConfigError> {
    Inspector::new(Schematic::default(), 9090).with_relay_target(target)
}
fn config(target: String) -> Result<RelayConfig, RelayConfigError> {
    RelayConfig::new(target)
}

#[test]
fn external_consumer_can_handle_all_fallible_startup_signatures() {
    let _: fn(String) -> Result<Inspector, RelayConfigError> = configure;
    let _: fn(String) -> Result<RelayConfig, RelayConfigError> = config;
    let _: fn(RelayConfig) -> Result<RelayState, RelayConfigError> = RelayState::new;
}

#[test]
fn direct_invalid_configuration_returns_public_sanitized_error() {
    let result = RelayState::new(RelayConfig {
        target_url: "http://user:SENTINEL@localhost".into(),
        timeout_ms: 1,
        max_concurrent: 1,
    });
    match result {
        Err(error) => {
            assert_eq!(error, RelayConfigError::InvalidTarget);
            assert!(!error.to_string().contains("SENTINEL"));
        }
        Ok(_) => panic!("credential-bearing target accepted"),
    }
}
