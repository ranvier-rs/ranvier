//! Block - Reusable Sub-Trees (Pipe Blocks)
//!
//! Blocks are encapsulated sub-trees of state transitions that can be
//! composed together. They represent reusable business logic patterns.
//!
//! # Philosophy
//! > Block = Encapsulated decision sub-tree

use std::marker::PhantomData;

/// A Block represents a reusable sub-tree of transitions.
///
/// Blocks can be thought of as "macros" for common patterns:
/// - Authentication block
/// - Validation block
/// - Rate limiting block
///
/// # Example
/// ```rust,ignore
/// let auth_block = Block::new("auth")
///     .then::<ValidateToken>()
///     .then::<LoadUser>();
/// ```
pub struct Block<In, Out, Ctx, E> {
    /// Block identifier for debugging/tracing
    pub name: &'static str,
    _phantom: PhantomData<fn(In, &Ctx) -> Result<Out, E>>,
}

impl<In, Out, Ctx, E> Block<In, Out, Ctx, E> {
    /// Create a new block with the given name
    pub const fn new(name: &'static str) -> Self {
        Block {
            name,
            _phantom: PhantomData,
        }
    }
}

/// Trait for types that can be executed as a block.
///
/// This is implemented by composable block patterns.
pub trait BlockExecutor<Ctx> {
    /// Input type
    type Input;
    /// Output type
    type Output;
    /// Error type
    type Error;

    /// Execute this block, transforming input to output
    fn execute(&self, input: Self::Input, ctx: &Ctx) -> Result<Self::Output, Self::Error>;
}

/// A composed block that chains two blocks together.
///
/// The intermediate type `Mid` is captured in the struct itself
/// to satisfy Rust's type parameter constraints.
pub struct ChainedBlock<A, B, Mid> {
    first: A,
    second: B,
    _mid: PhantomData<Mid>,
}

impl<A, B, Mid> ChainedBlock<A, B, Mid> {
    pub fn new(first: A, second: B) -> Self {
        ChainedBlock {
            first,
            second,
            _mid: PhantomData,
        }
    }
}

impl<A, B, Mid, Ctx> BlockExecutor<Ctx> for ChainedBlock<A, B, Mid>
where
    A: BlockExecutor<Ctx, Output = Mid>,
    B: BlockExecutor<Ctx, Input = Mid>,
    A::Error: Into<B::Error>,
{
    type Input = A::Input;
    type Output = B::Output;
    type Error = B::Error;

    fn execute(&self, input: Self::Input, ctx: &Ctx) -> Result<Self::Output, Self::Error> {
        let mid = self.first.execute(input, ctx).map_err(Into::into)?;
        self.second.execute(mid, ctx)
    }
}
