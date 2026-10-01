# Mock WebSocket Loop

This finite in-memory source/sink example does not open a WebSocket server.
Run `cargo run -p websocket-loop`. It uppercases three messages, then handles
the exact `exit` message as the local `connection.close` Emit signal.

The Axon is reused, but each message has a fresh Bus. ProcessMessage binds the
message ID in context; Broadcast rejects missing/mismatched context. Empty or
over-4096-byte messages fail before sink delivery. The simulated user is user_1,
not an authenticated connection identity.

Delivery inspects ExecutionReport first. Only Unpersisted with complete cleanup
can map to the local sink. Uncertain/persisted reports, incomplete cleanup,
domain Fault or unexpected control outcomes stop the loop. Close signals do not
enter the sink and later messages are not consumed. This is a local stop, not
a protocol close-frame/handshake acknowledgement.

The mock output queue holds at most 64 responses. Sink capacity/lock failure
returns an error and stops before the next message; no retry is authorized.
A real sink may have performed an effect before returning an error and needs
reconciliation. The example does not log raw message/response/error bodies.

Tests cover fresh message context, stop behavior, bounded input/queue, report
rejection before sink and actual finite main execution. Real WebSocket framing,
TLS/origin/auth, transport backpressure, disconnect/source-error classification,
close handshake, cancellation/reconnect, durable delivery, drain/load and
native/customer release acceptance remain OPEN.
