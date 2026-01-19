//! Ranvier Pipeline Core
//!
//! Type-safe pipeline abstraction based on the original Flux architecture.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Common error type for pipeline steps
#[derive(Debug, Clone)]
pub enum Error {
    BadRequest(String),
    Unauthorized,
    NotFound,
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadRequest(msg) => write!(f, "Bad Request: {}", msg),
            Error::Unauthorized => write!(f, "Unauthorized"),
            Error::NotFound => write!(f, "Not Found"),
            Error::Internal(msg) => write!(f, "Internal Error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

/// The Pipeline struct using boxed steps
pub struct Pipeline<In, Out> {
    executor: Arc<
        dyn Fn(
                In,
                Arc<crate::context::Context>,
            ) -> Pin<Box<dyn Future<Output = Result<Out, Error>> + Send>>
            + Send
            + Sync,
    >,
}

impl<In, Out> Clone for Pipeline<In, Out> {
    fn clone(&self) -> Self {
        Pipeline {
            executor: Arc::clone(&self.executor),
        }
    }
}

impl<In, Out> Pipeline<In, Out>
where
    In: Send + 'static,
    Out: Send + 'static,
{
    /// Create a new pipeline from an async function
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(In, Arc<crate::context::Context>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Out, Error>> + Send + 'static,
    {
        Pipeline {
            executor: Arc::new(move |input, ctx| Box::pin(f(input, ctx))),
        }
    }

    /// Chain another step
    pub fn pipe<NextOut, F, Fut>(self, next_fn: F) -> Pipeline<In, NextOut>
    where
        NextOut: Send + 'static,
        F: Fn(Out, Arc<crate::context::Context>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NextOut, Error>> + Send + 'static,
    {
        let prev = self.executor;
        let next = Arc::new(next_fn);

        Pipeline {
            executor: Arc::new(move |input, ctx| {
                let prev = Arc::clone(&prev);
                let next = Arc::clone(&next);
                let ctx2 = Arc::clone(&ctx);

                Box::pin(async move {
                    let mid = prev(input, ctx).await?;
                    next(mid, ctx2).await
                })
            }),
        }
    }

    /// Execute the pipeline
    pub async fn execute(
        &self,
        input: In,
        ctx: Arc<crate::context::Context>,
    ) -> Result<Out, Error> {
        (self.executor)(input, ctx).await
    }
}

/// Helper to create an identity pipeline (pass-through)
pub fn identity<T: Send + 'static>() -> Pipeline<T, T> {
    Pipeline::new(|input: T, _ctx| async move { Ok(input) })
}
