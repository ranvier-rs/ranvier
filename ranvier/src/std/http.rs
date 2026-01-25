use crate::bus::Bus;
use crate::metadata::StepMetadata;
use crate::module::{Module, ModuleResult};
use async_trait::async_trait;
use http::{HeaderName, HeaderValue, response::Builder};
use uuid::Uuid;

/// A simple module that adds a fixed header to the response.
#[derive(Clone)]
pub struct SetHeaderModule {
    key: HeaderName,
    val: HeaderValue,
}

impl SetHeaderModule {
    pub fn new(key: HeaderName, val: HeaderValue) -> Self {
        Self { key, val }
    }
}

#[async_trait]
impl Module for SetHeaderModule {
    fn metadata(&self) -> StepMetadata {
        StepMetadata {
            id: Uuid::new_v4(),
            label: "SetHeader".to_string(),
            description: None,
            inputs: vec![],
            outputs: vec![],
        }
    }

    async fn execute(&self, bus: &mut Bus) -> ModuleResult {
        // http::response::Builder is a bit stateful.
        // We have to swap it out to modify it because header() consumes self.
        let res = std::mem::replace(&mut bus.res, Builder::new());
        bus.res = res.header(self.key.clone(), self.val.clone());
        Ok(())
    }
}
