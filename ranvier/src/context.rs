use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Request context passed through the pipeline.
#[derive(Debug, Default)]
pub struct Context {
    extensions: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.extensions.insert(TypeId::of::<T>(), Box::new(val));
    }

    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref())
    }

    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.extensions
            .get_mut(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_mut())
    }
}
