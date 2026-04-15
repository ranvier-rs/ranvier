use crate::{AuditEvent, AuditSink};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Standard audit action types for categorizing events.
///
/// Used with [`AuditLog`] to describe what kind of operation triggered the audit event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditAction {
    Create,
    Read,
    Update,
    Delete,
    Login,
    Logout,
    Export,
    Custom(String),
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create => write!(f, "Create"),
            Self::Read => write!(f, "Read"),
            Self::Update => write!(f, "Update"),
            Self::Delete => write!(f, "Delete"),
            Self::Login => write!(f, "Login"),
            Self::Logout => write!(f, "Logout"),
            Self::Export => write!(f, "Export"),
            Self::Custom(s) => write!(f, "{s}"),
        }
    }
}

/// Identifies who performed an audited action.
///
/// Store in `Bus` via `bus.provide(AuditActor::User { ... })` — typically done
/// by an authentication guard. `AuditLog` extracts this automatically during
/// transition execution. Falls back to `AuditActor::System` when absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditActor {
    /// A human user with an ID and display name.
    User { id: String, name: String },
    /// The system itself (background jobs, startup, etc.).
    System,
    /// An external service or API client.
    Service { name: String },
}

impl fmt::Display for AuditActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User { id, name } => write!(f, "user:{id}:{name}"),
            Self::System => write!(f, "system"),
            Self::Service { name } => write!(f, "service:{name}"),
        }
    }
}

/// A composable audit logging transition for Ranvier Axon chains.
///
/// `AuditLog<S>` implements `Transition<T, T>` — it passes input data unchanged
/// while recording an audit event to the configured sink.
///
/// # Design
///
/// - **Explicit chain node**: visible in Axon chain and Schematic as a named node
/// - **Non-blocking**: sink failures are logged via `tracing::warn!`, pipeline continues
/// - **Composable**: chain with any `T: Send + Sync + 'static`
///
/// # Example
///
/// ```rust,ignore
/// let create_dept = Axon::typed::<DeptInput, String>("dept-create")
///     .then(CreateDepartment)
///     .then(AuditLog::new(audit_sink.clone(), AuditAction::Create, "departments"));
/// ```
pub struct AuditLog<S: AuditSink> {
    sink: Arc<S>,
    action: AuditAction,
    target: String,
}

impl<S: AuditSink> AuditLog<S> {
    /// Creates a new audit log transition node.
    ///
    /// - `sink`: shared audit sink (e.g., `Arc<InMemoryAuditSink>`)
    /// - `action`: the type of action being audited
    /// - `target`: resource identifier (e.g., `"departments"`, `"users"`)
    pub fn new(sink: Arc<S>, action: AuditAction, target: impl Into<String>) -> Self {
        Self {
            sink,
            action,
            target: target.into(),
        }
    }
}

#[async_trait]
impl<T, S> Transition<T, T> for AuditLog<S>
where
    T: Send + Sync + 'static,
    S: AuditSink + 'static,
{
    type Error = String;
    type Resources = ();

    fn label(&self) -> String {
        format!("AuditLog({}:{})", self.action, self.target)
    }

    fn description(&self) -> Option<String> {
        Some(format!("Audit {} on '{}'", self.action, self.target))
    }

    async fn run(&self, state: T, _resources: &(), bus: &mut Bus) -> Outcome<T, String> {
        let actor = bus
            .get_cloned::<AuditActor>()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| AuditActor::System.to_string());

        let event = AuditEvent::new(
            uuid::Uuid::new_v4().to_string(),
            actor,
            self.action.to_string(),
            self.target.clone(),
        );

        if let Err(e) = self.sink.append(&event).await {
            tracing::warn!(
                error = %e,
                action = %self.action,
                target = %self.target,
                "audit log append failed — continuing pipeline"
            );
        }

        Outcome::Next(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuditError, InMemoryAuditSink};
    use async_trait::async_trait;

    // A sink that always fails, for testing graceful error handling.
    struct FailingSink;

    #[async_trait]
    impl AuditSink for FailingSink {
        async fn append(&self, _event: &AuditEvent) -> Result<(), AuditError> {
            Err(AuditError::AppendFailed(
                "intentional test failure".to_string(),
            ))
        }
    }

    #[test]
    fn audit_action_display() {
        assert_eq!(AuditAction::Create.to_string(), "Create");
        assert_eq!(AuditAction::Read.to_string(), "Read");
        assert_eq!(AuditAction::Update.to_string(), "Update");
        assert_eq!(AuditAction::Delete.to_string(), "Delete");
        assert_eq!(AuditAction::Login.to_string(), "Login");
        assert_eq!(AuditAction::Logout.to_string(), "Logout");
        assert_eq!(AuditAction::Export.to_string(), "Export");
        assert_eq!(AuditAction::Custom("Approve".into()).to_string(), "Approve");
    }

    #[test]
    fn audit_actor_display() {
        let user = AuditActor::User {
            id: "u1".into(),
            name: "Alice".into(),
        };
        assert_eq!(user.to_string(), "user:u1:Alice");
        assert_eq!(AuditActor::System.to_string(), "system");
        assert_eq!(
            AuditActor::Service {
                name: "gateway".into()
            }
            .to_string(),
            "service:gateway"
        );
    }

    #[test]
    fn audit_log_label() {
        let sink = Arc::new(InMemoryAuditSink::new());
        let log = AuditLog::new(sink, AuditAction::Create, "departments");
        let label = <AuditLog<InMemoryAuditSink> as Transition<String, String>>::label(&log);
        assert_eq!(label, "AuditLog(Create:departments)");
    }

    #[tokio::test]
    async fn audit_log_records_event() {
        let sink = Arc::new(InMemoryAuditSink::new());
        let log: AuditLog<InMemoryAuditSink> =
            AuditLog::new(sink.clone(), AuditAction::Create, "departments");

        let mut bus = Bus::new();
        bus.provide(AuditActor::User {
            id: "u42".into(),
            name: "Bob".into(),
        });

        let result = log.run("dept-data".to_string(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref s) if s == "dept-data"));

        let events = sink.get_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "Create");
        assert_eq!(events[0].target, "departments");
        assert_eq!(events[0].actor, "user:u42:Bob");
    }

    #[tokio::test]
    async fn audit_log_falls_back_to_system_actor() {
        let sink = Arc::new(InMemoryAuditSink::new());
        let log: AuditLog<InMemoryAuditSink> =
            AuditLog::new(sink.clone(), AuditAction::Delete, "users");

        let mut bus = Bus::new();
        // No AuditActor in Bus — should fall back to "system"

        let result = log.run(42i64, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));

        let events = sink.get_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].actor, "system");
        assert_eq!(events[0].action, "Delete");
    }

    #[tokio::test]
    async fn audit_log_continues_on_sink_failure() {
        let sink = Arc::new(FailingSink);
        let log: AuditLog<FailingSink> = AuditLog::new(sink, AuditAction::Update, "interfaces");

        let mut bus = Bus::new();
        let input = vec![1, 2, 3];

        // Even though the sink fails, the pipeline should continue
        let result = log.run(input.clone(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if *v == vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn audit_log_with_service_actor() {
        let sink = Arc::new(InMemoryAuditSink::new());
        let log: AuditLog<InMemoryAuditSink> =
            AuditLog::new(sink.clone(), AuditAction::Export, "reports");

        let mut bus = Bus::new();
        bus.provide(AuditActor::Service {
            name: "batch-exporter".into(),
        });

        let result = log.run("report-data", &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next("report-data")));

        let events = sink.get_events().await;
        assert_eq!(events[0].actor, "service:batch-exporter");
        assert_eq!(events[0].action, "Export");
    }

    #[tokio::test]
    async fn audit_log_custom_action() {
        let sink = Arc::new(InMemoryAuditSink::new());
        let log: AuditLog<InMemoryAuditSink> = AuditLog::new(
            sink.clone(),
            AuditAction::Custom("Approve".into()),
            "workflows",
        );

        let mut bus = Bus::new();
        let result = log.run("wf-1", &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next("wf-1")));

        let events = sink.get_events().await;
        assert_eq!(events[0].action, "Approve");
        assert_eq!(events[0].target, "workflows");
    }

    #[test]
    fn audit_action_serde_roundtrip() {
        let actions = vec![AuditAction::Create, AuditAction::Custom("BatchRun".into())];
        let json = serde_json::to_string(&actions).unwrap();
        let parsed: Vec<AuditAction> = serde_json::from_str(&json).unwrap();
        assert_eq!(actions, parsed);
    }

    #[test]
    fn audit_actor_serde_roundtrip() {
        let actor = AuditActor::User {
            id: "u1".into(),
            name: "Alice".into(),
        };
        let json = serde_json::to_string(&actor).unwrap();
        let parsed: AuditActor = serde_json::from_str(&json).unwrap();
        assert_eq!(actor, parsed);
    }
}
