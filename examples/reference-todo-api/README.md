# Reference Todo API

A complete CRUD application with JWT authentication built on Ranvier.
Demonstrates Axon circuits, Bus dependency injection, HTTP routing, and
`ranvier test` collection-based testing.

## Audience

Use this example when you want:
- the smallest complete JWT CRUD backend in the public example surface
- a reference point before moving to `admin-crud-demo`
- a simple service that still shows route-per-circuit structure clearly

Do not use this example when you need:
- pagination/search
- SQL persistence
- frontend integration
- multi-step business workflows

## Architecture

```
POST /login ──► [login] ──► JWT token
GET  /todos ──► [list_todos] ──► Vec<Todo>
POST /todos ──► [create_todo] ──► Todo
GET  /todos/:id ──► [get_todo] ──► Todo
PUT  /todos/:id ──► [update_todo] ──► Todo
DELETE /todos/:id ──► [delete_todo] ──► { deleted: id }
```

Each endpoint is a single-transition Axon circuit. Shared state
(`Arc<Mutex<Vec<Todo>>>`) is placed on the Bus at startup and
accessed from within transitions.

## Run

```bash
cargo run -p reference-todo-api
```

## When To Use It

| Surface | Use it when |
|---|---|
| Official Track | You are learning CRUD and auth before larger examples |
| Bridge Example | Move to `admin-crud-demo` if you want an admin-style backend |
| Reference Apps | Move to `reference-fullstack-admin` or `reference-ecommerce-order` if you need larger public architecture examples |

## Test with ranvier CLI

```bash
ranvier test .ranvier/collections/todo-crud.json
```

## Environment

| Variable    | Default         | Description       |
|-------------|-----------------|-------------------|
| BIND_ADDR   | 0.0.0.0:3000    | Listen address    |
| JWT_SECRET  | (hardcoded)     | JWT signing key   |
