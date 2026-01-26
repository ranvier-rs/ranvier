//! Bus - Type-Safe Resource Injection
//!
//! The Bus is the "wiring" for resources - type-safe, compile-time verified.
//!
//! # Philosophy
//! > Types = Wires. Type mismatch = Wiring error at compile time.
//!
//! The Bus does NOT use string keys or dynamic casting.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Type-safe resource container (TypeMap pattern).
///
/// Bus provides compile-time verified resource injection.
/// No string keys, no `Any` downcasting at runtime*.
///
/// (*TypeId is used internally, but the API is fully typed)
#[derive(Default)]
pub struct Bus {
    resources: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Bus {
    /// Create a new empty Bus
    pub fn new() -> Self {
        Bus {
            resources: HashMap::new(),
        }
    }

    /// Insert a resource into the Bus.
    ///
    /// If a resource of this type already exists, it is replaced.
    pub fn insert<T: Send + Sync + 'static>(&mut self, resource: T) {
        self.resources.insert(TypeId::of::<T>(), Box::new(resource));
    }

    /// Get a reference to a resource.
    ///
    /// Returns `None` if the resource type is not present.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.resources
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref())
    }

    /// Get a mutable reference to a resource.
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.resources
            .get_mut(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_mut())
    }

    /// Check if a resource type is present.
    pub fn contains<T: 'static>(&self) -> bool {
        self.resources.contains_key(&TypeId::of::<T>())
    }

    /// Remove a resource from the Bus, returning it if present.
    pub fn remove<T: 'static>(&mut self) -> Option<T> {
        self.resources
            .remove(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast().ok())
            .map(|boxed| *boxed)
    }
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bus")
            .field("resource_count", &self.resources.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut bus = Bus::new();
        bus.insert(42i32);
        bus.insert("hello".to_string());

        assert_eq!(bus.get::<i32>(), Some(&42));
        assert_eq!(bus.get::<String>(), Some(&"hello".to_string()));
        assert_eq!(bus.get::<f64>(), None);
    }

    #[test]
    fn test_get_mut() {
        let mut bus = Bus::new();
        bus.insert(vec![1, 2, 3]);

        if let Some(v) = bus.get_mut::<Vec<i32>>() {
            v.push(4);
        }

        assert_eq!(bus.get::<Vec<i32>>(), Some(&vec![1, 2, 3, 4]));
    }
}
