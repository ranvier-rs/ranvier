# Ranvier Full-Stack Demo (Embedded Static Serving)

This experimental demo shows a single Ranvier backend process serving both:

1. API route: `POST /api/order`
2. Frontend static assets via `.serve_dir("/assets", ...)` + `.spa_fallback(...)`

It no longer relies on an external reverse proxy path for local full-stack wiring.

## Run

```bash
cargo run -p fullstack-demo
```

Then open:

- `http://127.0.0.1:3030`

The page is served from embedded static assets and calls `POST /api/order` on the same origin.

## What This Validates

1. Embedded static serving in `HttpIngress` (`.serve_dir()`).
2. SPA fallback for deep links (`.spa_fallback()`).
3. API and static path co-existence in one process.
