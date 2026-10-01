# Core NATS Pub/Sub Demo

Run a NATS broker, set RANVIER_ECOSYSTEM_NATS_URL if needed, and run
`cargo run -p ecosystem-nats-demo`. The default endpoint is localhost:4222.
Connection failure is an error, not a successful skipped demonstration.

The demo uses unique Core NATS subjects, confirms subscriptions with flush,
publishes two finite inputs and observes the exact projected outputs. Each
execution uses a fresh Bus with typed route context. Invalid format, missing
route, oversized (64 KiB) event or invalid UTF-8 is an explicit failure; invalid
bytes are not represented as source closure. Raw event bodies are not logged.

Only an Unpersisted ExecutionReport with complete cleanup maps to publication.
Other reports require reconciliation without automatic replay. Publication/flush
failure or timeout may follow a send, so they also require reconciliation.
Observed output and flush are not durable effect confirmation.

This is Core NATS, not JetStream: no durable consumer ACK, persistence, recovery
ledger or exactly-once contract is implemented. No message replay guarantee is
implied. TLS/auth/tenant policy, durable event identity, redelivery/idempotency,
dead-letter repair, reconnect/load/drain, real crash/restore and native/customer
acceptance remain OPEN. Establish authority contracts before adding recovery.

Default tests exercise pure transitions and report policy. The ignored live test
requires an explicitly configured disposable broker and checks the real finite
demo plus invalid-UTF-8 source classification:

```sh
cargo test -p ecosystem-nats-demo -- --include-ignored
```
