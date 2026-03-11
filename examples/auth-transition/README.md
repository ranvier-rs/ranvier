# Auth Transition Example (Ranvier Way)

**Recommended approach**: Transition-based authentication using Ranvier's core paradigm.

---

## What This Example Demonstrates

This example shows **how to implement authentication in pure Ranvier** using the Transition/Outcome/Bus paradigm:

1. **JWT authentication** via `#[transition]` macro
2. **Role-based authorization** (RBAC)
3. **Context propagation** through Bus (type-safe, explicit)
4. **Schematic visualization** of the auth flow
5. **Composable pipeline** (easy to add/remove steps)

---

## Why Transition-Based Auth? (Ranvier Recommended)

### ✅ **Bus-Based Context Propagation**

`AuthContext` is automatically stored in the Bus after successful authentication, making it available to all downstream transitions:

```rust
// authenticate transition returns AuthContext → stored in Bus
#[transition]
async fn authenticate(req: Request) -> Outcome<AuthContext, AuthError> {
    let auth_ctx = validate_jwt(token)?;
    Outcome::Next(auth_ctx)  // Stored in Bus automatically
}

// authorize receives &AuthContext → injected from Bus automatically
#[transition]
async fn authorize(auth: &AuthContext) -> Outcome<(), AuthError> {
    if !auth.roles.contains(&"admin".into()) {
        return Outcome::Fault(AuthError::Unauthorized("admin".into()));
    }
    Outcome::Next(())
}
```

**Benefits**:
- **Type-safe**: Compiler ensures `AuthContext` exists before `authorize` runs
- **Explicit**: No hidden globals or request extensions
- **Testable**: Inject mock `AuthContext` into Bus for testing

### ✅ **Schematic Visualization**

The entire auth flow is represented in `schematic.json` and visible in VSCode Circuit view:

```
authenticate → authorize → protected_handler
     ↓              ↓              ↓
AuthContext    Check admin    Use AuthContext
(to Bus)         role          for response
```

**Benefits**:
- **Visual debugging**: See exact flow at a glance
- **Documentation**: Schematic IS the documentation (always up-to-date)
- **Team communication**: Non-technical stakeholders can understand the flow

### ✅ **Testability**

Each transition can be unit-tested independently:

```rust
#[tokio::test]
async fn test_authorize_success() {
    let mut bus = Bus::new();
    bus.write(AuthContext {
        user_id: "alice".into(),
        roles: vec!["admin".into()],
    });

    let result = authorize(&AuthContext { ... }).await;
    assert!(result.is_ok());
}
```

**Benefits**:
- **Fast tests**: No HTTP server needed
- **Isolated tests**: Test auth logic independently from JWT parsing
- **Mock-friendly**: Inject any `AuthContext` for edge cases

### ✅ **Composability**

Easy to extend the pipeline with additional steps:

```rust
// Add audit logging
let pipeline = Axon::simple()
    .pipe(authenticate, audit_log, authorize, protected_handler)
    .build();

// Add subscription check
let pipeline = Axon::simple()
    .pipe(authenticate, check_subscription, authorize, protected_handler)
    .build();

// Parallel checks (role + subscription)
let pipeline = Axon::simple()
    .pipe(authenticate)
    .parallel(authorize, check_subscription)
    .pipe(protected_handler)
    .build();
```

**Benefits**:
- **Incremental feature addition**: Add steps without rewriting existing code
- **Clear intent**: Pipeline structure shows business logic flow
- **Parallel execution**: Run independent checks concurrently

---

## How to Run

### 1. Install Dependencies

```bash
cd examples/auth-transition
cargo build
```

### 2. Run the Example

```bash
cargo run
```

The example runs demonstration scenarios:
- ✅ Valid admin token → Success
- ❌ Valid token, no admin role → Authorization error
- ❌ Missing Authorization header → Authentication error
- ❌ Invalid token → JWT validation error

### 3. (Optional) Set Custom JWT Secret

```bash
export JWT_SECRET="your-secret-key"
cargo run
```

---

## Expected Output

```
INFO Starting auth-transition example (Ranvier Way)
INFO Schematic exported to schematic.json

=== Demo Execution ===

Scenario 1: Valid admin token
INFO User authenticated successfully, user_id="alice", roles=["admin", "user"]
INFO Authorization successful, user_id="alice", role="admin"
INFO Protected handler executed for authenticated user, user_id="alice"
✅ Success: {"message":"Hello, alice!","user_id":"alice","roles":["admin","user"],"timestamp":"2026-03-11T..."}

Scenario 2: Valid token, no admin role
INFO User authenticated successfully, user_id="bob", roles=["user"]
WARN Authorization failed: missing required role, user_id="bob", required_role="admin", actual_roles=["user"]
✅ Expected error: Auth error: Unauthorized: requires role admin

Scenario 3: Missing Authorization header
✅ Expected error: Auth error: Missing authorization header

Scenario 4: Invalid token
✅ Expected error: Auth error: Invalid token: ...
```

---

## Code Structure

```
examples/auth-transition/
├── Cargo.toml          # Dependencies (ranvier, jsonwebtoken, serde, tokio)
├── src/
│   ├── main.rs         # Pipeline definition + demo execution
│   └── auth.rs         # AuthContext, AuthError, JWT validation helper
├── README.md           # This file
├── README.ko.md        # Korean version
└── .env.example        # Example JWT secret
```

---

## Next Steps

### Extend This Example

1. **Add more roles**: Check for specific roles (e.g., `"moderator"`, `"billing"`)
   ```rust
   #[transition]
   async fn require_role(auth: &AuthContext, role: &str) -> Outcome<(), AuthError> {
       if !auth.roles.contains(&role.to_string()) {
           return Outcome::Fault(AuthError::Unauthorized(role.into()));
       }
       Outcome::Next(())
   }
   ```

2. **Add audit logging**: Log all authentication attempts
   ```rust
   #[transition]
   async fn audit_log(auth: &AuthContext, req: &Request) -> Outcome<(), Never> {
       tracing::info!(user_id = %auth.user_id, path = %req.uri, "Auth request");
       Outcome::Next(())
   }
   ```

3. **Add subscription check**: Verify user has active subscription
   ```rust
   #[transition]
   async fn check_subscription(auth: &AuthContext, db: &PgPool) -> Outcome<(), SubError> {
       let sub = fetch_subscription(db, &auth.user_id).await?;
       if !sub.is_active() {
           return Outcome::Fault(SubError::Expired);
       }
       Outcome::Next(())
   }
   ```

4. **Add token refresh**: Handle expired tokens gracefully
   ```rust
   #[transition]
   async fn refresh_if_expired(auth: AuthContext, req: &Request) -> Outcome<AuthContext, AuthError> {
       if auth.is_expired() {
           let new_token = refresh_token(&req)?;
           return Outcome::Next(new_token);
       }
       Outcome::Next(auth)
   }
   ```

### Compare with Other Approaches

- **Tower integration**: See `examples/auth-tower-integration/`
  Demonstrates using Tower Service layers for auth (ecosystem way)

- **Comparison guide**: See `docs/guides/auth-comparison.md`
  Detailed feature comparison, performance analysis, and migration paths

### Production Considerations

When deploying to production:

1. **Use environment variables** for JWT secrets (never hardcode)
2. **Add rate limiting** to prevent brute force attacks
3. **Add token revocation** (check against blacklist/database)
4. **Use HTTPS** to prevent token interception
5. **Set token expiration** (short-lived: 15min access, long-lived: 7d refresh)
6. **Add CORS** if serving web clients from different domains
7. **Add metrics** (authentication success/failure rates)
8. **Add alerting** (unusual authentication patterns)

---

## Related Documentation

- [PHILOSOPHY.md](../../PHILOSOPHY.md) — "Opinionated Core, Flexible Edges" principle
- [DESIGN_PRINCIPLES.md](../../DESIGN_PRINCIPLES.md) — Architecture decision records
- [docs/guides/auth-comparison.md](../../docs/guides/auth-comparison.md) — Transition vs Tower comparison

---

## License

This example is part of the Ranvier project and follows the same license.
