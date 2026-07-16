//! Typed runtime intent and secret-free startup-policy diagnostics.
//!
//! This module owns protocol-agnostic policy vocabulary only. Adapter crates
//! contribute typed observations and violations without making `ranvier-core`
//! depend on HTTP, Guard, Inspector, Audit, or Runtime implementations.

use chrono::NaiveDate;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

/// Runtime safety intent selected before application startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProfile {
    /// Local development with loopback-safe operational defaults.
    #[default]
    Development,
    /// Production startup with validated, explicit operational policy.
    Production,
}

impl RuntimeProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }
}

impl fmt::Display for RuntimeProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuntimeProfile {
    type Err = RuntimeProfileParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "development" => Ok(Self::Development),
            "production" => Ok(Self::Production),
            _ => Err(RuntimeProfileParseError),
        }
    }
}

/// A runtime profile was supplied but was not one of the canonical values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("runtime profile must be 'development' or 'production'")]
pub struct RuntimeProfileParseError;

/// The strongest layer that selected the effective runtime profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub(crate) enum RuntimeProfileSource {
    Explicit,
    Environment,
    File,
    #[default]
    Default,
}

/// Stable machine-readable startup-policy identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum StartupPolicyCode {
    RuntimeProfileInvalid,
    ConfigValueInvalid,
    ConfigEnvValueInvalid,
    LegacyModeInvalid,
    LegacyModeConflict,
    InspectorAuthMissing,
    InspectorBindImplicit,
    InspectorCorsPermissive,
    InspectorRetentionUnbounded,
    LocalRateLimitUnbounded,
    DistributedFailureModeUnset,
    AuditRotationUnbounded,
    AuditRetentionUnbounded,
    UnsafeAckInvalid,
    UnsafeAckExpired,
}

impl StartupPolicyCode {
    pub(crate) const fn acknowledgement_allowed(self) -> bool {
        matches!(
            self,
            Self::InspectorAuthMissing
                | Self::InspectorBindImplicit
                | Self::InspectorCorsPermissive
                | Self::InspectorRetentionUnbounded
                | Self::LocalRateLimitUnbounded
                | Self::AuditRotationUnbounded
                | Self::AuditRetentionUnbounded
        )
    }
}

impl FromStr for StartupPolicyCode {
    type Err = StartupPolicyCodeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "RUNTIME_PROFILE_INVALID" => Ok(Self::RuntimeProfileInvalid),
            "CONFIG_VALUE_INVALID" => Ok(Self::ConfigValueInvalid),
            "CONFIG_ENV_VALUE_INVALID" => Ok(Self::ConfigEnvValueInvalid),
            "LEGACY_MODE_INVALID" => Ok(Self::LegacyModeInvalid),
            "LEGACY_MODE_CONFLICT" => Ok(Self::LegacyModeConflict),
            "INSPECTOR_AUTH_MISSING" => Ok(Self::InspectorAuthMissing),
            "INSPECTOR_BIND_IMPLICIT" => Ok(Self::InspectorBindImplicit),
            "INSPECTOR_CORS_PERMISSIVE" => Ok(Self::InspectorCorsPermissive),
            "INSPECTOR_RETENTION_UNBOUNDED" => Ok(Self::InspectorRetentionUnbounded),
            "LOCAL_RATE_LIMIT_UNBOUNDED" => Ok(Self::LocalRateLimitUnbounded),
            "DISTRIBUTED_FAILURE_MODE_UNSET" => Ok(Self::DistributedFailureModeUnset),
            "AUDIT_ROTATION_UNBOUNDED" => Ok(Self::AuditRotationUnbounded),
            "AUDIT_RETENTION_UNBOUNDED" => Ok(Self::AuditRetentionUnbounded),
            "UNSAFE_ACK_INVALID" => Ok(Self::UnsafeAckInvalid),
            "UNSAFE_ACK_EXPIRED" => Ok(Self::UnsafeAckExpired),
            _ => Err(StartupPolicyCodeParseError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown startup policy code")]
pub struct StartupPolicyCodeParseError;

/// Static, secret-free component key used in policy diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PolicyComponent(&'static str);

impl PolicyComponent {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub(crate) const CORE: Self = Self("core");
}

/// Static, secret-free field key used in policy diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PolicyField(&'static str);

impl PolicyField {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub(crate) const RUNTIME_PROFILE: Self = Self("runtime_profile");
    pub(crate) const RUNTIME_PROFILE_SOURCE: Self = Self("runtime_profile_source");
    pub(crate) const SERVER_HOST: Self = Self("server_host");
    pub(crate) const SERVER_PORT: Self = Self("server_port");
    pub(crate) const SERVER_SHUTDOWN_TIMEOUT_SECS: Self = Self("server_shutdown_timeout_secs");
    pub(crate) const LOGGING_FORMAT: Self = Self("logging_format");
    pub(crate) const LOGGING_LEVEL: Self = Self("logging_level");
    pub(crate) const TLS_ENABLED: Self = Self("tls_enabled");
    pub(crate) const TLS_CERT_CONFIGURED: Self = Self("tls_cert_configured");
    pub(crate) const TLS_KEY_CONFIGURED: Self = Self("tls_key_configured");
    pub(crate) const INSPECTOR_ENABLED: Self = Self("inspector_enabled");
    pub(crate) const INSPECTOR_PORT: Self = Self("inspector_port");
    pub(crate) const TELEMETRY_ENDPOINT_CONFIGURED: Self = Self("telemetry_endpoint_configured");
    pub(crate) const TELEMETRY_PROTOCOL: Self = Self("telemetry_protocol");
    pub(crate) const TELEMETRY_SERVICE_NAME_CONFIGURED: Self =
        Self("telemetry_service_name_configured");
    pub(crate) const TELEMETRY_SAMPLE_RATIO: Self = Self("telemetry_sample_ratio");
    pub(crate) const ACKNOWLEDGEMENT_CODE: Self = Self("acknowledgement_code");
    pub(crate) const ACKNOWLEDGEMENT_ID: Self = Self("acknowledgement_id");
    pub(crate) const ACKNOWLEDGEMENT_OWNER: Self = Self("acknowledgement_owner");
    pub(crate) const ACKNOWLEDGEMENT_RATIONALE: Self = Self("acknowledgement_rationale");
    pub(crate) const ACKNOWLEDGEMENT_REVIEW_ON: Self = Self("acknowledgement_review_on");
    pub(crate) const ACKNOWLEDGEMENT_EXPIRES_ON: Self = Self("acknowledgement_expires_on");
}

/// Secret-free value vocabulary for effective-policy observations.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PolicyValue {
    Bool(bool),
    Count(u64),
    DurationMs(u64),
    RatioBasisPoints(u16),
    Configured(bool),
    Label(&'static str),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PolicyObservation {
    field: PolicyField,
    value: PolicyValue,
}

impl PolicyObservation {
    pub const fn new(field: PolicyField, value: PolicyValue) -> Self {
        Self { field, value }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ComponentPolicyReport {
    component: PolicyComponent,
    observations: Vec<PolicyObservation>,
}

impl ComponentPolicyReport {
    pub(crate) fn new(component: PolicyComponent, observations: Vec<PolicyObservation>) -> Self {
        Self {
            component,
            observations,
        }
    }
}

/// One adapter's observations and violations, bound to the same component.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupPolicyContribution {
    component: PolicyComponent,
    observations: Vec<PolicyObservation>,
    violations: Vec<(StartupPolicyCode, PolicyField)>,
}

/// Supplies one component's policy using the profile resolved by core.
///
/// Implementations must be side-effect free. In particular, this method runs
/// before listeners, background tasks, dependency connections, migrations, or
/// durable writes are started.
pub trait StartupPolicyProvider {
    fn startup_policy(&self, profile: RuntimeProfile) -> StartupPolicyContribution;
}

impl StartupPolicyContribution {
    pub fn new(
        component: PolicyComponent,
        observations: Vec<PolicyObservation>,
        violations: Vec<(StartupPolicyCode, PolicyField)>,
    ) -> Self {
        Self {
            component,
            observations,
            violations,
        }
    }

    pub(crate) fn into_report_and_violations(
        self,
    ) -> (ComponentPolicyReport, Vec<StartupPolicyViolation>) {
        let report = ComponentPolicyReport::new(self.component, self.observations);
        let violations = self
            .violations
            .into_iter()
            .map(|(code, field)| StartupPolicyViolation::new(code, self.component, field))
            .collect();
        (report, violations)
    }
}

/// One failed invariant. Raw configuration values are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StartupPolicyViolation {
    pub(crate) code: StartupPolicyCode,
    pub(crate) component: PolicyComponent,
    pub(crate) field: PolicyField,
    acknowledgement_allowed: bool,
}

impl StartupPolicyViolation {
    pub(crate) const fn new(
        code: StartupPolicyCode,
        component: PolicyComponent,
        field: PolicyField,
    ) -> Self {
        Self {
            code,
            component,
            field,
            acknowledgement_allowed: code.acknowledgement_allowed(),
        }
    }
}

/// Attributable and time-bounded acceptance of one unsafe policy condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsafeAcknowledgement {
    policy_code: StartupPolicyCode,
    id: String,
    owner: String,
    rationale: String,
    review_on: NaiveDate,
    expires_on: NaiveDate,
}

impl UnsafeAcknowledgement {
    pub(crate) fn try_new(
        policy_code: StartupPolicyCode,
        id: impl Into<String>,
        owner: impl Into<String>,
        rationale: impl Into<String>,
        review_on: NaiveDate,
        expires_on: NaiveDate,
    ) -> Result<Self, Vec<StartupPolicyViolation>> {
        let acknowledgement = Self {
            policy_code,
            id: id.into().trim().to_string(),
            owner: owner.into().trim().to_string(),
            rationale: rationale.into().trim().to_string(),
            review_on,
            expires_on,
        };
        let violations = acknowledgement.structural_violations();
        if violations.is_empty() {
            Ok(acknowledgement)
        } else {
            Err(violations)
        }
    }

    pub(crate) const fn policy_code(&self) -> StartupPolicyCode {
        self.policy_code
    }

    pub(crate) fn violations_on(&self, today: NaiveDate) -> Vec<StartupPolicyViolation> {
        let mut violations = self.structural_violations();
        if self.review_on < today || self.expires_on < today {
            violations.push(StartupPolicyViolation::new(
                StartupPolicyCode::UnsafeAckExpired,
                PolicyComponent::CORE,
                PolicyField::ACKNOWLEDGEMENT_EXPIRES_ON,
            ));
        }
        violations
    }

    fn structural_violations(&self) -> Vec<StartupPolicyViolation> {
        let mut violations = Vec::new();
        if !self.policy_code.acknowledgement_allowed() {
            violations.push(invalid_ack(PolicyField::ACKNOWLEDGEMENT_CODE));
        }
        if self.id.is_empty() {
            violations.push(invalid_ack(PolicyField::ACKNOWLEDGEMENT_ID));
        }
        if self.owner.is_empty() {
            violations.push(invalid_ack(PolicyField::ACKNOWLEDGEMENT_OWNER));
        }
        if self.rationale.is_empty() {
            violations.push(invalid_ack(PolicyField::ACKNOWLEDGEMENT_RATIONALE));
        }
        if self.expires_on < self.review_on {
            violations.push(invalid_ack(PolicyField::ACKNOWLEDGEMENT_EXPIRES_ON));
        }
        violations
    }

    fn summary(&self) -> AcknowledgementSummary {
        AcknowledgementSummary {
            policy_code: self.policy_code,
            id: self.id.clone(),
            owner: self.owner.clone(),
            review_on: self.review_on,
            expires_on: self.expires_on,
        }
    }
}

fn invalid_ack(field: PolicyField) -> StartupPolicyViolation {
    StartupPolicyViolation::new(
        StartupPolicyCode::UnsafeAckInvalid,
        PolicyComponent::CORE,
        field,
    )
}

/// Secret-free acknowledgement projection. Rationale is intentionally omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AcknowledgementSummary {
    policy_code: StartupPolicyCode,
    id: String,
    owner: String,
    review_on: NaiveDate,
    expires_on: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StartupPolicyStatus {
    Valid,
    Invalid,
    AcknowledgedUnsafe,
}

/// Versioned, secret-free effective-policy projection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartupPolicyReport {
    schema_version: String,
    profile: RuntimeProfile,
    profile_source: RuntimeProfileSource,
    status: StartupPolicyStatus,
    components: Vec<ComponentPolicyReport>,
    acknowledgements: Vec<AcknowledgementSummary>,
    violations: Vec<StartupPolicyViolation>,
}

impl StartupPolicyReport {
    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    pub const fn profile(&self) -> RuntimeProfile {
        self.profile
    }

    pub const fn status(&self) -> StartupPolicyStatus {
        self.status
    }

    pub fn violation_codes(&self) -> impl Iterator<Item = StartupPolicyCode> + '_ {
        self.violations.iter().map(|violation| violation.code)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("startup policy validation failed with {unacknowledged_count} unacknowledged violation(s)")]
pub struct StartupPolicyError {
    report: StartupPolicyReport,
    unacknowledged_count: usize,
}

impl StartupPolicyError {
    pub const fn report(&self) -> &StartupPolicyReport {
        &self.report
    }

    pub const fn unacknowledged_count(&self) -> usize {
        self.unacknowledged_count
    }
}

pub(crate) fn evaluate_startup_policy(
    profile: RuntimeProfile,
    profile_source: RuntimeProfileSource,
    acknowledgements: &[UnsafeAcknowledgement],
    components: Vec<ComponentPolicyReport>,
    violations: Vec<StartupPolicyViolation>,
) -> Result<StartupPolicyReport, StartupPolicyError> {
    let (report, unacknowledged_count) = build_startup_policy_report(
        profile,
        profile_source,
        acknowledgements,
        components,
        violations,
    );

    if unacknowledged_count == 0 {
        Ok(report)
    } else {
        Err(StartupPolicyError {
            report,
            unacknowledged_count,
        })
    }
}

pub(crate) fn invalid_startup_policy(
    profile: RuntimeProfile,
    profile_source: RuntimeProfileSource,
    acknowledgements: &[UnsafeAcknowledgement],
    components: Vec<ComponentPolicyReport>,
    violations: Vec<StartupPolicyViolation>,
) -> StartupPolicyError {
    let (report, unacknowledged_count) = build_startup_policy_report(
        profile,
        profile_source,
        acknowledgements,
        components,
        violations,
    );
    StartupPolicyError {
        report,
        unacknowledged_count,
    }
}

fn build_startup_policy_report(
    profile: RuntimeProfile,
    profile_source: RuntimeProfileSource,
    acknowledgements: &[UnsafeAcknowledgement],
    mut components: Vec<ComponentPolicyReport>,
    mut violations: Vec<StartupPolicyViolation>,
) -> (StartupPolicyReport, usize) {
    for component in &mut components {
        component
            .observations
            .sort_by_key(|observation| observation.field);
    }
    components.sort_by_key(|component| component.component);
    violations.sort_by_key(|violation| (violation.component, violation.code, violation.field));

    let acknowledged_codes: HashSet<_> = acknowledgements
        .iter()
        .map(UnsafeAcknowledgement::policy_code)
        .collect();
    let unacknowledged_count = violations
        .iter()
        .filter(|violation| {
            !violation.acknowledgement_allowed || !acknowledged_codes.contains(&violation.code)
        })
        .count();

    let status = if unacknowledged_count > 0 {
        StartupPolicyStatus::Invalid
    } else if violations.is_empty() {
        StartupPolicyStatus::Valid
    } else {
        StartupPolicyStatus::AcknowledgedUnsafe
    };

    let mut acknowledgement_summaries: Vec<_> = acknowledgements
        .iter()
        .map(UnsafeAcknowledgement::summary)
        .collect();
    acknowledgement_summaries.sort_by(|left, right| {
        (left.policy_code, left.id.as_str()).cmp(&(right.policy_code, right.id.as_str()))
    });

    let report = StartupPolicyReport {
        schema_version: "1.0.0".to_string(),
        profile,
        profile_source,
        status,
        components,
        acknowledgements: acknowledgement_summaries,
        violations,
    };

    (report, unacknowledged_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn runtime_profile_parser_is_strict() {
        assert_eq!(
            "development".parse::<RuntimeProfile>().unwrap(),
            RuntimeProfile::Development
        );
        assert!("PRODUCTION".parse::<RuntimeProfile>().is_err());
        assert!(" production ".parse::<RuntimeProfile>().is_err());
        assert!("prod".parse::<RuntimeProfile>().is_err());
        assert!("unknown".parse::<RuntimeProfile>().is_err());
    }

    #[test]
    fn acknowledgement_rejects_non_acknowledgeable_code_and_empty_fields() {
        let violations = UnsafeAcknowledgement::try_new(
            StartupPolicyCode::ConfigEnvValueInvalid,
            "",
            "",
            "",
            date("2026-07-16"),
            date("2026-07-15"),
        )
        .unwrap_err();

        assert_eq!(violations.len(), 5);
        assert!(
            violations
                .iter()
                .all(|violation| violation.code == StartupPolicyCode::UnsafeAckInvalid)
        );
    }

    #[test]
    fn unacknowledged_violation_is_an_error() {
        let violation = StartupPolicyViolation::new(
            StartupPolicyCode::InspectorAuthMissing,
            PolicyComponent::new("inspector"),
            PolicyField::new("inspector_auth_configured"),
        );
        let error = evaluate_startup_policy(
            RuntimeProfile::Production,
            RuntimeProfileSource::Explicit,
            &[],
            vec![],
            vec![violation],
        )
        .unwrap_err();

        assert_eq!(error.unacknowledged_count(), 1);
        assert_eq!(error.report().status(), StartupPolicyStatus::Invalid);
    }

    #[test]
    fn matching_acknowledgement_keeps_unsafe_condition_visible() {
        let acknowledgement = UnsafeAcknowledgement::try_new(
            StartupPolicyCode::InspectorAuthMissing,
            "INC-42",
            "on-call",
            "temporary incident containment",
            date("2026-07-16"),
            date("2026-07-17"),
        )
        .unwrap();
        let violation = StartupPolicyViolation::new(
            StartupPolicyCode::InspectorAuthMissing,
            PolicyComponent::new("inspector"),
            PolicyField::new("inspector_auth_configured"),
        );
        let report = evaluate_startup_policy(
            RuntimeProfile::Production,
            RuntimeProfileSource::Explicit,
            &[acknowledgement],
            vec![],
            vec![violation],
        )
        .unwrap();

        assert_eq!(report.status(), StartupPolicyStatus::AcknowledgedUnsafe);
        assert_eq!(report.violation_codes().count(), 1);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("temporary incident containment"));
    }

    #[test]
    fn expired_acknowledgement_is_reported() {
        let acknowledgement = UnsafeAcknowledgement::try_new(
            StartupPolicyCode::InspectorBindImplicit,
            "CHG-1",
            "release-owner",
            "temporary migration",
            date("2026-07-14"),
            date("2026-07-15"),
        )
        .unwrap();

        assert_eq!(
            acknowledgement.violations_on(date("2026-07-16"))[0].code,
            StartupPolicyCode::UnsafeAckExpired
        );
    }

    #[test]
    fn overdue_review_is_reported_before_expiry() {
        let acknowledgement = UnsafeAcknowledgement::try_new(
            StartupPolicyCode::InspectorBindImplicit,
            "CHG-2",
            "release-owner",
            "temporary migration",
            date("2026-07-15"),
            date("2026-07-31"),
        )
        .unwrap();

        assert_eq!(
            acknowledgement.violations_on(date("2026-07-16"))[0].code,
            StartupPolicyCode::UnsafeAckExpired
        );
    }
}
