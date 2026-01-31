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

use std::any::Any;
use std::collections::HashMap;
use uuid::Uuid;

/// Type-safe resource container for dependency injection.
///
/// Resources are inserted at startup and accessed via type.
/// This ensures compile-time safety and explicit dependencies.
pub struct Bus {
    /// Type-indexed resource storage
    resources: HashMap<std::any::TypeId, Box<dyn Any + Send + Sync>>,
    /// Optional unique identifier for this Bus instance
    pub id: Uuid,
}

impl Bus {
    /// Create a new empty Bus.
    pub fn new() -> Self {
        Self {
            resources: HashMap::new(),
            id: Uuid::new_v4(),
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
        let type_id = std::any::TypeId::of::<T>();
        self.resources
            .get(&type_id)
            .and_then(|r| r.downcast_ref::<T>())
    }

    /// Read a mutable reference to a resource from the Bus.
    ///
    /// Returns `None` if the resource type is not present.
    pub fn read_mut<T: Any + Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        let type_id = std::any::TypeId::of::<T>();
        self.resources
            .get_mut(&type_id)
            .and_then(|r| r.downcast_mut::<T>())
    }

    /// Check if a resource type exists in the Bus.
    pub fn has<T: Any + Send + Sync + 'static>(&self) -> bool {
        let type_id = std::any::TypeId::of::<T>();
        self.resources.contains_key(&type_id)
    }

    /// Remove a resource from the Bus.
    ///
    /// Returns the resource if it was present, `None` otherwise.
    pub fn remove<T: Any + Send + Sync + 'static>(&mut self) -> Option<T> {
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
    fn test_connection_bus() {
        let id = ConnectionId::new();
        let conn = ConnectionBus::new(id);

        assert_eq!(conn.connection_id(), id);
    }
}
