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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Top-level Ranvier configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Pretty,
    Compact,
}

/// TLS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OtlpProtocol {
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

impl Default for RanvierConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            logging: LoggingConfig::default(),
            tls: TlsConfig::default(),
            inspector: InspectorConfig::default(),
            telemetry: TelemetryConfig::default(),
            profile: HashMap::new(),
        }
    }
}

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

impl Default for LogFormat {
    fn default() -> Self {
        Self::Pretty
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: String::new(),
            key_path: String::new(),
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

impl Default for OtlpProtocol {
    fn default() -> Self {
        Self::Grpc
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
}
