# Ranvier Full-Stack Demo (Same-Process Static Serving)

This experimental demo shows a single Ranvier backend process serving both:

1. API route: `POST /api/order`
2. Frontend files from the repository via `.serve_assets("/assets", ...)` + `.serve_spa_shell(...)`

It no longer relies on an external reverse proxy path for local full-stack wiring.

## Run

```bash
cargo run -p fullstack-demo
```

Then open:

- `http://127.0.0.1:3030`

The page is served from frontend files stored under `embedded/`, but the serving
path is currently **file-backed** (`.serve_assets()` + `.serve_spa_shell()`),
not compile-time embedded asset packaging. The page calls `POST /api/order` on
the same origin.

## What This Validates

1. Same-process static file serving in `HttpIngress` (`.serve_assets()`).
2. SPA fallback for deep links (`.serve_spa_shell()`).
3. API and static path co-existence in one process.
