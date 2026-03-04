# Migration Guide: 0.15 → 0.16

This guide covers all changes needed when upgrading from Ranvier 0.15.x to 0.16.

---

## Quick Summary

Ranvier 0.16 is primarily a **stability release**. The API has been frozen since v0.10 and no major breaking changes are introduced. The upgrade is straightforward for most users.

**Required changes:**
1. Update version numbers in `Cargo.toml`
2. Remove usage of deprecated items (if any)
3. Ensure `Serialize + DeserializeOwned` bounds on Axon type parameters

**No changes needed if:**
- You are already on 0.15.x and not using deprecated APIs
- Your Axon type parameters already satisfy serde bounds

---

## Step 1: Update Dependencies

Update all `ranvier-*` dependencies in your `Cargo.toml`:

```toml
# Before (0.15.x)
ranvier-core = "0.15"
ranvier-runtime = "0.15"
ranvier-http = "0.15"

# After (0.16.x)
ranvier-core = "0.16"
ranvier-runtime = "0.16"
ranvier-http = "0.16"
```

If you use `ranvier-kit` (the convenience facade), updating it alone is sufficient:

```toml
ranvier-kit = "0.16"
```

---

## Step 2: Deprecated API Removal

### `static_gen::StaticNode` → `StaticAxon`

```rust
// Before
use ranvier_core::static_gen::StaticNode;
let node = StaticNode::new("label", value);

// After
use ranvier_core::StaticAxon;
let node = StaticAxon::new("label", value);
```

### `read_json_file` / `write_json_file`

These internal utility functions have been made `pub(crate)`. If you were using them, replace with direct `serde_json` calls:

```rust
// Before
use ranvier_core::read_json_file;
let data = read_json_file("config.json")?;

// After
let content = std::fs::read_to_string("config.json")?;
let data: MyConfig = serde_json::from_str(&content)?;
```

---

## Step 3: Axon Serde Bounds

`Axon<In, Out, E, Res>` requires all type parameters (except `Res`) to implement `Serialize + DeserializeOwned`. This has been the case since 0.15.0 but is now strictly enforced.

**Common issues:**

| Type | Problem | Fix |
|---|---|---|
| `std::convert::Infallible` | Not `Serialize` | Use `String` for error type |
| `&'static str` | Not `DeserializeOwned` | Use `String` for output type |
| Custom types | Missing derives | Add `#[derive(Serialize, Deserialize)]` |

```rust
// Before (won't compile)
Axon::<(), (), Infallible, ()>::new("MyFlow")

// After
Axon::<(), (), String, ()>::new("MyFlow")
```

For transitions:

```rust
// Before
impl Transition<(), &'static str> for MyTransition {
    type Error = Infallible;
    // ...
}

// After
impl Transition<(), String> for MyTransition {
    type Error = String;

    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<String, String> {
        Outcome::next("result".to_string())
    }
}
```

---

## Step 4: Rust Edition

Ranvier 0.16 requires **Rust 1.93.0+** with **Edition 2024**. Update your `Cargo.toml`:

```toml
[package]
edition = "2024"
rust-version = "1.93.0"
```

**Edition 2024 changes that may affect your code:**
- `std::convert::Infallible` is no longer in the prelude — add `use std::convert::Infallible;` if needed
- Let-chain syntax is stable

---

## Step 5: Workspace Lints (Optional)

If you maintain a workspace, consider adopting Ranvier's lint configuration:

```toml
[workspace.lints.clippy]
type_complexity = "allow"       # Tower service signatures
too_many_arguments = "allow"    # Runtime execution functions
collapsible_if = "allow"        # Edition 2024 pattern
result_large_err = "allow"      # HttpResponse is large
should_implement_trait = "allow" # SSE builder pattern
large_enum_variant = "allow"    # Trigger enum variants
```

Per-crate:
```toml
[lints]
workspace = true
```

---

## Extension Crate Compatibility

All extension crates are version-synchronized. When upgrading, update all `ranvier-*` dependencies to 0.16 simultaneously.

| Crate | 0.15 → 0.16 Changes |
|---|---|
| `ranvier-core` | Deprecated items removed |
| `ranvier-runtime` | No API changes |
| `ranvier-http` | No API changes |
| `ranvier-guard` | No API changes |
| `ranvier-auth` | No API changes |
| `ranvier-session` | No API changes |
| `ranvier-db` | No API changes |
| `ranvier-observe` | No API changes |
| `ranvier-openapi` | No API changes |
| `ranvier-grpc` | No API changes |
| `ranvier-graphql` | No API changes |
| `ranvier-job` | No API changes |
| All others | No API changes |

---

## Getting Help

- [GitHub Issues](https://github.com/ranvier-rs/ranvier/issues) — Bug reports and feature requests
- [CONTRIBUTING.md](./CONTRIBUTING.md) — How to contribute
- [Plugin Architecture](../docs/05_dev_plans/plugin_architecture_m177.md) — Extension development guide
