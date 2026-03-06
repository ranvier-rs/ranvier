# Production Config Demo

Demonstrates Ranvier's production configuration management system.

## Features

- **`ranvier.toml`** — File-based configuration with typed schema
- **Environment variable overrides** — `RANVIER_SERVER_PORT=8080` overrides file settings
- **Profile system** — `RANVIER_PROFILE=prod` activates profile-specific overrides
- **Structured logging** — JSON, pretty, or compact log formats via `tracing-subscriber`
- **Graceful shutdown** — Configurable timeout for draining in-flight requests
- **Health checks** — Built-in `/health`, `/ready`, `/live` endpoints

## Configuration Layers

Settings are applied in order (later layers override earlier):

1. **Defaults** — Sensible defaults (`host=127.0.0.1`, `port=3000`, etc.)
2. **`ranvier.toml`** — File found by walking up from the current directory
3. **Profile** — `[profile.prod]` section applied when `RANVIER_PROFILE=prod`
4. **Environment variables** — `RANVIER_*` env vars override all file settings

## Running

```sh
# Default (reads ranvier.toml)
cargo run -p production-config-demo

# Override port
RANVIER_SERVER_PORT=8080 cargo run -p production-config-demo

# Production profile (JSON logs, TLS enabled)
RANVIER_PROFILE=prod cargo run -p production-config-demo
```

## Endpoints

| Method | Path      | Description              |
|--------|-----------|--------------------------|
| GET    | `/`       | Hello message            |
| GET    | `/status` | App status + request count |
| GET    | `/health` | Health check             |
| GET    | `/ready`  | Readiness probe          |
| GET    | `/live`   | Liveness probe           |

## Environment Variables

| Variable                              | Description                |
|---------------------------------------|----------------------------|
| `RANVIER_SERVER_HOST`                 | Bind host                  |
| `RANVIER_SERVER_PORT`                 | Bind port                  |
| `RANVIER_SERVER_SHUTDOWN_TIMEOUT_SECS`| Graceful shutdown timeout  |
| `RANVIER_LOGGING_FORMAT`              | `json` / `pretty` / `compact` |
| `RANVIER_LOGGING_LEVEL`              | Global log level           |
| `RANVIER_TLS_ENABLED`                | Enable/disable TLS         |
| `RANVIER_TLS_CERT_PATH`             | Certificate PEM path       |
| `RANVIER_TLS_KEY_PATH`              | Private key PEM path       |
| `RANVIER_PROFILE`                    | Active profile name        |
