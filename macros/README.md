# Ranvier Macros (`ranvier-macros`)

Procedural macros for defining Ranvier transitions.

This README describes the current source candidate. Its release version/channel
is pending; published `0.51.0` artifacts are a separate contract.

`#[transition]` accepts free async functions with owned input, borrowed resources,
and mutable Bus parameters, in that order. Each role is optional. Resources use
direct `Send + Sync + 'static` bounds. `resources = Type` is accepted only when
the function has no resource parameter; otherwise the `&Type` parameter determines
the resource type.

## `#[transition(schema)]`

The `schema` attribute generates JSON Schema metadata for the input type, which
must implement `schemars::JsonSchema`. Enable `ranvier/schema` for a facade
consumer, or `ranvier-core/schema` for a direct-core consumer. Adding a consumer
feature named `schemars` does not enable the required framework API. A requested
schema with the framework feature disabled fails compilation.

The expansion resolves its schema/JSON helpers through core macro support;
consumers do not add implementation dependencies solely for expansion. A custom
input's `JsonSchema` implementation may require its own derive dependencies.

The following source matches the maintained facade schema fixture:

```rust
use ranvier::prelude::*;

#[transition(schema)]
async fn facade_schema_transition(
    input: String,
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, String> {
    Outcome::next(input)
}
```

The local fixture in `tests/compile/facade-schema-attribute` tests
`Transition::input_schema()` with `--features schema-contract`; the same source
must fail without that feature. This proves local schema generation. Inspector
route registration, schema transport and hosted endpoint behavior require their
own integration checks.

## `#[streaming_transition]`

Enable the `streaming` feature on `ranvier-macros` (or `ranvier/streaming` through
the facade). The function returns `Result<impl Stream<Item = T> + Send, E>` and
uses the same ordered parameter roles and resource-override restriction.
Streaming schema metadata and Bus policy attributes are currently rejected;
they require separate trait/Schematic contracts before support is added.

## Examples

- [`macros-demo`](../examples/macros-demo/) — `#[transition]` plus compile-time policy-declaration checks and runtime Bus enforcement

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
