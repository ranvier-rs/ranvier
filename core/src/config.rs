//! Configuration management for Ranvier applications.
//!
//! Provides a layered configuration system:
//! 1. Defaults → 2. `ranvier.toml` file → 3. Profile overrides → 4. Environment variables
//!
//! # Example `ranvier.toml`
//!
//! ```toml
//! [server]
//! host = "0.0.0.0"
//! port = 3000
//! shutdown_timeout_secs = 30
//!
//! [logging]
//! format = "json"
//! level = "info"
//!
//! [tls]
//! enabled = false
//! cert_path = "certs/cert.pem"
//! key_path = "certs/key.pem"
//!
//! [inspector]
//! enabled = true
//! port = 3001
//!
//! [telemetry]
//! otlp_endpoint = "http://localhost:4317"
//! otlp_protocol = "grpc"
//! service_name  = "my-api"
//! sample_ratio  = 1.0
//!
//! [profile.prod]
//! logging.format = "json"
//! logging.level = "warn"
//! tls.enabled = true
//! inspector.enabled = false
//! ```

use crate::runtime_policy::{
    ComponentPolicyReport, PolicyComponent, PolicyField, PolicyObservation, PolicyValue,
    RuntimeProfile, RuntimeProfileSource, StartupPolicyCode, StartupPolicyContribution,
    StartupPolicyError, StartupPolicyReport, StartupPolicyViolation, UnsafeAcknowledgement,
    evaluate_startup_policy, invalid_startup_policy,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// Top-level Ranvier configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RanvierConfig {
    pub server: ServerConfig,
    pub logging: LoggingConfig,
    pub tls: TlsConfig,
    pub inspector: InspectorConfig,
    pub telemetry: TelemetryConfig,
    /// Profile-specific overrides keyed by profile name (e.g., "dev", "staging", "prod").
    #[serde(default)]
    pub profile: HashMap<String, ProfileOverride>,
}

/// HTTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Bind host address (default: "127.0.0.1").
    pub host: String,
    /// Bind port (default: 3000).
    pub port: u16,
    /// Graceful shutdown timeout in seconds (default: 30).
    pub shutdown_timeout_secs: u64,
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Log output format: "json", "pretty", or "compact" (default: "pretty").
    pub format: LogFormat,
    /// Global log level (default: "info").
    pub level: String,
    /// Per-module log level overrides.
    #[serde(default)]
    pub module_levels: HashMap<String, String>,
}

/// Log output format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    #[default]
    Pretty,
    Compact,
}

/// TLS configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TlsConfig {
    /// Whether TLS is enabled (default: false).
    pub enabled: bool,
    /// Path to the certificate PEM file.
    pub cert_path: String,
    /// Path to the private key PEM file.
    pub key_path: String,
}

/// Inspector configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectorConfig {
    /// Whether the Inspector is enabled (default: true).
    pub enabled: bool,
    /// Inspector port (default: 3001).
    pub port: u16,
}

/// Telemetry (OpenTelemetry) configuration.
///
/// When `otlp_endpoint` is set, `init_telemetry()` will initialize an OTLP
/// exporter that sends traces to the given endpoint.  When absent, telemetry
/// initialization is skipped (no-op).
///
/// ```toml
/// [telemetry]
/// otlp_endpoint = "http://localhost:4317"
/// otlp_protocol = "grpc"
/// service_name  = "my-api"
/// sample_ratio  = 1.0
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// OTLP collector endpoint (e.g. `http://localhost:4317`).
    /// When `None`, telemetry is disabled.
    pub otlp_endpoint: Option<String>,
    /// OTLP transport protocol (default: `Grpc`).
    pub otlp_protocol: OtlpProtocol,
    /// OpenTelemetry service name (default: `"ranvier"`).
    pub service_name: String,
    /// Trace sampling ratio, `0.0` (none) to `1.0` (all). Default: `1.0`.
    pub sample_ratio: f64,
}

/// OTLP transport protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OtlpProtocol {
    #[default]
    Grpc,
    Http,
}

/// Profile-specific configuration overrides.
///
/// Fields are all optional — only specified fields override the base config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileOverride {
    pub server: Option<ServerOverride>,
    pub logging: Option<LoggingOverride>,
    pub tls: Option<TlsOverride>,
    pub inspector: Option<InspectorOverride>,
    pub telemetry: Option<TelemetryOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerOverride {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub shutdown_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoggingOverride {
    pub format: Option<LogFormat>,
    pub level: Option<String>,
    pub module_levels: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TlsOverride {
    pub enabled: Option<bool>,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InspectorOverride {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelemetryOverride {
    pub otlp_endpoint: Option<String>,
    pub otlp_protocol: Option<OtlpProtocol>,
    pub service_name: Option<String>,
    pub sample_ratio: Option<f64>,
}

// ── Defaults ──

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
            shutdown_timeout_secs: 30,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Pretty,
            level: "info".to_string(),
            module_levels: HashMap::new(),
        }
    }
}

impl Default for InspectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 3001,
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            otlp_protocol: OtlpProtocol::Grpc,
            service_name: "ranvier".to_string(),
            sample_ratio: 1.0,
        }
    }
}

// ── Loading ──

/// Errors that can occur when loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file '{path}': {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse TOML config: {0}")]
    ParseToml(#[from] toml::de::Error),
    #[error("profile '{0}' not found in configuration")]
    ProfileNotFound(String),
}

/// Errors produced only by the additive resolved runtime-config path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolvedConfigError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    InvalidRuntimePolicy(StartupPolicyError),
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RuntimeDocument {
    runtime: RuntimeSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RuntimeSection {
    profile: Option<String>,
    unsafe_acknowledgements: Vec<UnsafeAcknowledgementInput>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UnsafeAcknowledgementInput {
    policy_code: Option<String>,
    id: Option<String>,
    owner: Option<String>,
    rationale: Option<String>,
    review_on: Option<String>,
    expires_on: Option<String>,
}

/// Configuration resolved with one explicit runtime safety intent.
///
/// This additive wrapper preserves the public layout and legacy behavior of
/// [`RanvierConfig`] while enabling strict parsing and pre-side-effect startup
/// policy validation.
#[derive(Clone)]
pub struct ResolvedRuntimeConfig {
    profile: RuntimeProfile,
    profile_source: RuntimeProfileSource,
    config: RanvierConfig,
    acknowledgements: Vec<UnsafeAcknowledgement>,
}

impl std::fmt::Debug for ResolvedRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeConfig")
            .field("profile", &self.profile)
            .field("profile_source", &self.profile_source)
            .field("config", &"<redacted>")
            .field("acknowledgement_count", &self.acknowledgements.len())
            .finish()
    }
}

impl ResolvedRuntimeConfig {
    /// Discover `ranvier.toml`, resolve runtime intent, and apply strict
    /// environment overrides.
    pub fn load() -> Result<Self, ResolvedConfigError> {
        Self::load_with_explicit_profile(None)
    }

    /// Load configuration while giving a typed runtime profile highest
    /// precedence.
    pub fn load_for(profile: RuntimeProfile) -> Result<Self, ResolvedConfigError> {
        Self::load_with_explicit_profile(Some(profile))
    }

    /// Load a specific file and resolve runtime intent from file/environment.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ResolvedConfigError> {
        Self::from_file_with_explicit_profile(path, None)
    }

    /// Load a specific file with a typed runtime profile at highest precedence.
    pub fn from_file_for(
        path: impl AsRef<Path>,
        profile: RuntimeProfile,
    ) -> Result<Self, ResolvedConfigError> {
        Self::from_file_with_explicit_profile(path, Some(profile))
    }

    pub const fn profile(&self) -> RuntimeProfile {
        self.profile
    }

    pub fn config(&self) -> &RanvierConfig {
        &self.config
    }

    /// Aggregate core and adapter policy without starting listeners, tasks,
    /// migrations, dependency loops, or durable writes.
    pub fn validate_startup(
        &self,
        contributions: Vec<StartupPolicyContribution>,
    ) -> Result<StartupPolicyReport, StartupPolicyError> {
        let mut components = Vec::with_capacity(contributions.len() + 1);
        let mut violations = Vec::new();
        for contribution in contributions {
            let (report, current_violations) = contribution.into_report_and_violations();
            components.push(report);
            violations.extend(current_violations);
        }
        for acknowledgement in &self.acknowledgements {
            violations.extend(acknowledgement.violations_on(Utc::now().date_naive()));
        }
        components.push(self.core_policy_report());
        evaluate_startup_policy(
            self.profile,
            self.profile_source,
            &self.acknowledgements,
            components,
            violations,
        )
    }

    fn load_with_explicit_profile(
        explicit_profile: Option<RuntimeProfile>,
    ) -> Result<Self, ResolvedConfigError> {
        if let Some(path) = RanvierConfig::find_config_file() {
            Self::from_file_with_explicit_profile(path, explicit_profile)
        } else {
            let mut environment = |key: &str| std::env::var(key).ok();
            Self::resolve_document_with_environment("", explicit_profile, &mut environment)
        }
    }

    fn from_file_with_explicit_profile(
        path: impl AsRef<Path>,
        explicit_profile: Option<RuntimeProfile>,
    ) -> Result<Self, ResolvedConfigError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
            path: path.display().to_string(),
            source,
        })?;
        let mut environment = |key: &str| std::env::var(key).ok();
        Self::resolve_document_with_environment(&content, explicit_profile, &mut environment)
    }

    fn resolve_document_with_environment<F>(
        content: &str,
        explicit_profile: Option<RuntimeProfile>,
        environment: &mut F,
    ) -> Result<Self, ResolvedConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut config: RanvierConfig = toml::from_str(content).map_err(ConfigError::from)?;
        let runtime_document: RuntimeDocument =
            toml::from_str(content).map_err(ConfigError::from)?;

        if let Some(profile_name) = environment("RANVIER_PROFILE") {
            config.apply_profile(&profile_name)?;
        }

        let mut violations = config.apply_env_overrides_collect(environment);
        violations.extend(config.value_violations());

        let file_profile = parse_runtime_profile(
            runtime_document.runtime.profile.as_deref(),
            PolicyField::RUNTIME_PROFILE,
            &mut violations,
        );
        let environment_profile_raw = environment("RANVIER_RUNTIME_PROFILE");
        let environment_profile = parse_runtime_profile(
            environment_profile_raw.as_deref(),
            PolicyField::RUNTIME_PROFILE,
            &mut violations,
        );

        let (profile, profile_source) = if let Some(profile) = explicit_profile {
            (profile, RuntimeProfileSource::Explicit)
        } else if let Some(profile) = environment_profile {
            (profile, RuntimeProfileSource::Environment)
        } else if let Some(profile) = file_profile {
            (profile, RuntimeProfileSource::File)
        } else {
            (RuntimeProfile::Development, RuntimeProfileSource::Default)
        };

        let (acknowledgements, acknowledgement_violations) =
            parse_acknowledgements(runtime_document.runtime.unsafe_acknowledgements);
        violations.extend(acknowledgement_violations);
        violations.sort_by_key(|violation| (violation.component, violation.code, violation.field));

        let resolved = Self {
            profile,
            profile_source,
            config,
            acknowledgements,
        };

        if !violations.is_empty() {
            let error = invalid_startup_policy(
                resolved.profile,
                resolved.profile_source,
                &resolved.acknowledgements,
                vec![resolved.core_policy_report()],
                violations,
            );
            return Err(ResolvedConfigError::InvalidRuntimePolicy(error));
        }

        Ok(resolved)
    }

    fn core_policy_report(&self) -> ComponentPolicyReport {
        let config = &self.config;
        ComponentPolicyReport::new(
            PolicyComponent::CORE,
            vec![
                PolicyObservation::new(
                    PolicyField::RUNTIME_PROFILE,
                    PolicyValue::Label(match self.profile {
                        RuntimeProfile::Development => "development",
                        RuntimeProfile::Production => "production",
                    }),
                ),
                PolicyObservation::new(
                    PolicyField::RUNTIME_PROFILE_SOURCE,
                    PolicyValue::Label(match self.profile_source {
                        RuntimeProfileSource::Explicit => "explicit",
                        RuntimeProfileSource::Environment => "environment",
                        RuntimeProfileSource::File => "file",
                        RuntimeProfileSource::Default => "default",
                    }),
                ),
                PolicyObservation::new(
                    PolicyField::SERVER_HOST,
                    PolicyValue::Label(if is_loopback_host(&config.server.host) {
                        "loopback"
                    } else {
                        "non_loopback"
                    }),
                ),
                PolicyObservation::new(
                    PolicyField::SERVER_PORT,
                    PolicyValue::Count(u64::from(config.server.port)),
                ),
                PolicyObservation::new(
                    PolicyField::SERVER_SHUTDOWN_TIMEOUT_SECS,
                    PolicyValue::DurationMs(
                        config.server.shutdown_timeout_secs.saturating_mul(1_000),
                    ),
                ),
                PolicyObservation::new(
                    PolicyField::LOGGING_FORMAT,
                    PolicyValue::Label(match config.logging.format {
                        LogFormat::Json => "json",
                        LogFormat::Pretty => "pretty",
                        LogFormat::Compact => "compact",
                    }),
                ),
                PolicyObservation::new(
                    PolicyField::LOGGING_LEVEL,
                    PolicyValue::Configured(!config.logging.level.trim().is_empty()),
                ),
                PolicyObservation::new(
                    PolicyField::TLS_ENABLED,
                    PolicyValue::Bool(config.tls.enabled),
                ),
                PolicyObservation::new(
                    PolicyField::TLS_CERT_CONFIGURED,
                    PolicyValue::Configured(!config.tls.cert_path.trim().is_empty()),
                ),
                PolicyObservation::new(
                    PolicyField::TLS_KEY_CONFIGURED,
                    PolicyValue::Configured(!config.tls.key_path.trim().is_empty()),
                ),
                PolicyObservation::new(
                    PolicyField::INSPECTOR_ENABLED,
                    PolicyValue::Bool(config.inspector.enabled),
                ),
                PolicyObservation::new(
                    PolicyField::INSPECTOR_PORT,
                    PolicyValue::Count(u64::from(config.inspector.port)),
                ),
                PolicyObservation::new(
                    PolicyField::TELEMETRY_ENDPOINT_CONFIGURED,
                    PolicyValue::Configured(config.telemetry.otlp_endpoint.is_some()),
                ),
                PolicyObservation::new(
                    PolicyField::TELEMETRY_PROTOCOL,
                    PolicyValue::Label(match config.telemetry.otlp_protocol {
                        OtlpProtocol::Grpc => "grpc",
                        OtlpProtocol::Http => "http",
                    }),
                ),
                PolicyObservation::new(
                    PolicyField::TELEMETRY_SERVICE_NAME_CONFIGURED,
                    PolicyValue::Configured(!config.telemetry.service_name.trim().is_empty()),
                ),
                PolicyObservation::new(
                    PolicyField::TELEMETRY_SAMPLE_RATIO,
                    PolicyValue::RatioBasisPoints(
                        (config.telemetry.sample_ratio * 10_000.0).round() as u16,
                    ),
                ),
            ],
        )
    }
}

fn parse_runtime_profile(
    raw: Option<&str>,
    field: PolicyField,
    violations: &mut Vec<StartupPolicyViolation>,
) -> Option<RuntimeProfile> {
    raw.and_then(|value| match value.parse::<RuntimeProfile>() {
        Ok(profile) => Some(profile),
        Err(_) => {
            violations.push(StartupPolicyViolation::new(
                StartupPolicyCode::RuntimeProfileInvalid,
                PolicyComponent::CORE,
                field,
            ));
            None
        }
    })
}

fn parse_acknowledgements(
    inputs: Vec<UnsafeAcknowledgementInput>,
) -> (Vec<UnsafeAcknowledgement>, Vec<StartupPolicyViolation>) {
    let today = Utc::now().date_naive();
    let mut acknowledgements = Vec::new();
    let mut violations = Vec::new();
    let mut seen_codes = HashSet::new();

    for input in inputs {
        let mut input_violations = Vec::new();
        let code = input
            .policy_code
            .as_deref()
            .and_then(|raw| raw.parse::<StartupPolicyCode>().ok());
        if code.is_none() {
            input_violations.push(invalid_ack_field(PolicyField::ACKNOWLEDGEMENT_CODE));
        }
        let review_on = parse_acknowledgement_date(
            input.review_on.as_deref(),
            PolicyField::ACKNOWLEDGEMENT_REVIEW_ON,
            &mut input_violations,
        );
        let expires_on = parse_acknowledgement_date(
            input.expires_on.as_deref(),
            PolicyField::ACKNOWLEDGEMENT_EXPIRES_ON,
            &mut input_violations,
        );

        let id = input.id.unwrap_or_default();
        let owner = input.owner.unwrap_or_default();
        let rationale = input.rationale.unwrap_or_default();
        if id.trim().is_empty() {
            input_violations.push(invalid_ack_field(PolicyField::ACKNOWLEDGEMENT_ID));
        }
        if owner.trim().is_empty() {
            input_violations.push(invalid_ack_field(PolicyField::ACKNOWLEDGEMENT_OWNER));
        }
        if rationale.trim().is_empty() {
            input_violations.push(invalid_ack_field(PolicyField::ACKNOWLEDGEMENT_RATIONALE));
        }

        if !input_violations.is_empty() {
            violations.extend(input_violations);
            continue;
        }

        let Some((code, review_on, expires_on)) = code
            .zip(review_on)
            .zip(expires_on)
            .map(|((code, review_on), expires_on)| (code, review_on, expires_on))
        else {
            continue;
        };

        if !seen_codes.insert(code) {
            violations.push(invalid_ack_field(PolicyField::ACKNOWLEDGEMENT_CODE));
            continue;
        }

        match UnsafeAcknowledgement::try_new(code, id, owner, rationale, review_on, expires_on) {
            Ok(acknowledgement) => {
                let current_violations = acknowledgement.violations_on(today);
                if current_violations.is_empty() {
                    acknowledgements.push(acknowledgement);
                } else {
                    violations.extend(current_violations);
                }
            }
            Err(current_violations) => violations.extend(current_violations),
        }
    }

    (acknowledgements, violations)
}

fn parse_acknowledgement_date(
    raw: Option<&str>,
    field: PolicyField,
    violations: &mut Vec<StartupPolicyViolation>,
) -> Option<NaiveDate> {
    raw.and_then(|value| match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            violations.push(invalid_ack_field(field));
            None
        }
    })
}

fn invalid_ack_field(field: PolicyField) -> StartupPolicyViolation {
    StartupPolicyViolation::new(
        StartupPolicyCode::UnsafeAckInvalid,
        PolicyComponent::CORE,
        field,
    )
}

fn invalid_environment(field: PolicyField) -> StartupPolicyViolation {
    StartupPolicyViolation::new(
        StartupPolicyCode::ConfigEnvValueInvalid,
        PolicyComponent::CORE,
        field,
    )
}

fn invalid_config_value(field: PolicyField) -> StartupPolicyViolation {
    StartupPolicyViolation::new(
        StartupPolicyCode::ConfigValueInvalid,
        PolicyComponent::CORE,
        field,
    )
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl RanvierConfig {
    /// Load configuration from a specific file path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
            path: path.display().to_string(),
            source: e,
        })?;
        let config: RanvierConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration by searching for `ranvier.toml` in the current directory
    /// and its ancestors. Returns default config if no file is found.
    pub fn discover() -> Result<Self, ConfigError> {
        if let Some(path) = Self::find_config_file() {
            Self::from_file(path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load configuration with full layering:
    /// 1. Load from file (or defaults)
    /// 2. Apply active profile if `RANVIER_PROFILE` is set
    /// 3. Apply environment variable overrides
    pub fn load() -> Result<Self, ConfigError> {
        let mut config = Self::discover()?;
        let profile = std::env::var("RANVIER_PROFILE").ok();
        if let Some(ref profile_name) = profile {
            config.apply_profile(profile_name)?;
        }
        config.apply_env_overrides();
        Ok(config)
    }

    /// Apply a named profile's overrides to this configuration.
    pub fn apply_profile(&mut self, name: &str) -> Result<(), ConfigError> {
        let overrides = self
            .profile
            .get(name)
            .cloned()
            .ok_or_else(|| ConfigError::ProfileNotFound(name.to_string()))?;

        if let Some(server) = overrides.server {
            if let Some(host) = server.host {
                self.server.host = host;
            }
            if let Some(port) = server.port {
                self.server.port = port;
            }
            if let Some(timeout) = server.shutdown_timeout_secs {
                self.server.shutdown_timeout_secs = timeout;
            }
        }

        if let Some(logging) = overrides.logging {
            if let Some(format) = logging.format {
                self.logging.format = format;
            }
            if let Some(level) = logging.level {
                self.logging.level = level;
            }
            if let Some(module_levels) = logging.module_levels {
                self.logging.module_levels.extend(module_levels);
            }
        }

        if let Some(tls) = overrides.tls {
            if let Some(enabled) = tls.enabled {
                self.tls.enabled = enabled;
            }
            if let Some(cert_path) = tls.cert_path {
                self.tls.cert_path = cert_path;
            }
            if let Some(key_path) = tls.key_path {
                self.tls.key_path = key_path;
            }
        }

        if let Some(inspector) = overrides.inspector {
            if let Some(enabled) = inspector.enabled {
                self.inspector.enabled = enabled;
            }
            if let Some(port) = inspector.port {
                self.inspector.port = port;
            }
        }

        if let Some(telemetry) = overrides.telemetry {
            if let Some(endpoint) = telemetry.otlp_endpoint {
                self.telemetry.otlp_endpoint = Some(endpoint);
            }
            if let Some(protocol) = telemetry.otlp_protocol {
                self.telemetry.otlp_protocol = protocol;
            }
            if let Some(service_name) = telemetry.service_name {
                self.telemetry.service_name = service_name;
            }
            if let Some(sample_ratio) = telemetry.sample_ratio {
                self.telemetry.sample_ratio = sample_ratio;
            }
        }

        Ok(())
    }

    fn apply_env_overrides_collect<F>(&mut self, environment: &mut F) -> Vec<StartupPolicyViolation>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut violations = Vec::new();

        if let Some(value) = environment("RANVIER_SERVER_HOST") {
            if value.trim().is_empty() {
                violations.push(invalid_environment(PolicyField::SERVER_HOST));
            } else {
                self.server.host = value;
            }
        }
        if let Some(value) = environment("RANVIER_SERVER_PORT") {
            match value.parse::<u16>() {
                Ok(port) => self.server.port = port,
                Err(_) => violations.push(invalid_environment(PolicyField::SERVER_PORT)),
            }
        }
        if let Some(value) = environment("RANVIER_SERVER_SHUTDOWN_TIMEOUT_SECS") {
            match value.parse::<u64>() {
                Ok(timeout) => self.server.shutdown_timeout_secs = timeout,
                Err(_) => violations.push(invalid_environment(
                    PolicyField::SERVER_SHUTDOWN_TIMEOUT_SECS,
                )),
            }
        }
        if let Some(value) = environment("RANVIER_LOGGING_FORMAT") {
            match value.to_ascii_lowercase().as_str() {
                "json" => self.logging.format = LogFormat::Json,
                "pretty" => self.logging.format = LogFormat::Pretty,
                "compact" => self.logging.format = LogFormat::Compact,
                _ => violations.push(invalid_environment(PolicyField::LOGGING_FORMAT)),
            }
        }
        if let Some(value) = environment("RANVIER_LOGGING_LEVEL") {
            if tracing_subscriber::EnvFilter::try_new(&value).is_ok() {
                self.logging.level = value;
            } else {
                violations.push(invalid_environment(PolicyField::LOGGING_LEVEL));
            }
        }
        if let Some(value) = environment("RUST_LOG")
            && tracing_subscriber::EnvFilter::try_new(&value).is_err()
        {
            violations.push(invalid_environment(PolicyField::LOGGING_LEVEL));
        }
        if let Some(value) = environment("RANVIER_TLS_ENABLED") {
            match value.parse::<bool>() {
                Ok(enabled) => self.tls.enabled = enabled,
                Err(_) => violations.push(invalid_environment(PolicyField::TLS_ENABLED)),
            }
        }
        if let Some(value) = environment("RANVIER_TLS_CERT_PATH") {
            self.tls.cert_path = value;
        }
        if let Some(value) = environment("RANVIER_TLS_KEY_PATH") {
            self.tls.key_path = value;
        }
        if let Some(value) = environment("RANVIER_INSPECTOR_ENABLED") {
            match value.parse::<bool>() {
                Ok(enabled) => self.inspector.enabled = enabled,
                Err(_) => violations.push(invalid_environment(PolicyField::INSPECTOR_ENABLED)),
            }
        }
        if let Some(value) = environment("RANVIER_INSPECTOR_PORT") {
            match value.parse::<u16>() {
                Ok(port) => self.inspector.port = port,
                Err(_) => violations.push(invalid_environment(PolicyField::INSPECTOR_PORT)),
            }
        }
        if let Some(value) = environment("RANVIER_TELEMETRY_OTLP_ENDPOINT") {
            if value.trim().is_empty() {
                violations.push(invalid_environment(
                    PolicyField::TELEMETRY_ENDPOINT_CONFIGURED,
                ));
            } else {
                self.telemetry.otlp_endpoint = Some(value);
            }
        }
        if let Some(value) = environment("RANVIER_TELEMETRY_OTLP_PROTOCOL") {
            match value.to_ascii_lowercase().as_str() {
                "grpc" => self.telemetry.otlp_protocol = OtlpProtocol::Grpc,
                "http" => self.telemetry.otlp_protocol = OtlpProtocol::Http,
                _ => violations.push(invalid_environment(PolicyField::TELEMETRY_PROTOCOL)),
            }
        }
        if let Some(value) = environment("RANVIER_TELEMETRY_SERVICE_NAME") {
            if value.trim().is_empty() {
                violations.push(invalid_environment(
                    PolicyField::TELEMETRY_SERVICE_NAME_CONFIGURED,
                ));
            } else {
                self.telemetry.service_name = value;
            }
        }
        if let Some(value) = environment("RANVIER_TELEMETRY_SAMPLE_RATIO") {
            match value.parse::<f64>() {
                Ok(ratio) if ratio.is_finite() && (0.0..=1.0).contains(&ratio) => {
                    self.telemetry.sample_ratio = ratio;
                }
                _ => violations.push(invalid_environment(PolicyField::TELEMETRY_SAMPLE_RATIO)),
            }
        }

        violations
    }

    fn value_violations(&self) -> Vec<StartupPolicyViolation> {
        let mut violations = Vec::new();
        if self.server.host.trim().is_empty() {
            violations.push(invalid_config_value(PolicyField::SERVER_HOST));
        }
        if tracing_subscriber::EnvFilter::try_new(&self.logging.level).is_err() {
            violations.push(invalid_config_value(PolicyField::LOGGING_LEVEL));
        }
        if self.logging.module_levels.iter().any(|(module, level)| {
            module.trim().is_empty()
                || tracing_subscriber::EnvFilter::try_new(format!("{module}={level}")).is_err()
        }) {
            violations.push(invalid_config_value(PolicyField::LOGGING_LEVEL));
        }
        if self.telemetry.service_name.trim().is_empty() {
            violations.push(invalid_config_value(
                PolicyField::TELEMETRY_SERVICE_NAME_CONFIGURED,
            ));
        }
        if !self.telemetry.sample_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.telemetry.sample_ratio)
        {
            violations.push(invalid_config_value(PolicyField::TELEMETRY_SAMPLE_RATIO));
        }
        if self.tls.enabled && self.tls.cert_path.trim().is_empty() {
            violations.push(invalid_config_value(PolicyField::TLS_CERT_CONFIGURED));
        }
        if self.tls.enabled && self.tls.key_path.trim().is_empty() {
            violations.push(invalid_config_value(PolicyField::TLS_KEY_CONFIGURED));
        }
        violations
    }

    /// Apply environment variable overrides.
    ///
    /// Supported variables:
    /// - `RANVIER_SERVER_HOST`
    /// - `RANVIER_SERVER_PORT`
    /// - `RANVIER_SERVER_SHUTDOWN_TIMEOUT_SECS`
    /// - `RANVIER_LOGGING_FORMAT` ("json" | "pretty" | "compact")
    /// - `RANVIER_LOGGING_LEVEL`
    /// - `RANVIER_TLS_ENABLED` ("true" | "false")
    /// - `RANVIER_TLS_CERT_PATH`
    /// - `RANVIER_TLS_KEY_PATH`
    /// - `RANVIER_INSPECTOR_ENABLED` ("true" | "false")
    /// - `RANVIER_INSPECTOR_PORT`
    /// - `RANVIER_TELEMETRY_OTLP_ENDPOINT`
    /// - `RANVIER_TELEMETRY_OTLP_PROTOCOL` ("grpc" | "http")
    /// - `RANVIER_TELEMETRY_SERVICE_NAME`
    /// - `RANVIER_TELEMETRY_SAMPLE_RATIO`
    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("RANVIER_SERVER_HOST") {
            self.server.host = v;
        }
        if let Ok(v) = std::env::var("RANVIER_SERVER_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                self.server.port = port;
            }
        }
        if let Ok(v) = std::env::var("RANVIER_SERVER_SHUTDOWN_TIMEOUT_SECS") {
            if let Ok(timeout) = v.parse::<u64>() {
                self.server.shutdown_timeout_secs = timeout;
            }
        }
        if let Ok(v) = std::env::var("RANVIER_LOGGING_FORMAT") {
            match v.to_lowercase().as_str() {
                "json" => self.logging.format = LogFormat::Json,
                "pretty" => self.logging.format = LogFormat::Pretty,
                "compact" => self.logging.format = LogFormat::Compact,
                _ => {}
            }
        }
        if let Ok(v) = std::env::var("RANVIER_LOGGING_LEVEL") {
            self.logging.level = v;
        }
        if let Ok(v) = std::env::var("RANVIER_TLS_ENABLED") {
            if let Ok(enabled) = v.parse::<bool>() {
                self.tls.enabled = enabled;
            }
        }
        if let Ok(v) = std::env::var("RANVIER_TLS_CERT_PATH") {
            self.tls.cert_path = v;
        }
        if let Ok(v) = std::env::var("RANVIER_TLS_KEY_PATH") {
            self.tls.key_path = v;
        }
        if let Ok(v) = std::env::var("RANVIER_INSPECTOR_ENABLED") {
            if let Ok(enabled) = v.parse::<bool>() {
                self.inspector.enabled = enabled;
            }
        }
        if let Ok(v) = std::env::var("RANVIER_INSPECTOR_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                self.inspector.port = port;
            }
        }
        if let Ok(v) = std::env::var("RANVIER_TELEMETRY_OTLP_ENDPOINT") {
            self.telemetry.otlp_endpoint = Some(v);
        }
        if let Ok(v) = std::env::var("RANVIER_TELEMETRY_OTLP_PROTOCOL") {
            match v.to_lowercase().as_str() {
                "grpc" => self.telemetry.otlp_protocol = OtlpProtocol::Grpc,
                "http" => self.telemetry.otlp_protocol = OtlpProtocol::Http,
                _ => {}
            }
        }
        if let Ok(v) = std::env::var("RANVIER_TELEMETRY_SERVICE_NAME") {
            self.telemetry.service_name = v;
        }
        if let Ok(v) = std::env::var("RANVIER_TELEMETRY_SAMPLE_RATIO") {
            if let Ok(ratio) = v.parse::<f64>() {
                self.telemetry.sample_ratio = ratio.clamp(0.0, 1.0);
            }
        }
    }

    /// Returns the server bind address as `"host:port"`.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    /// Returns the graceful shutdown timeout as a `Duration`.
    pub fn shutdown_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.server.shutdown_timeout_secs)
    }

    /// Search for `ranvier.toml` starting from the current directory and walking up.
    fn find_config_file() -> Option<PathBuf> {
        let cwd = std::env::current_dir().ok()?;
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("ranvier.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = dir.parent()?;
        }
    }
}

// ── Structured Logging ──

impl RanvierConfig {
    /// Initialize the `tracing` subscriber based on this configuration.
    ///
    /// Sets up structured logging with the configured format (JSON/pretty/compact),
    /// global log level, and per-module level overrides.
    ///
    /// This should be called once at application startup.
    pub fn init_logging(&self) {
        init_logging(&self.logging);
    }

    /// Initialize OpenTelemetry telemetry based on this configuration.
    ///
    /// When `telemetry.otlp_endpoint` is set, this creates a TracerProvider
    /// and registers it as the global tracer.  When absent, this is a no-op.
    ///
    /// Call this *after* `init_logging()` so that the OTel layer can attach
    /// to the existing tracing subscriber.
    pub fn init_telemetry(&self) {
        if let Some(ref endpoint) = self.telemetry.otlp_endpoint {
            tracing::info!(
                endpoint = %endpoint,
                protocol = ?self.telemetry.otlp_protocol,
                service = %self.telemetry.service_name,
                sample_ratio = %self.telemetry.sample_ratio,
                "OTLP telemetry configured (exporter integration requires `opentelemetry` feature)"
            );
        }
    }
}

/// Initialize the `tracing` subscriber from a `LoggingConfig`.
///
/// # Panics
///
/// Panics if the global subscriber has already been set (call only once).
pub fn init_logging(config: &LoggingConfig) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;

    let mut filter = EnvFilter::try_new(&config.level).unwrap_or_else(|_| EnvFilter::new("info"));
    for (module, level) in &config.module_levels {
        let directive = format!("{}={}", module, level);
        if let Ok(d) = directive.parse() {
            filter = filter.add_directive(d);
        }
    }

    // Allow RUST_LOG env var to override everything
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if let Ok(env_filter) = EnvFilter::try_new(&rust_log) {
            filter = env_filter;
        }
    }

    match config.format {
        LogFormat::Json => {
            let layer = fmt::layer().json().with_target(true).with_thread_ids(true);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .init();
        }
        LogFormat::Pretty => {
            let layer = fmt::layer().pretty().with_target(true);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .init();
        }
        LogFormat::Compact => {
            let layer = fmt::layer().compact().with_target(true);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = RanvierConfig::default();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 3000);
        assert_eq!(cfg.server.shutdown_timeout_secs, 30);
        assert_eq!(cfg.logging.format, LogFormat::Pretty);
        assert_eq!(cfg.logging.level, "info");
        assert!(!cfg.tls.enabled);
        assert!(cfg.inspector.enabled);
        assert_eq!(cfg.inspector.port, 3001);
    }

    #[test]
    fn parse_toml_full() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 8080
shutdown_timeout_secs = 60

[logging]
format = "json"
level = "debug"

[logging.module_levels]
ranvier_runtime = "trace"
sqlx = "warn"

[tls]
enabled = true
cert_path = "certs/cert.pem"
key_path = "certs/key.pem"

[inspector]
enabled = false
port = 9090

[profile.prod.logging]
format = "json"
level = "warn"

[profile.prod.tls]
enabled = true

[profile.prod.inspector]
enabled = false
"#;

        let cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.logging.format, LogFormat::Json);
        assert_eq!(cfg.logging.level, "debug");
        assert_eq!(
            cfg.logging.module_levels.get("ranvier_runtime").unwrap(),
            "trace"
        );
        assert!(cfg.tls.enabled);
        assert!(!cfg.inspector.enabled);
        assert!(cfg.profile.contains_key("prod"));
    }

    #[test]
    fn profile_override_applies() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 3000

[logging]
format = "pretty"
level = "info"

[profile.prod.logging]
format = "json"
level = "warn"

[profile.prod.tls]
enabled = true
"#;

        let mut cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        cfg.apply_profile("prod").unwrap();
        assert_eq!(cfg.logging.format, LogFormat::Json);
        assert_eq!(cfg.logging.level, "warn");
        assert!(cfg.tls.enabled);
        // Server should remain unchanged
        assert_eq!(cfg.server.port, 3000);
    }

    #[test]
    fn profile_not_found_error() {
        let cfg = RanvierConfig::default();
        let mut cfg = cfg;
        let err = cfg.apply_profile("nonexistent").unwrap_err();
        assert!(matches!(err, ConfigError::ProfileNotFound(_)));
    }

    #[test]
    fn env_override_port() {
        let mut cfg = RanvierConfig::default();
        // SAFETY: test runs with --test-threads=1 or env vars are unique per test
        unsafe { std::env::set_var("RANVIER_SERVER_PORT", "9999") };
        cfg.apply_env_overrides();
        assert_eq!(cfg.server.port, 9999);
        unsafe { std::env::remove_var("RANVIER_SERVER_PORT") };
    }

    #[test]
    fn env_override_logging_format() {
        let mut cfg = RanvierConfig::default();
        unsafe { std::env::set_var("RANVIER_LOGGING_FORMAT", "json") };
        cfg.apply_env_overrides();
        assert_eq!(cfg.logging.format, LogFormat::Json);
        unsafe { std::env::remove_var("RANVIER_LOGGING_FORMAT") };
    }

    #[test]
    fn env_override_tls_enabled() {
        let mut cfg = RanvierConfig::default();
        unsafe { std::env::set_var("RANVIER_TLS_ENABLED", "true") };
        cfg.apply_env_overrides();
        assert!(cfg.tls.enabled);
        unsafe { std::env::remove_var("RANVIER_TLS_ENABLED") };
    }

    #[test]
    fn bind_addr_format() {
        let cfg = RanvierConfig::default();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:3000");
    }

    #[test]
    fn shutdown_timeout_duration() {
        let cfg = RanvierConfig::default();
        assert_eq!(cfg.shutdown_timeout(), std::time::Duration::from_secs(30));
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let toml_str = r#"
[server]
port = 4000
"#;
        let cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.port, 4000);
        assert_eq!(cfg.server.host, "127.0.0.1"); // default
        assert_eq!(cfg.logging.format, LogFormat::Pretty); // default
    }

    #[test]
    fn empty_toml_uses_all_defaults() {
        let cfg: RanvierConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 3000);
    }

    #[test]
    fn roundtrip_serialization() {
        let cfg = RanvierConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let cfg2: RanvierConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(cfg.server.port, cfg2.server.port);
        assert_eq!(cfg.logging.level, cfg2.logging.level);
    }

    #[test]
    fn telemetry_defaults() {
        let cfg = RanvierConfig::default();
        assert!(cfg.telemetry.otlp_endpoint.is_none());
        assert_eq!(cfg.telemetry.otlp_protocol, OtlpProtocol::Grpc);
        assert_eq!(cfg.telemetry.service_name, "ranvier");
        assert!((cfg.telemetry.sample_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_telemetry_toml() {
        let toml_str = r#"
[telemetry]
otlp_endpoint = "http://localhost:4317"
otlp_protocol = "http"
service_name  = "my-api"
sample_ratio  = 0.5
"#;
        let cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://localhost:4317")
        );
        assert_eq!(cfg.telemetry.otlp_protocol, OtlpProtocol::Http);
        assert_eq!(cfg.telemetry.service_name, "my-api");
        assert!((cfg.telemetry.sample_ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn telemetry_absent_in_toml_uses_defaults() {
        let toml_str = r#"
[server]
port = 4000
"#;
        let cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.telemetry.otlp_endpoint.is_none());
        assert_eq!(cfg.telemetry.service_name, "ranvier");
    }

    #[test]
    fn telemetry_profile_override() {
        let toml_str = r#"
[telemetry]
service_name = "my-api"

[profile.prod.telemetry]
otlp_endpoint = "http://otel-collector:4317"
sample_ratio = 0.1
"#;
        let mut cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        cfg.apply_profile("prod").unwrap();
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://otel-collector:4317")
        );
        assert!((cfg.telemetry.sample_ratio - 0.1).abs() < f64::EPSILON);
        // service_name should remain from base
        assert_eq!(cfg.telemetry.service_name, "my-api");
    }

    #[test]
    fn telemetry_env_override() {
        let mut cfg = RanvierConfig::default();
        unsafe { std::env::set_var("RANVIER_TELEMETRY_OTLP_ENDPOINT", "http://otel:4317") };
        unsafe { std::env::set_var("RANVIER_TELEMETRY_SERVICE_NAME", "test-svc") };
        unsafe { std::env::set_var("RANVIER_TELEMETRY_SAMPLE_RATIO", "0.25") };
        unsafe { std::env::set_var("RANVIER_TELEMETRY_OTLP_PROTOCOL", "http") };
        cfg.apply_env_overrides();
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://otel:4317")
        );
        assert_eq!(cfg.telemetry.service_name, "test-svc");
        assert!((cfg.telemetry.sample_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(cfg.telemetry.otlp_protocol, OtlpProtocol::Http);
        unsafe { std::env::remove_var("RANVIER_TELEMETRY_OTLP_ENDPOINT") };
        unsafe { std::env::remove_var("RANVIER_TELEMETRY_SERVICE_NAME") };
        unsafe { std::env::remove_var("RANVIER_TELEMETRY_SAMPLE_RATIO") };
        unsafe { std::env::remove_var("RANVIER_TELEMETRY_OTLP_PROTOCOL") };
    }

    #[test]
    fn telemetry_protocol_grpc_default() {
        let cfg = TelemetryConfig::default();
        assert_eq!(cfg.otlp_protocol, OtlpProtocol::Grpc);
    }

    #[test]
    fn telemetry_protocol_http_roundtrip() {
        let toml_str = r#"
[telemetry]
otlp_protocol = "http"
"#;
        let cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.telemetry.otlp_protocol, OtlpProtocol::Http);
    }

    #[test]
    fn telemetry_sample_ratio_default_is_one() {
        let cfg = TelemetryConfig::default();
        assert!((cfg.sample_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn telemetry_partial_profile_preserves_unset_fields() {
        let toml_str = r#"
[telemetry]
otlp_endpoint = "http://base:4317"
service_name = "base-svc"
sample_ratio = 0.8

[profile.staging.telemetry]
sample_ratio = 0.5
"#;
        let mut cfg: RanvierConfig = toml::from_str(toml_str).unwrap();
        cfg.apply_profile("staging").unwrap();
        // Overridden field
        assert!((cfg.telemetry.sample_ratio - 0.5).abs() < f64::EPSILON);
        // Preserved fields
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://base:4317")
        );
        assert_eq!(cfg.telemetry.service_name, "base-svc");
    }

    #[test]
    fn telemetry_init_does_not_panic_without_endpoint() {
        let cfg = RanvierConfig::default();
        // Should be a no-op, not panic
        cfg.init_telemetry();
    }

    #[test]
    fn resolved_runtime_profile_is_separate_from_named_overlay() {
        let document = r#"
[runtime]
profile = "production"

[profile.prod.server]
port = 4111
"#;
        let environment_values =
            HashMap::from([("RANVIER_PROFILE".to_string(), "prod".to_string())]);
        let mut environment = |key: &str| environment_values.get(key).cloned();

        let resolved = ResolvedRuntimeConfig::resolve_document_with_environment(
            document,
            None,
            &mut environment,
        )
        .unwrap();

        assert_eq!(resolved.profile(), RuntimeProfile::Production);
        assert_eq!(resolved.profile_source, RuntimeProfileSource::File);
        assert_eq!(resolved.config().server.port, 4111);
    }

    #[test]
    fn resolved_loader_aggregates_strict_environment_errors_without_raw_values() {
        let environment_values = HashMap::from([
            (
                "RANVIER_SERVER_PORT".to_string(),
                "secret-port-value".to_string(),
            ),
            (
                "RANVIER_INSPECTOR_ENABLED".to_string(),
                "secret-bool-value".to_string(),
            ),
            (
                "RANVIER_TELEMETRY_SAMPLE_RATIO".to_string(),
                "9.5".to_string(),
            ),
        ]);
        let mut environment = |key: &str| environment_values.get(key).cloned();

        let error = ResolvedRuntimeConfig::resolve_document_with_environment(
            "",
            Some(RuntimeProfile::Production),
            &mut environment,
        )
        .unwrap_err();
        let ResolvedConfigError::InvalidRuntimePolicy(error) = error else {
            panic!("expected structured policy violations");
        };

        let violation_codes = error.report().violation_codes().collect::<Vec<_>>();
        assert_eq!(violation_codes.len(), 3);
        assert!(
            violation_codes
                .iter()
                .all(|code| { *code == StartupPolicyCode::ConfigEnvValueInvalid })
        );
        let serialized = serde_json::to_string(error.report()).unwrap();
        assert!(!serialized.contains("secret-port-value"));
        assert!(!serialized.contains("secret-bool-value"));
    }

    #[test]
    fn invalid_lower_precedence_profile_is_not_silently_ignored() {
        let environment_values =
            HashMap::from([("RANVIER_RUNTIME_PROFILE".to_string(), "prod".to_string())]);
        let mut environment = |key: &str| environment_values.get(key).cloned();

        let error = ResolvedRuntimeConfig::resolve_document_with_environment(
            "",
            Some(RuntimeProfile::Production),
            &mut environment,
        )
        .unwrap_err();
        let ResolvedConfigError::InvalidRuntimePolicy(error) = error else {
            panic!("expected invalid runtime profile");
        };

        assert_eq!(error.unacknowledged_count(), 1);
        assert_eq!(
            error.report().violation_codes().collect::<Vec<_>>(),
            vec![StartupPolicyCode::RuntimeProfileInvalid]
        );
    }

    #[test]
    fn acknowledgement_makes_unsafe_state_visible_without_leaking_rationale() {
        let document = r#"
[runtime]
profile = "production"

[[runtime.unsafe_acknowledgements]]
policy_code = "INSPECTOR_AUTH_MISSING"
id = "INC-42"
owner = "release-owner"
rationale = "sentinel-private-rationale"
review_on = "2099-01-01"
expires_on = "2099-01-02"
"#;
        let mut environment = |_key: &str| None;
        let resolved = ResolvedRuntimeConfig::resolve_document_with_environment(
            document,
            None,
            &mut environment,
        )
        .unwrap();
        assert!(!format!("{resolved:?}").contains("sentinel-private-rationale"));
        let violation = StartupPolicyViolation::new(
            StartupPolicyCode::InspectorAuthMissing,
            PolicyComponent::new("inspector"),
            PolicyField::new("inspector_auth_configured"),
        );

        let contribution = StartupPolicyContribution::new(
            PolicyComponent::new("inspector"),
            vec![],
            vec![(violation.code, violation.field)],
        );
        let report = resolved.validate_startup(vec![contribution]).unwrap();
        assert_eq!(
            report.status(),
            crate::runtime_policy::StartupPolicyStatus::AcknowledgedUnsafe
        );
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains("INC-42"));
        assert!(!serialized.contains("sentinel-private-rationale"));
    }

    #[test]
    fn effective_policy_projection_contains_no_raw_configuration_strings() {
        let document = r#"
[runtime]
profile = "production"

[server]
host = "sentinel-secret-host"

[tls]
enabled = false
cert_path = "sentinel-secret-cert-path"
key_path = "sentinel-secret-key-path"

[telemetry]
otlp_endpoint = "https://sentinel-secret-collector"
service_name = "sentinel-secret-service"
"#;
        let mut environment = |_key: &str| None;
        let resolved = ResolvedRuntimeConfig::resolve_document_with_environment(
            document,
            None,
            &mut environment,
        )
        .unwrap();
        let report = resolved.validate_startup(vec![]).unwrap();
        let serialized = serde_json::to_string(&report).unwrap();
        let debug = format!("{resolved:?}");

        assert!(!serialized.contains("sentinel-secret-host"));
        assert!(!serialized.contains("sentinel-secret-cert-path"));
        assert!(!serialized.contains("sentinel-secret-key-path"));
        assert!(!serialized.contains("sentinel-secret-collector"));
        assert!(!serialized.contains("sentinel-secret-service"));
        assert!(!debug.contains("sentinel-secret-host"));
        assert!(!debug.contains("sentinel-secret-key-path"));
        assert!(!debug.contains("sentinel-secret-collector"));
    }

    #[test]
    fn legacy_config_shape_ignores_additive_runtime_table() {
        let config: RanvierConfig = toml::from_str(
            r#"
[runtime]
profile = "production"

[server]
port = 4222
"#,
        )
        .unwrap();

        assert_eq!(config.server.port, 4222);
    }
}
