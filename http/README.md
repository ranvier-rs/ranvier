# Ranvier HTTP (`ranvier-http`)

> **The Ingress:** Tower-native HTTP adapter for Ranvier.

## 🎯 Purpose

`ranvier-http` bridges the gap between raw HTTP requests and Ranvier `Axon` circuits. It allows you to expose your business logic as a high-performance HTTP service with minimal boilerplate.

## 🔑 Key Components

- **`RanvierService`**: Implements `tower::Service`, making it compatible with Hyper, Axum, and other Tower ecosystems.
- **`Ranvier` Builder**: The entry point for the "Flat API" (`Ranvier::http()`).
- **Input Converters**: Logic to map incoming `http::Request` to your circuit's `Input` type.

## 🚀 Usage

```rust
use ranvier_http::Ranvier;

Ranvier::http()
    .bind("127.0.0.1:3000")
    .route("/", my_axon)
    .run()
    .await?;
```
