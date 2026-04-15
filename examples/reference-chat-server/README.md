# Reference Chat Server

A multi-room WebSocket chat server built with Ranvier, demonstrating real-time communication patterns.

## Architecture

```
Client A ──WebSocket──┐
                      │
Client B ──WebSocket──┼──► Ranvier::http()
                      │        │
Client C ──WebSocket──┘        ├── POST /login → LoginTransition
                               ├── GET  /rooms → ListRoomsTransition
                               ├── POST /rooms → CreateRoomTransition
                               ├── GET  /rooms/:id/history → RoomHistoryTransition
                               └── WS   /ws → WebSocketHandler
                                              │
                                              ├── RoomManager (join/leave/broadcast)
                                              ├── TokenStore (JWT-style auth)
                                              └── MessageHistory (in-memory)
```

## Running

```sh
cargo run -p reference-chat-server
```

## REST Endpoints

| Method | Path                  | Auth | Description              |
|--------|-----------------------|------|--------------------------|
| POST   | `/login`              | No   | Get auth token           |
| GET    | `/rooms`              | No   | List public rooms        |
| POST   | `/rooms`              | Yes  | Create a new room        |
| GET    | `/rooms/:id/history`  | No   | Room message history     |
| GET    | `/health`             | No   | Health check             |
| GET    | `/ready`              | No   | Readiness check          |
| GET    | `/live`               | No   | Liveness check           |
| GET    | `/ws`                 | WS   | WebSocket connection     |

## WebSocket Protocol

### Connection

Connect with token from login: `ws://localhost:3000/ws?token=tok_xxx`

Authentication notes:

1. Missing or invalid `?token=` connections receive a structured `error` frame
   and are closed.
2. `POST /rooms` also expects `Authorization: Bearer <token>` on the REST side.
3. This demo keeps auth explicit at the application boundary; it does not hide
   WebSocket auth behind proxy-side session assumptions.

### Client → Server Messages

```json
{"type": "join", "room": "general"}
{"type": "leave", "room": "general"}
{"type": "chat", "room": "general", "message": "Hello!"}
{"type": "typing", "room": "general"}
```

### Server → Client Messages

```json
{"type": "welcome", "user": "alice"}
{"type": "joined", "room": "general", "user": "alice", "count": 3}
{"type": "left", "room": "general", "user": "bob", "count": 2}
{"type": "message", "room": "general", "user": "alice", "message": "Hello!", "timestamp": "..."}
{"type": "history", "room": "general", "messages": [...]}
{"type": "room_list", "rooms": [...]}
{"type": "error", "code": "auth_failed", "detail": "..."}
```

## Configuration

Uses `ranvier.toml` (M220 config system). See `ranvier.toml` in this directory.

Useful env overrides for local operability checks:

- `RANVIER_SERVER_PORT`
- `RANVIER_SERVER_HOST`
- `RANVIER_INSPECTOR_PORT`

## Operability Notes

1. The server exposes `/health`, `/ready`, and `/live` and uses
   `graceful_shutdown(config.shutdown_timeout())` for controlled shutdown.
2. WebSocket upgrades must preserve `Connection: upgrade` and `Upgrade:
   websocket` through any reverse proxy.
3. This demo does not emit application-level ping/heartbeat messages. If your
   proxy/load balancer has aggressive idle timeouts, configure transport-level
   pings or adjust those timeouts explicitly.
4. State is process-local and in-memory. Reconnect behavior on deploy/restart is
   expected; the demo is a backend delivery reference, not a multi-node
   realtime platform.
