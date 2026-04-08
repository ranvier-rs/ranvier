# Reference Fullstack Admin

Public-only fullstack reference app with a Ranvier backend and a SvelteKit frontend.

This example is intentionally separate from `playground/` and is designed as a
small public reference app, not an internal migration artifact.

## Surfaces

Backend:
- `POST /login`
- `GET /dashboard`
- `GET /departments`
- `GET /users`
- `POST /users`
- `PUT /users/:id`
- `DELETE /users/:id`
- `GET /openapi.json`
- `GET /docs`

Frontend:
- `/login`
- `/`
- `/users`

## Run

Backend:

```bash
cargo run -p reference-fullstack-admin
```

Frontend:

```bash
cd frontend
npm install
npm run dev
```

Defaults:
- backend: `http://127.0.0.1:3130`
- frontend dev: `http://127.0.0.1:5176`
- login: `admin` / `admin123`

## Notes

- Uses SQLite in-memory storage for zero-infra local runs.
- Uses permissive CORS in dev mode so the SvelteKit frontend can call the API.
- Uses OpenAPI generation and Swagger UI on the backend.
