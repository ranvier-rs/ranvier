//! GraphQL Ingress Adapter for Ranvier
//!
//! This crate provides the `GraphQLIngress` adapter and utilities for mapping
//! `async-graphql` schemas to Ranvier `Axon` circuits.

pub mod ingress;
pub mod schema;

pub use async_graphql;
pub use ingress::GraphQLIngress;
