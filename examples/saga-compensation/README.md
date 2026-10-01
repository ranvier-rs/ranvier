# Saga Compensation Example

Run `cargo run -p saga-compensation` or `cargo test -p saga-compensation --locked`.

The example registers inventory release and refund with `then_compensated` and
`SagaPolicy::Enabled`. Runtime invocation records drive rollback in reverse
order. Hooks consult the actual local ledger rather than fabricated snapshots.

A failed forward step may already have caused an effect, so runtime can invoke
its compensation. Hooks safely no-op when no local effect occurred, reject
another order and avoid repeating completed local reversal. Tests inject refund
Fault and a test-only panic and inspect the owned cleanup report.

Incomplete cleanup and non-local durability require reconciliation without
automatic retry. Local hook completion does not certify external refund or
durable completion. Resources is a fresh in-memory fixture for one invocation;
do not share it across independent workflows. Production requires a shared
authoritative ledger, tenant/amount binding, atomic reservation, idempotency and
crash/readback recovery. Payment amounts use integer cents.
