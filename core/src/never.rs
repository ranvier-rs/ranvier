//! # Never: Serde-Compatible Uninhabited Type
//!
//! A replacement for `std::convert::Infallible` that satisfies
//! Axon's `Serialize + DeserializeOwned` bounds.
//!
//! Use `Never` as the error type for pipelines that cannot fail,
//! or as any generic parameter that should be uninhabited.
//!
//! # Example
//!
//! ```rust
//! use ranvier_core::Never;
//!
//! // Never can be used as the error type in Axon type aliases:
//! // type InfallibleAxon<In, Out, Res = ()> = Axon<In, Out, Never, Res>;
//! ```

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::convert::Infallible;
use std::fmt;

/// An uninhabited type that implements `Serialize + DeserializeOwned`.
///
/// This type has no variants and can never be constructed at runtime,
/// making it semantically identical to `std::convert::Infallible`.
/// Unlike `Infallible`, `Never` satisfies the `Serialize + DeserializeOwned`
/// bounds required by `Axon<In, Out, E, Res>`.
///
/// # When to Use
///
/// - As the error type for infallible pipelines: `Axon<In, Out, Never, Res>`
/// - Anywhere an uninhabited type is needed with serde compatibility
///
/// # Guarantees
///
/// - Cannot be constructed (no variants)
/// - Serialization: unreachable (no value exists to serialize)
/// - Deserialization: always fails with a descriptive error
/// - Converts freely from `std::convert::Infallible`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Never {}

impl Serialize for Never {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        match *self {}
    }
}

impl<'de> Deserialize<'de> for Never {
    fn deserialize<D: Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom(
            "Never type cannot be deserialized (uninhabited)",
        ))
    }
}

impl fmt::Display for Never {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for Never {}

impl From<Infallible> for Never {
    fn from(x: Infallible) -> Self {
        match x {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_serialize_json_schema() {
        // Never cannot be instantiated, so we only verify deserialization fails gracefully
        let result: Result<Never, _> = serde_json::from_str("null");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot be deserialized"));
    }

    #[test]
    fn never_deser_any_value_fails() {
        let result: Result<Never, _> = serde_json::from_str("42");
        assert!(result.is_err());
    }

    #[test]
    fn never_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Never>();
    }
}
