//! # Bus: Type-Safe Resource Injection
//!
//! The `Bus` is a typed map that holds **Resources** injected at startup.
//!
//! ## Design Philosophy
//!
//! * **It is NOT a global singleton.**
//! * It is passed explicitly to every transition.
//! * It holds external handles like DB Pools, Configs, or Event Senders.
//! * **It does NOT hold request-specific state** (that belongs in the State Node).
//!
//! ## Protocol Agnosticism
//!
//! The Bus is protocol-agnostic. HTTP-specific types (Request, Response)
//! belong in the **HTTP Adapter Layer**, not in the core Bus.

use std::any::{Any, TypeId, type_name};
use std::collections::HashSet;
use std::sync::Arc;

use ahash::AHashMap;
use uuid::Uuid;

/// Type reference used by bus access policy declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BusTypeRef {
    pub type_id: TypeId,
    pub type_name: &'static str,
}

impl BusTypeRef {
    pub fn of<T: Any + Send + Sync + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
        }
    }
}

/// Transition-scoped bus access policy.
///
/// - `allow`: when present, only listed resource types are accessible.
/// - `deny`: listed resource types are always denied.
#[derive(Debug, Clone, Default)]
pub struct BusAccessPolicy {
    pub allow: Option<Vec<BusTypeRef>>,
    pub deny: Vec<BusTypeRef>,
}

impl BusAccessPolicy {
    pub fn allow_only(types: Vec<BusTypeRef>) -> Self {
        Self {
            allow: Some(types),
            deny: Vec::new(),
        }
    }

    pub fn deny_only(types: Vec<BusTypeRef>) -> Self {
        Self {
            allow: None,
            deny: types,
        }
    }
}

#[derive(Debug, Clone)]
struct BusAccessGuard {
    transition_label: Arc<str>,
    allow: Option<HashSet<TypeId>>,
    allow_names: Arc<[&'static str]>,
    deny: HashSet<TypeId>,
    deny_names: Arc<[&'static str]>,
}

impl BusAccessGuard {
    fn from_policy(transition_label: String, policy: BusAccessPolicy) -> Self {
        let allow_names: Arc<[&'static str]> = policy
            .allow
            .as_ref()
            .map(|types| types.iter().map(|t| t.type_name).collect())
            .unwrap_or_default();
        let allow = policy
            .allow
            .map(|types| types.into_iter().map(|t| t.type_id).collect::<HashSet<_>>());
        let deny_names: Arc<[&'static str]> = policy.deny.iter().map(|t| t.type_name).collect();
        let deny = policy
            .deny
            .into_iter()
            .map(|type_ref| type_ref.type_id)
            .collect::<HashSet<_>>();
        Self {
            transition_label: transition_label.into(),
            allow,
            allow_names,
            deny,
            deny_names,
        }
    }
}

/// Bus access error with policy context.
#[derive(Debug, Clone)]
pub enum BusAccessError {
    Unauthorized {
        transition: String,
        resource: &'static str,
        allow: Option<Vec<&'static str>>,
        deny: Vec<&'static str>,
    },
    NotFound {
        resource: &'static str,
    },
}

impl std::fmt::Display for BusAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BusAccessError::Unauthorized {
                transition,
                resource,
                allow,
                deny,
            } => {
                write!(
                    f,
                    "Bus access denied in transition `{transition}` for resource `{resource}`"
                )?;
                if let Some(allow_list) = allow {
                    write!(f, " (allow={allow_list:?})")?;
                }
                if !deny.is_empty() {
                    write!(f, " (deny={deny:?})")?;
                }
                Ok(())
            }
            BusAccessError::NotFound { resource } => {
                write!(f, "Bus resource not found: `{resource}`")
            }
        }
    }
}

impl std::error::Error for BusAccessError {}

/// Type-safe resource container for dependency injection.
///
/// Resources are inserted at startup and accessed via type.
/// This ensures compile-time safety and explicit dependencies.
pub struct Bus {
    /// Type-indexed resource storage
    resources: AHashMap<std::any::TypeId, Box<dyn Any + Send + Sync>>,
    /// Optional unique identifier for this Bus instance
    pub id: Uuid,
    /// Optional transition-scoped access guard (M143 opt-in)
    access_guard: Option<BusAccessGuard>,
}

impl Bus {
    /// Create a new empty Bus.
    #[inline]
    pub fn new() -> Self {
        Self {
            resources: AHashMap::new(),
            id: Uuid::new_v4(),
            access_guard: None,
        }
    }

    /// Insert a resource into the Bus.
    ///
    /// If a resource of this type already exists, it will be replaced.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// # let db_pool = "PgPool"; // simplified
    /// let mut bus = Bus::new();
    /// bus.insert(db_pool);
    /// ```
    #[inline]
    pub fn insert<T: Any + Send + Sync + 'static>(&mut self, resource: T) {
        let type_id = std::any::TypeId::of::<T>();
        self.resources.insert(type_id, Box::new(resource));
    }

    /// Read a resource from the Bus.
    ///
    /// Returns `None` if the resource type is not present **or** if access is
    /// denied by an active [`BusAccessPolicy`]. Policy violations are logged
    /// via `tracing::error!` instead of panicking, so a misconfigured policy
    /// cannot crash the server.
    ///
    /// For explicit error handling, use [`get`](Bus::get) which returns
    /// `Result<&T, BusAccessError>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// # let mut bus = Bus::new();
    /// # bus.insert(42i32);
    /// let value = bus.read::<i32>().unwrap();
    /// assert_eq!(*value, 42);
    /// ```
    #[inline]
    pub fn read<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        match self.get::<T>() {
            Ok(value) => Some(value),
            Err(BusAccessError::NotFound { .. }) => None,
            Err(err) => {
                tracing::error!("{err}");
                None
            }
        }
    }

    /// Read a mutable reference to a resource from the Bus.
    ///
    /// Returns `None` if the resource type is not present or access is denied.
    /// Policy violations are logged via `tracing::error!` instead of panicking.
    ///
    /// For explicit error handling, use [`get_mut`](Bus::get_mut).
    #[inline]
    pub fn read_mut<T: Any + Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        match self.get_mut::<T>() {
            Ok(value) => Some(value),
            Err(BusAccessError::NotFound { .. }) => None,
            Err(err) => {
                tracing::error!("{err}");
                None
            }
        }
    }

    /// Read a resource with explicit policy/not-found error details.
    #[inline]
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Result<&T, BusAccessError> {
        self.ensure_access::<T>()?;
        let type_id = TypeId::of::<T>();
        self.resources
            .get(&type_id)
            .and_then(|r| r.downcast_ref::<T>())
            .ok_or(BusAccessError::NotFound {
                resource: type_name::<T>(),
            })
    }

    /// Read a mutable resource with explicit policy/not-found error details.
    #[inline]
    pub fn get_mut<T: Any + Send + Sync + 'static>(&mut self) -> Result<&mut T, BusAccessError> {
        self.ensure_access::<T>()?;
        let type_id = TypeId::of::<T>();
        self.resources
            .get_mut(&type_id)
            .and_then(|r| r.downcast_mut::<T>())
            .ok_or(BusAccessError::NotFound {
                resource: type_name::<T>(),
            })
    }

    /// Check if a resource type exists in the Bus.
    ///
    /// Returns `false` if access is denied by an active policy (logged via
    /// `tracing::error!`).
    #[inline]
    pub fn has<T: Any + Send + Sync + 'static>(&self) -> bool {
        if let Err(err) = self.ensure_access::<T>() {
            tracing::error!("{err}");
            return false;
        }
        let type_id = std::any::TypeId::of::<T>();
        self.resources.contains_key(&type_id)
    }

    /// Remove a resource from the Bus.
    ///
    /// Returns the resource if it was present, `None` otherwise.
    /// Returns `None` if access is denied by an active policy (logged via
    /// `tracing::error!`).
    pub fn remove<T: Any + Send + Sync + 'static>(&mut self) -> Option<T> {
        if let Err(err) = self.ensure_access::<T>() {
            tracing::error!("{err}");
            return None;
        }
        let type_id = std::any::TypeId::of::<T>();
        self.resources
            .remove(&type_id)
            .and_then(|r| r.downcast::<T>().ok().map(|b| *b))
    }

    /// Get the number of resources in the Bus.
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Check if the Bus is empty.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Provide a resource to the Bus.
    ///
    /// Semantic alias for [`insert`](Bus::insert) that makes the intent clearer
    /// when injecting external library handles (DB pools, HTTP clients, etc.).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// let mut bus = Bus::new();
    /// bus.provide(42i32);
    /// assert_eq!(*bus.read::<i32>().unwrap(), 42);
    /// ```
    #[inline]
    pub fn provide<T: Any + Send + Sync + 'static>(&mut self, resource: T) {
        self.insert(resource);
    }

    /// Require a resource from the Bus, panicking with a helpful message if missing.
    ///
    /// Use this when the resource is expected to always be present (e.g., injected
    /// at startup). For optional resources, use [`try_require`](Bus::try_require).
    ///
    /// # Panics
    ///
    /// Panics if the resource type `T` has not been inserted into the Bus.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// let mut bus = Bus::new();
    /// bus.provide(42i32);
    /// let value: &i32 = bus.require::<i32>();
    /// assert_eq!(*value, 42);
    /// ```
    #[inline]
    pub fn require<T: Any + Send + Sync + 'static>(&self) -> &T {
        self.read::<T>().unwrap_or_else(|| {
            panic!(
                "Bus: required resource `{}` not found. Did you forget to call bus.provide()?",
                std::any::type_name::<T>()
            )
        })
    }

    /// Try to require a resource from the Bus, returning `None` if missing.
    ///
    /// Semantic alias for [`read`](Bus::read) that pairs with [`provide`](Bus::provide)
    /// and [`require`](Bus::require) for consistent naming.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// let bus = Bus::new();
    /// assert!(bus.try_require::<i32>().is_none());
    /// ```
    #[inline]
    pub fn try_require<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        self.read::<T>()
    }

    /// Read a resource and clone it in one step.
    ///
    /// Equivalent to `bus.get::<T>().map(Clone::clone)` but more concise.
    /// Useful when a transition needs an owned copy (e.g., `PgPool`, `Arc<T>`).
    ///
    /// Returns `Err(BusAccessError::NotFound)` if the resource is missing, or
    /// `Err(BusAccessError::Unauthorized)` if an access policy denies it.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use ranvier_core::Bus;
    /// let mut bus = Bus::new();
    /// bus.provide(42i32);
    /// let value: i32 = bus.get_cloned::<i32>().unwrap();
    /// assert_eq!(value, 42);
    /// ```
    #[inline]
    pub fn get_cloned<T: Any + Send + Sync + Clone + 'static>(&self) -> Result<T, BusAccessError> {
        self.get::<T>().map(Clone::clone)
    }

    /// Set transition-scoped policy. `None` keeps access unrestricted.
    pub fn set_access_policy(
        &mut self,
        transition_label: impl Into<String>,
        policy: Option<BusAccessPolicy>,
    ) {
        self.access_guard =
            policy.map(|policy| BusAccessGuard::from_policy(transition_label.into(), policy));
    }

    /// Clear transition-scoped policy.
    pub fn clear_access_policy(&mut self) {
        self.access_guard = None;
    }

    #[inline]
    fn ensure_access<T: Any + Send + Sync + 'static>(&self) -> Result<(), BusAccessError> {
        let Some(guard) = &self.access_guard else {
            return Ok(());
        };

        let requested = TypeId::of::<T>();
        if guard.deny.contains(&requested) {
            return Err(BusAccessError::Unauthorized {
                transition: guard.transition_label.to_string(),
                resource: type_name::<T>(),
                allow: if guard.allow_names.is_empty() {
                    None
                } else {
                    Some(guard.allow_names.to_vec())
                },
                deny: guard.deny_names.to_vec(),
            });
        }

        if let Some(allow) = &guard.allow
            && !allow.contains(&requested)
        {
            return Err(BusAccessError::Unauthorized {
                transition: guard.transition_label.to_string(),
                resource: type_name::<T>(),
                allow: Some(guard.allow_names.to_vec()),
                deny: guard.deny_names.to_vec(),
            });
        }

        Ok(())
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique identifier for a connection (e.g., WebSocket connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub Uuid);

impl Default for ConnectionId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl ConnectionId {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A specialized Bus that guarantees a Connection context exists.
///
/// This prevents logic that requires a connection from running in a
/// connection-less context (e.g., HTTP).
pub struct ConnectionBus {
    /// The underlying resource Bus
    pub bus: Bus,
    /// The connection identifier
    pub id: ConnectionId,
}

impl ConnectionBus {
    /// Create a new ConnectionBus with the given ID.
    pub fn new(id: ConnectionId) -> Self {
        Self {
            bus: Bus::new(),
            id,
        }
    }

    /// Create a ConnectionBus from an existing Bus.
    pub fn from_bus(id: ConnectionId, bus: Bus) -> Self {
        Self { bus, id }
    }

    /// Get the connection ID.
    pub fn connection_id(&self) -> ConnectionId {
        self.id
    }
}

impl std::ops::Deref for ConnectionBus {
    type Target = Bus;

    fn deref(&self) -> &Self::Target {
        &self.bus
    }
}

impl std::ops::DerefMut for ConnectionBus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_read() {
        let mut bus = Bus::new();
        bus.insert(42i32);

        assert!(bus.has::<i32>());
        assert_eq!(*bus.read::<i32>().unwrap(), 42);
    }

    #[test]
    fn test_read_none() {
        let bus = Bus::new();
        assert!(bus.read::<i32>().is_none());
    }

    #[test]
    fn test_remove() {
        let mut bus = Bus::new();
        bus.insert(42i32);

        let value = bus.remove::<i32>();
        assert_eq!(value, Some(42));
        assert!(!bus.has::<i32>());
    }

    #[test]
    fn test_multiple_types() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());

        assert_eq!(*bus.read::<i32>().unwrap(), 42);
        assert_eq!(bus.read::<String>().unwrap(), "hello");
    }

    #[test]
    fn bus_policy_allow_only_blocks_unauthorized_get() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());
        bus.set_access_policy(
            "OnlyInt",
            Some(BusAccessPolicy::allow_only(vec![BusTypeRef::of::<i32>()])),
        );

        let err = bus.get::<String>().expect_err("String should be denied");
        assert!(err.to_string().contains("OnlyInt"));
        assert!(err.to_string().contains("alloc::string::String"));
    }

    #[test]
    fn bus_policy_deny_only_blocks_explicit_type() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());
        bus.set_access_policy(
            "DenyString",
            Some(BusAccessPolicy::deny_only(vec![BusTypeRef::of::<String>()])),
        );

        let err = bus.get::<String>().expect_err("String should be denied");
        assert!(err.to_string().contains("DenyString"));
    }

    #[test]
    fn test_connection_bus() {
        let id = ConnectionId::new();
        let conn = ConnectionBus::new(id);

        assert_eq!(conn.connection_id(), id);
    }

    #[test]
    fn provide_and_require_round_trip() {
        let mut bus = Bus::new();
        bus.provide(42i32);
        assert_eq!(*bus.require::<i32>(), 42);
    }

    #[test]
    #[should_panic(expected = "required resource")]
    fn require_panics_with_helpful_message_when_missing() {
        let bus = Bus::new();
        let _ = bus.require::<String>();
    }

    #[test]
    fn try_require_returns_none_when_missing() {
        let bus = Bus::new();
        assert!(bus.try_require::<i32>().is_none());
    }

    #[test]
    fn try_require_returns_some_when_present() {
        let mut bus = Bus::new();
        bus.provide("hello".to_string());
        assert_eq!(bus.try_require::<String>().unwrap(), "hello");
    }

    #[test]
    fn test_reinsertion_overwrites_previous_value() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        assert_eq!(*bus.read::<i32>().unwrap(), 42);

        bus.insert(100i32);
        assert_eq!(*bus.read::<i32>().unwrap(), 100);
    }

    #[test]
    fn test_remove_then_read_returns_none() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        assert!(bus.read::<i32>().is_some());

        let removed = bus.remove::<i32>();
        assert_eq!(removed, Some(42));
        assert!(bus.read::<i32>().is_none());
    }

    #[test]
    fn test_is_empty_after_insertions_and_removals() {
        let mut bus = Bus::new();
        assert!(bus.is_empty());
        assert_eq!(bus.len(), 0);

        bus.insert(42i32);
        assert!(!bus.is_empty());
        assert_eq!(bus.len(), 1);

        bus.insert("hello".to_string());
        assert!(!bus.is_empty());
        assert_eq!(bus.len(), 2);

        bus.remove::<i32>();
        assert!(!bus.is_empty());
        assert_eq!(bus.len(), 1);

        bus.remove::<String>();
        assert!(bus.is_empty());
        assert_eq!(bus.len(), 0);
    }

    #[test]
    fn test_read_mut_modifies_value_in_place() {
        let mut bus = Bus::new();
        bus.insert(42i32);

        if let Some(value) = bus.read_mut::<i32>() {
            *value = 100;
        }

        assert_eq!(*bus.read::<i32>().unwrap(), 100);
    }

    #[test]
    fn test_multiple_types_coexist() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert(3.14f64);
        bus.insert("hello".to_string());
        bus.insert(true);

        assert!(bus.has::<i32>());
        assert!(bus.has::<f64>());
        assert!(bus.has::<String>());
        assert!(bus.has::<bool>());

        assert_eq!(*bus.read::<i32>().unwrap(), 42);
        assert_eq!(*bus.read::<f64>().unwrap(), 3.14);
        assert_eq!(bus.read::<String>().unwrap(), "hello");
        assert_eq!(*bus.read::<bool>().unwrap(), true);
    }

    #[test]
    fn bus_policy_violation_returns_none_instead_of_panic() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());
        bus.set_access_policy(
            "OnlyInt",
            Some(BusAccessPolicy::allow_only(vec![BusTypeRef::of::<i32>()])),
        );

        // read() should return None instead of panicking on policy violation
        assert!(bus.read::<String>().is_none());
        // has() should return false instead of panicking
        assert!(!bus.has::<String>());
        // Allowed type should still work
        assert_eq!(*bus.read::<i32>().unwrap(), 42);
        assert!(bus.has::<i32>());
    }

    #[test]
    fn get_cloned_returns_owned_copy() {
        let mut bus = Bus::new();
        bus.provide("hello".to_string());
        let cloned: String = bus.get_cloned::<String>().unwrap();
        assert_eq!(cloned, "hello");
        // Original still in bus
        assert_eq!(bus.read::<String>().unwrap(), "hello");
    }

    #[test]
    fn get_cloned_missing_returns_not_found() {
        let bus = Bus::new();
        let err = bus.get_cloned::<String>().unwrap_err();
        assert!(matches!(err, BusAccessError::NotFound { .. }));
    }

    #[test]
    fn get_cloned_policy_violation_returns_unauthorized() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());
        bus.set_access_policy(
            "OnlyInt",
            Some(BusAccessPolicy::allow_only(vec![BusTypeRef::of::<i32>()])),
        );
        let err = bus.get_cloned::<String>().unwrap_err();
        assert!(matches!(err, BusAccessError::Unauthorized { .. }));
        // Allowed type works
        assert_eq!(bus.get_cloned::<i32>().unwrap(), 42);
    }

    #[test]
    fn bus_policy_violation_remove_returns_none() {
        let mut bus = Bus::new();
        bus.insert("hello".to_string());
        bus.set_access_policy(
            "DenyString",
            Some(BusAccessPolicy::deny_only(vec![BusTypeRef::of::<String>()])),
        );

        // remove() should return None instead of panicking
        assert!(bus.remove::<String>().is_none());
    }
}
