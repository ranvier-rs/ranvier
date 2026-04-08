# Admin CRUD Demo

Admin-style CRUD backend with JWT login, SQLite, pagination, search, and OpenAPI.

This example is the **bridge example** after the official track:

`Hello World` -> `Todo API` -> `Order Pipeline` -> `Admin CRUD Demo`

## Endpoints

Public:
- `POST /login`

Protected:
- `GET /departments`
- `GET /users`
- `POST /users`
- `GET /users/:id`
- `PUT /users/:id`
- `DELETE /users/:id`

Docs:
- `GET /openapi.json`
- `GET /docs`

## Run

```bash
cargo run -p admin-crud-demo
```

Defaults:
- App URL: `http://127.0.0.1:3120`
- Login: `admin` / `admin123`
- JWT secret: `admin-crud-demo-secret` (override with `JWT_SECRET`)
