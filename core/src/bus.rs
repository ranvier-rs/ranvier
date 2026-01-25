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

    /// Explicitly writes a UTC timestamp.
    /// This is the ONLY recommended way to put time on the Bus.
    pub fn write_time(&mut self, time: chrono::DateTime<chrono::Utc>) {
        self.write(time);
    }
}
