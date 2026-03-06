//! Transformation nodes for data manipulation within Ranvier circuits.

use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use std::marker::PhantomData;
use std::sync::Arc;

/// Applies a mapping function to transform input to output.
pub struct MapNode<In, Out, F> {
    f: Arc<F>,
    _marker: PhantomData<(In, Out)>,
}

impl<In, Out, F> MapNode<In, Out, F>
where
    F: Fn(In) -> Out + Send + Sync + 'static,
{
    pub fn new(f: F) -> Self {
        Self {
            f: Arc::new(f),
            _marker: PhantomData,
        }
    }
}

impl<In, Out, F> Clone for MapNode<In, Out, F> {
    fn clone(&self) -> Self {
        Self {
            f: self.f.clone(),
            _marker: PhantomData,
        }
    }
}

impl<In, Out, F> std::fmt::Debug for MapNode<In, Out, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapNode").finish()
    }
}

#[async_trait]
impl<In, Out, F> Transition<In, Out> for MapNode<In, Out, F>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    F: Fn(In) -> Out + Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: In,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Out, Self::Error> {
        Outcome::next((self.f)(input))
    }
}

/// Filters elements from a Vec, keeping only those that match the predicate.
pub struct FilterTransformNode<T, F> {
    predicate: Arc<F>,
    _marker: PhantomData<T>,
}

impl<T, F> FilterTransformNode<T, F>
where
    F: Fn(&T) -> bool + Send + Sync + 'static,
{
    pub fn new(predicate: F) -> Self {
        Self {
            predicate: Arc::new(predicate),
            _marker: PhantomData,
        }
    }
}

impl<T, F> Clone for FilterTransformNode<T, F> {
    fn clone(&self) -> Self {
        Self {
            predicate: self.predicate.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T, F> std::fmt::Debug for FilterTransformNode<T, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterTransformNode").finish()
    }
}

#[async_trait]
impl<T, F> Transition<Vec<T>, Vec<T>> for FilterTransformNode<T, F>
where
    T: Send + Sync + 'static,
    F: Fn(&T) -> bool + Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: Vec<T>,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Vec<T>, Self::Error> {
        let filtered: Vec<T> = input.into_iter().filter(|x| (self.predicate)(x)).collect();
        Outcome::next(filtered)
    }
}

/// Flattens a `Vec<Vec<T>>` into a `Vec<T>`.
#[derive(Debug, Clone)]
pub struct FlattenNode<T> {
    _marker: PhantomData<T>,
}

impl<T> FlattenNode<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for FlattenNode<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T> Transition<Vec<Vec<T>>, Vec<T>> for FlattenNode<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: Vec<Vec<T>>,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Vec<T>, Self::Error> {
        Outcome::next(input.into_iter().flatten().collect())
    }
}

/// Merges two JSON values into one (second value's keys override first).
#[derive(Debug, Clone)]
pub struct MergeNode;

impl MergeNode {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MergeNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transition<(serde_json::Value, serde_json::Value), serde_json::Value> for MergeNode {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: (serde_json::Value, serde_json::Value),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        let (mut base, overlay) = input;
        if let (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) =
            (&mut base, overlay)
        {
            for (k, v) in overlay_map {
                base_map.insert(k, v);
            }
            Outcome::next(base)
        } else {
            Outcome::fault("MergeNode requires two JSON objects".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn map_node_transforms_value() {
        let node = MapNode::new(|x: i32| x * 2);
        let mut bus = Bus::new();
        let result = node.run(21, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }

    #[tokio::test]
    async fn map_node_type_conversion() {
        let node = MapNode::new(|x: i32| x.to_string());
        let mut bus = Bus::new();
        let result = node.run(42, &(), &mut bus).await;
        match result {
            Outcome::Next(s) => assert_eq!(s, "42"),
            _ => panic!("Expected Next"),
        }
    }

    #[tokio::test]
    async fn filter_transform_keeps_matching() {
        let node = FilterTransformNode::new(|x: &i32| *x > 3);
        let mut bus = Bus::new();
        let result = node.run(vec![1, 2, 3, 4, 5], &(), &mut bus).await;
        match result {
            Outcome::Next(v) => assert_eq!(v, vec![4, 5]),
            _ => panic!("Expected Next"),
        }
    }

    #[tokio::test]
    async fn flatten_node_flattens() {
        let node = FlattenNode::<i32>::new();
        let mut bus = Bus::new();
        let result = node.run(vec![vec![1, 2], vec![3, 4]], &(), &mut bus).await;
        match result {
            Outcome::Next(v) => assert_eq!(v, vec![1, 2, 3, 4]),
            _ => panic!("Expected Next"),
        }
    }

    #[tokio::test]
    async fn merge_node_combines_objects() {
        let node = MergeNode::new();
        let mut bus = Bus::new();
        let a = serde_json::json!({"name": "Alice", "age": 30});
        let b = serde_json::json!({"age": 31, "city": "NYC"});
        let result = node.run((a, b), &(), &mut bus).await;
        match result {
            Outcome::Next(v) => {
                assert_eq!(v["name"], "Alice");
                assert_eq!(v["age"], 31); // overridden
                assert_eq!(v["city"], "NYC");
            }
            _ => panic!("Expected Next"),
        }
    }
}
