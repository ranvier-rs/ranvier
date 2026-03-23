use async_trait::async_trait;
use num_traits::Num;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, marker::PhantomData};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum MathOperation {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MathNode<T> {
    pub operation: MathOperation,
    pub operand: T,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> MathNode<T> {
    pub fn new(operation: MathOperation, operand: T) -> Self {
        Self {
            operation,
            operand,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for MathNode<T>
where
    T: Num + Clone + Send + Sync + Debug + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        let result = match self.operation {
            MathOperation::Add => input.clone() + self.operand.clone(),
            MathOperation::Sub => input.clone() - self.operand.clone(),
            MathOperation::Mul => input.clone() * self.operand.clone(),
            MathOperation::Div => input.clone() / self.operand.clone(),
        };

        Outcome::next(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn math_add() {
        let node = MathNode::new(MathOperation::Add, 10i64);
        let mut bus = Bus::new();
        let result = node.run(5i64, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(15)));
    }

    #[tokio::test]
    async fn math_sub() {
        let node = MathNode::new(MathOperation::Sub, 3i64);
        let mut bus = Bus::new();
        let result = node.run(10i64, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(7)));
    }

    #[tokio::test]
    async fn math_mul() {
        let node = MathNode::new(MathOperation::Mul, 4i64);
        let mut bus = Bus::new();
        let result = node.run(5i64, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(20)));
    }

    #[tokio::test]
    async fn math_div() {
        let node = MathNode::new(MathOperation::Div, 2i64);
        let mut bus = Bus::new();
        let result = node.run(10i64, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(5)));
    }
}
