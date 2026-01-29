use http::{response::Builder, Extensions, Request};
use std::any::Any;

pub struct Bus {
    pub req: Request<()>,
    pub res: Builder,
    extra: Extensions,
}

impl Bus {
    pub fn new(req: Request<()>) -> Self {
        Self {
            req,
            res: Builder::new(),
            extra: Extensions::new(),
        }
    }

    /// Writes data to the Bus (Saltatory Conduction logic).
    /// To ensure strict time safety, `chrono::NaiveDateTime` is NOT allowed.
    pub fn write<T: Any + Send + Sync + Clone + 'static>(&mut self, val: T) {
        // Here we could add a check if T is NaiveDateTime if we can detect it at runtime or compile time,
        // but for now we rely on documentation and best practices.
        // Ideally, we'd have a specialized method for time.
        self.extra.insert(val);
    }

    pub fn read<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        self.extra.get::<T>()
    }

    pub fn read_mut<T: Any + Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.extra.get_mut::<T>()
    }

    /// Check if a type exists in the Bus.
    pub fn has<T: Any + Send + Sync + 'static>(&self) -> bool {
        self.extra.get::<T>().is_some()
    }

    /// Explicitly writes a UTC timestamp.
    /// This is the ONLY recommended way to put time on the Bus.
    pub fn write_time(&mut self, time: chrono::DateTime<chrono::Utc>) {
        self.write(time);
    }
}

/// Unique identifier for a connection (e.g., WebSocket connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub uuid::Uuid);

impl ConnectionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// A specialized Bus that guarantees a Connection context exists.
/// This prevents logic that requires a connection from running in a connection-less context (e.g., HTTP).
pub struct ConnectionBus {
    inner: Bus,
    id: ConnectionId,
}

impl ConnectionBus {
    pub fn new(id: ConnectionId, bus: Bus) -> Self {
        Self { inner: bus, id }
    }

    pub fn id(&self) -> ConnectionId {
        self.id
    }
}

impl std::ops::Deref for ConnectionBus {
    type Target = Bus;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for ConnectionBus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
