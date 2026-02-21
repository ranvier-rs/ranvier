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
use std::collections::{HashMap, HashSet};
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
    transition_label: String,
    allow: Option<HashSet<TypeId>>,
    allow_names: Vec<&'static str>,
    deny: HashSet<TypeId>,
    deny_names: Vec<&'static str>,
}

impl BusAccessGuard {
    fn from_policy(transition_label: String, policy: BusAccessPolicy) -> Self {
        let allow_names = policy
            .allow
            .as_ref()
            .map(|types| types.iter().map(|t| t.type_name).collect::<Vec<_>>())
            .unwrap_or_default();
        let allow = policy
            .allow
            .map(|types| types.into_iter().map(|t| t.type_id).collect::<HashSet<_>>());
        let deny_names = policy.deny.iter().map(|t| t.type_name).collect::<Vec<_>>();
        let deny = policy
            .deny
            .into_iter()
            .map(|type_ref| type_ref.type_id)
            .collect::<HashSet<_>>();
        Self {
            transition_label,
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
    resources: HashMap<std::any::TypeId, Box<dyn Any + Send + Sync>>,
    /// Optional unique identifier for this Bus instance
    pub id: Uuid,
    /// Optional transition-scoped access guard (M143 opt-in)
    access_guard: Option<BusAccessGuard>,
}

impl Bus {
    /// Create a new empty Bus.
    pub fn new() -> Self {
        Self {
            resources: HashMap::new(),
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
    pub fn insert<T: Any + Send + Sync + 'static>(&mut self, resource: T) {
        let type_id = std::any::TypeId::of::<T>();
        self.resources.insert(type_id, Box::new(resource));
    }

    /// Read a resource from the Bus.
    ///
    /// Returns `None` if the resource type is not present.
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
    pub fn read<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        match self.get::<T>() {
            Ok(value) => Some(value),
            Err(BusAccessError::NotFound { .. }) => None,
            Err(err) => panic!("{err}"),
        }
    }

    /// Read a mutable reference to a resource from the Bus.
    ///
    /// Returns `None` if the resource type is not present.
    pub fn read_mut<T: Any + Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        match self.get_mut::<T>() {
            Ok(value) => Some(value),
            Err(BusAccessError::NotFound { .. }) => None,
            Err(err) => panic!("{err}"),
        }
    }

    /// Read a resource with explicit policy/not-found error details.
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
    pub fn has<T: Any + Send + Sync + 'static>(&self) -> bool {
        if let Err(err) = self.ensure_access::<T>() {
            panic!("{err}");
        }
        let type_id = std::any::TypeId::of::<T>();
        self.resources.contains_key(&type_id)
    }

    /// Remove a resource from the Bus.
    ///
    /// Returns the resource if it was present, `None` otherwise.
    pub fn remove<T: Any + Send + Sync + 'static>(&mut self) -> Option<T> {
        if let Err(err) = self.ensure_access::<T>() {
            panic!("{err}");
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

    fn ensure_access<T: Any + Send + Sync + 'static>(&self) -> Result<(), BusAccessError> {
        let Some(guard) = &self.access_guard else {
            return Ok(());
        };

        let requested = TypeId::of::<T>();
        if guard.deny.contains(&requested) {
            return Err(BusAccessError::Unauthorized {
                transition: guard.transition_label.clone(),
                resource: type_name::<T>(),
                allow: if guard.allow_names.is_empty() {
                    None
                } else {
                    Some(guard.allow_names.clone())
                },
                deny: guard.deny_names.clone(),
            });
        }

        if let Some(allow) = &guard.allow {
            if !allow.contains(&requested) {
                return Err(BusAccessError::Unauthorized {
                    transition: guard.transition_label.clone(),
                    resource: type_name::<T>(),
                    allow: Some(guard.allow_names.clone()),
                    deny: guard.deny_names.clone(),
                });
            }
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

impl ConnectionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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
}
