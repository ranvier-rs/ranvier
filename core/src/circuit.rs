use crate::bus::Bus;
use crate::metadata::StepMetadata;
use crate::module::{Module, ModuleResult};
use async_trait::async_trait;
use uuid::Uuid;

pub struct Circuit {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub modules: Vec<Box<dyn Module>>,
}

impl Circuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: None,
            modules: Vec::new(),
            // inputs/outputs empty for now
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn wire<M: Module + 'static>(mut self, module: M) -> Self {
        self.modules.push(Box::new(module));
        self
    }
}

#[async_trait]
impl Module for Circuit {
    fn metadata(&self) -> StepMetadata {
        StepMetadata {
            id: self.id,
            label: self.name.clone(),
            description: self.description.clone(),
            inputs: vec![],
            outputs: vec![],
        }
    }

    async fn execute(&self, bus: &mut Bus) -> ModuleResult {
        for module in &self.modules {
            module.execute(bus).await?;
        }
        Ok(())
    }
}
