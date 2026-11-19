# Known Issues — Ranvier 0.51.x

This page lists known persistence and compensation issues in the 0.51.x line
that are **not** fixed by a patch release, because fixing them would change a
public API or a stored format. Each entry gives the affected stores and a
workaround. Issues fixed in 0.51.1 are listed in the
[CHANGELOG](CHANGELOG.md#0511--2026-11).

All entries apply when a `PersistenceHandle` is inserted into the Bus. Circuits
that do not use persistence are not affected.

## Resuming a trace loses the re-run node's row (PostgreSQL)

- **Affects:** `PostgresPersistenceStore`.
- **What happens:** when a trace that already has events is executed again
  (after `resume()` or after a run that stopped before completion), the
  `Enter` event is written at the resume step, and the node that runs at that
  step writes its own event at the same step. PostgreSQL keys events on
  `(trace_id, step)`, so the node's row is rejected and only a `warn` log is
  emitted. The trace is missing that node's result.
- **Why not patched:** event steps are schematic node indexes, and
  `resume(trace_id, resume_from_step)` is defined in those terms. Giving every
  event a unique sequence number changes the meaning of `resume`.
- **Workaround:** re-run a failed flow under a new `trace_id`, or use
  `RedisPersistenceStore` when the resume history must be kept (Redis keeps
  rows with duplicate steps).

## Resume prefers fresh input over the stored checkpoint

- **Affects:** all stores. This is the documented 0.51 behavior, listed here
  because it is easy to miss.
- **What happens:** when execution resumes at a node, the input passed to
  `execute` is used if it converts to that node's input type. The stored
  checkpoint value is used only when it does not. A circuit whose node input
  type matches the original input type therefore resumes from the new input,
  not from the stored state.
- **Workaround:** when resuming, pass the input you want the resumed node to
  receive.

## Compensation idempotency key ignores the fault step

- **Affects:** `CompensationIdempotencyHandle` with any store.
- **What happens:** the idempotency key is `trace_id:circuit:fault_kind`, and
  `fault_kind` is always `"Fault"`. If the same trace faults again after a
  resume, the second compensation is treated as already done: the hook does
  not run, but the trace is recorded as `Compensated`.
- **Why not patched:** changing the key format would not match markers already
  stored by 0.51.0, so compensations that already ran could run again after
  upgrading.
- **Workaround:** re-run under a new `trace_id`, or leave out
  `CompensationIdempotencyHandle` and make the compensation hook itself
  idempotent.

## PostgreSQL does not store `node_id`

- **Affects:** `PostgresPersistenceStore`.
- **What happens:** the events table has no `node_id` column, so loaded events
  always have `node_id: None`. Consumers such as the Inspector cannot tell
  which node produced an event. Redis stores the full envelope and is not
  affected.
- **Why not patched:** adding the column needs a schema migration of existing
  tables, which a patch release does not ship.

## Storage failures are only logged

- **Affects:** all stores.
- **What happens:** if appending an event or recording completion fails, the
  runtime logs a `warn` and continues. The caller only receives the
  `Outcome` and cannot tell that persistence failed. A trace can be marked
  complete even though one of its events was not stored.
- **Workaround:** monitor `warn` logs from `ranvier_runtime` persistence, and
  treat the store as best-effort audit data rather than a confirmed record.

## Compensation is not atomic

- **Affects:** all stores.
- **What happens:** compensation checks the idempotency marker, runs the hook,
  then writes the marker. Two concurrent executions can both run the hook,
  and a crash between the hook and the marker write runs the hook again on the
  next attempt. Exactly-once compensation is not guaranteed.
- **Workaround:** write compensation hooks so that running them twice is safe.
