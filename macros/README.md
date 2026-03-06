# Ranvier Macros (`ranvier-macros`)

Procedural macros for transition wiring and router integration in Ranvier.

## `#[transition(schema)]`

The `schema` attribute enables automatic JSON Schema generation for the transition's input type. When present, the macro generates a `schema_for!(InputType)` call gated behind the `schemars` feature flag.

```rust
#[transition(schema)]
async fn process(input: OrderRequest, _res: &(), _bus: &mut Bus) -> Outcome<OrderResponse, String> {
    // The macro generates schema_for!(OrderRequest) under #[cfg(feature = "schemars")]
    Outcome::Next(OrderResponse { id: input.id })
}
```

The generated schema is consumed by the Inspector schema registry (`/api/v1/routes/schema`) for automatic endpoint documentation and sample payload generation.

## Examples

- [`macros-demo`](../examples/macros-demo/) — #[transition] macro, bus_allow/bus_deny compile-time access control

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
