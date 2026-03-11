# Auth Tower Integration Example (Ecosystem Way)

**Alternative approach**: Tower Service layer integration for authentication.

---

## What This Example Demonstrates

This example shows **how to integrate Tower middleware with Ranvier** for authentication:

1. **Tower HTTP middleware** (`tower-http::auth::RequireAuthorizationLayer`)
2. **Custom `AuthorizeRequest` implementation** (high-level API)
3. **Manual Layer + Service implementation** (low-level, educational)
4. **Tower + Ranvier integration pattern** (Tower validates, Ranvier handles business logic)
5. **Request extensions** (Tower stores `AuthContext` in `request.extensions()`, not Bus)

---

## Why Tower Integration? (Ecosystem Way)

### ✅ **Ecosystem Compatibility**

Leverage battle-tested `tower-http` middleware without modification:

```rust
use tower::ServiceBuilder;
use tower_http::{cors::CorsLayer, trace::TraceLayer, timeout::TimeoutLayer};

let service = ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .layer(TraceLayer::new_for_http())
    .layer(TimeoutLayer::new(Duration::from_secs(30)))
    .layer(jwt_auth_layer(secret))  // Your custom auth layer
    .service(ranvier_adapter);
```

**Benefits**:
- **Reuse existing layers**: CORS, Trace, Timeout, RateLimit, Compression
- **No reinvention**: Don't rewrite what Tower already does well
- **Proven in production**: Tower powers many Rust web services

### ✅ **Team Knowledge Transfer**

If your team already knows Tower, minimal learning curve:

```rust
// Familiar Tower pattern
impl<B> AuthorizeRequest<B> for JwtAuthorizer {
    fn authorize(&mut self, request: &mut Request<B>) -> Result<(), Response<Self::ResponseBody>> {
        // Standard Tower auth logic
        let auth_header = request.headers().get("authorization")?;
        let token = extract_bearer_token(auth_header)?;
        let auth_ctx = validate_jwt(token, &self.secret)?;
        request.extensions_mut().insert(auth_ctx);
        Ok(())
    }
}
```

**Benefits**:
- **Familiar patterns**: Team experience with Tower is directly applicable
- **Less training**: No need to learn Ranvier auth patterns for Tower-based auth
- **Gradual migration**: Keep Tower auth while adding Ranvier for business logic

### ✅ **Gradual Migration Path**

If you have an existing Tower app, migrate incrementally:

**Stage 1**: Keep Tower for HTTP concerns (CORS, auth, trace)
```rust
let service = ServiceBuilder::new()
    .layer(cors_layer)
    .layer(jwt_auth_layer)
    .service(existing_tower_app);  // Still pure Tower
```

**Stage 2**: Add Ranvier for new business logic
```rust
let service = ServiceBuilder::new()
    .layer(cors_layer)
    .layer(jwt_auth_layer)
    .service(ranvier_adapter);  // New endpoints use Ranvier
```

**Stage 3**: Gradually move business logic to Ranvier transitions
```rust
// Old: Tower Service
async fn handler(req: Request) -> Response { ... }

// New: Ranvier Transition
#[transition]
async fn handler(input: Req, res: &(), bus: &mut Bus) -> Outcome<Resp, E> { ... }
```

---

## Trade-offs

### **Pros** ✅

- **Reuse Tower ecosystem**: CORS, Trace, Timeout, RateLimit, Compression
- **Team knowledge**: Existing Tower experience is directly applicable
- **Battle-tested**: Tower middleware is proven in production
- **Gradual migration**: Keep Tower auth while adding Ranvier incrementally

### **Cons** ❌

- **Not visualized in Schematic**: Tower layers are opaque in Ranvier's `schematic.json`
  - You can't see auth flow in VSCode Circuit view
  - Debugging requires understanding Tower middleware chain

- **AuthContext not in Bus**: Tower stores context in `request.extensions()`
  - Requires adapter to extract from extensions → put in Bus
  - Transitions can't directly access `AuthContext` from Bus (unless adapter does it)

- **More boilerplate**: Custom Tower layers require more code than Ranvier transitions
  - Low-level: 150+ lines for manual Layer + Service
  - High-level: 50+ lines for `AuthorizeRequest` implementation
  - Compare with Ranvier transition: 20 lines

---

## When to Use This Approach

**Choose Tower integration** if:
- You have an **existing Tower app** and want to add Ranvier gradually
- Your team **already knows Tower** and wants to leverage that knowledge
- You need **specific Tower middleware** that Ranvier doesn't provide (e.g., custom CORS, rate limiting)
- You're **migrating from another Rust web framework** (actix-web, axum) that uses Tower

**Choose pure Ranvier** (`examples/auth-transition/`) if:
- You're **starting a new project** and want full Ranvier benefits
- You want **Schematic visualization** of auth flow in VSCode
- You prefer **Bus-based context propagation** (type-safe, explicit)
- You want **easier testing** (unit test transitions independently)

**See comparison**: [`docs/guides/auth-comparison.md`](../../docs/guides/auth-comparison.md)

---

## How to Run

### 1. Build

```bash
cd examples/auth-tower-integration
cargo build
```

### 2. Run the example

```bash
cargo run
```

The example will demonstrate:
- ✅ Valid admin token → Tower validates, Ranvier handles request → Success
- ❌ Invalid token → Tower rejects (returns 401) → Never reaches Ranvier

### 3. (Optional) Set custom JWT secret

```bash
export JWT_SECRET="your-secret-key"
cargo run
```

---

## Expected Output

```
INFO Starting auth-tower-integration example (Tower + Ranvier)
INFO Tower auth layer configured (JWT validation)
INFO Ranvier pipeline configured (business logic)

In production, you'd wrap this with Tower ServiceBuilder:
  ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .layer(jwt_auth_layer(secret))
    .service(ranvier_adapter)

=== Demo Execution ===

Scenario 1: Tower validates token, Ranvier handles request
INFO User authenticated successfully (Tower verified token), user_id="alice"
✅ Success: {"message":"Hello, alice! (Verified by Tower)","user_id":"alice","roles":["admin","user"],"timestamp":"2026-03-11T...","verified_by":"Tower middleware"}

Scenario 2: Tower rejects invalid token (doesn't reach Ranvier)
  In production, Tower returns 401 before calling Ranvier
  Ranvier pipeline never executes
```

---

## Code Structure

```
examples/auth-tower-integration/
├── Cargo.toml          # Dependencies (ranvier, tower, tower-http, jsonwebtoken)
├── src/
│   ├── main.rs         # Tower + Ranvier integration pattern
│   ├── tower_auth.rs   # Two Tower auth implementations (high-level + low-level)
│   └── auth.rs         # AuthContext, AuthError, JWT validation
├── README.md           # This file
├── README.ko.md        # Korean version
└── .env.example        # JWT secret example
```

### Two Tower Integration Approaches

This example demonstrates **two ways** to integrate Tower auth:

#### **Option A: Manual Layer + Service (Educational)**

Low-level implementation showing Tower internals:

```rust
pub struct AuthLayer { secret: String }
pub struct AuthService<S> { inner: S, secret: String }

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        AuthService { inner, secret: self.secret.clone() }
    }
}

impl<S, B> Service<Request<B>> for AuthService<S>
where
    S: Service<Request<B>, Response = Response<BoxBody>>,
{
    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        // JWT validation logic...
        self.inner.call(req)
    }
}
```

**When to use**: Educational purposes, full control over request/response flow.

#### **Option B: AuthorizeRequest Trait (Recommended)**

High-level API using `tower-http::auth::RequireAuthorizationLayer`:

```rust
#[derive(Clone)]
pub struct JwtAuthorizer {
    pub secret: String,
}

impl<B> AuthorizeRequest<B> for JwtAuthorizer {
    type ResponseBody = String;

    fn authorize(&mut self, request: &mut Request<B>) -> Result<(), Response<Self::ResponseBody>> {
        let auth_header = request.headers().get("authorization")...;
        let token = extract_bearer_token(auth_header)?;
        let auth_ctx = validate_jwt(token, &self.secret)?;
        request.extensions_mut().insert(auth_ctx);
        Ok(())
    }
}

pub fn jwt_auth_layer(secret: String) -> RequireAuthorizationLayer<JwtAuthorizer> {
    RequireAuthorizationLayer::new(JwtAuthorizer { secret })
}
```

**When to use**: Production (recommended), less boilerplate, easier to maintain.

---

## Next Steps

### Extend This Example

1. **Add CORS layer**
   ```rust
   use tower_http::cors::CorsLayer;

   let service = ServiceBuilder::new()
       .layer(CorsLayer::permissive())
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

2. **Add request tracing**
   ```rust
   use tower_http::trace::TraceLayer;

   let service = ServiceBuilder::new()
       .layer(TraceLayer::new_for_http())
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

3. **Add rate limiting** (using `tower::limit::RateLimitLayer`)
   ```rust
   use tower::limit::RateLimitLayer;
   use std::time::Duration;

   let service = ServiceBuilder::new()
       .layer(RateLimitLayer::new(100, Duration::from_secs(60)))
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

4. **Add timeout protection**
   ```rust
   use tower_http::timeout::TimeoutLayer;

   let service = ServiceBuilder::new()
       .layer(TimeoutLayer::new(Duration::from_secs(30)))
       .layer(jwt_auth_layer(secret))
       .service(ranvier_adapter);
   ```

### Compare Approaches

- **Pure Ranvier approach**: See [`examples/auth-transition/`](../auth-transition/)
  - Demonstrates Transition-based auth (recommended for new projects)
  - Bus-based context propagation, Schematic visualization, easier testing

- **Detailed comparison**: See [`docs/guides/auth-comparison.md`](../../docs/guides/auth-comparison.md)
  - Feature comparison table
  - Performance analysis
  - Migration paths (Tower → Ranvier, Ranvier → Tower)
  - When to use which approach

### Production Considerations

When deploying to production:

1. **Use environment variables** for JWT secret (never hardcode)
2. **Add HTTPS** to prevent token interception (use TLS termination at load balancer)
3. **Token revocation** (blacklist/database check in Tower middleware)
4. **Token expiration** (short-lived access tokens: 15 minutes, long-lived refresh: 7 days)
5. **Rate limiting** (prevent brute-force attacks on auth endpoints)
6. **CORS configuration** (if serving web clients from different domains)
7. **Metrics** (track auth success/failure rates with `tower-http::metrics`)
8. **Alerting** (unusual auth patterns, high failure rates)

---

## Related Documentation

- [PHILOSOPHY.md](../../PHILOSOPHY.md) — "Opinionated Core, Flexible Edges" principle
- [DESIGN_PRINCIPLES.md](../../DESIGN_PRINCIPLES.md) — Architecture decision records
- [docs/guides/auth-comparison.md](../../docs/guides/auth-comparison.md) — Tower vs Ranvier auth comparison
- [`examples/auth-transition/`](../auth-transition/) — Pure Ranvier approach (recommended)

---

## License

This example is part of the Ranvier project and follows the same license.
