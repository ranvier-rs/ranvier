use tower::Layer;

/// A Step is a named Layer in the pipeline.
///
/// It extends `tower::Layer` with identification metadata for Ranvier Studio.
pub trait Step<S>: Layer<S> {
    fn id(&self) -> &'static str;
}
