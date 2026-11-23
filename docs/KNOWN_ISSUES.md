# Known Issues — next release after 0.51.x

This page belongs to the `[Unreleased]` section of the [CHANGELOG](CHANGELOG.md).
It lists what happened to the issues documented for 0.51.x and the limits
that still apply in the next release. The 0.51.x page itself stays on the
`release/0.51` branch
([0.51.x KNOWN_ISSUES](https://github.com/ranvier-rs/ranvier/blob/v0.51.1/docs/KNOWN_ISSUES.md)).

All entries apply when a `PersistenceHandle` is inserted into the Bus. Circuits
that do not use persistence are not affected.

## Upgrading from 0.51.x

Stores written by 0.51.x (store format version 1) are not supported and not
migrated. `ensure_schema()` and the first operation of each store refuse them
without writing anything. Start the new release on a new or emptied
PostgreSQL schema or Redis keyspace. Compensation markers written by 0.51.x
are ignored. See "store format check at startup" and "compensation marker
keys, version 2" in the CHANGELOG.

## Status of the 0.51.x issues

| 0.51.x issue | Status in this release | CHANGELOG entry |
|---|---|---|
| Resuming a trace loses the re-run node's row (PostgreSQL) | **Fixed.** Events are keyed by `(trace_id, event_sequence)` instead of `(trace_id, step)`, so a re-run node's event is stored next to the `Enter` event at the same step. | Store format check at startup; stored record fields |
| Resume prefers fresh input over the stored checkpoint | **Changed.** The resumed step receives its stored checkpoint. A continuation with a different input, or a checkpoint that does not decode, returns `BlockedBeforeEffect` before any event is written. | Typed resume uses the stored checkpoint |
| Compensation idempotency key ignores the fault step | **Fixed.** With a trace store, the marker is keyed by the journal sequence of the compensation's intent; without one, by the fault step and kind. | Compensation marker keys, version 2 |
| PostgreSQL does not store `node_id` | **Fixed.** The events table has a `node_id` column and loaded events carry it. | Store format check at startup |
| Storage failures are only logged | **Fixed.** `Axon::execute` returns an `ExecutionReport`; an unacknowledged write is reported as `Unconfirmed` or `BlockedBeforeEffect` instead of a log line. | Execution returns a report |
| Compensation is not atomic | **Narrowed, still applies.** See below. | Concurrent writers; compensation marker keys, version 2 |

## Compensation is still not exactly-once

- **Affects:** all stores.
- **What changed:** every store write is conditional on the trace's revision,
  so of two concurrent writers one receives `PersistenceConflict` and the
  trace is unchanged. A compensation whose intent was recorded but whose
  result was not (for example after a crash) is not run again: the next
  invocation of the trace is blocked with `PendingIntent` until an operator
  reconciles it.
- **What still happens:** the compensation hook may have run once without a
  recorded result. Ranvier cannot tell whether the external effect happened.
- **Workaround:** make compensation hooks idempotent and pass
  `CompensationContext::intent_sequence` to the provider as the idempotency
  key. Settle open compensations with the reconciliation procedure in the
  persistence operations runbook.

## Limits in this release

- **No automatic replay or retry.** Writes are never retried automatically,
  and streams are never replayed. A stream dropped before its end leaves its
  intent open, and a later invocation of the trace is blocked with
  `PendingIntent`.
- **Held SSE streams live in memory.** Streams held for recovery review stay
  until they are released or the process exits. After a restart, use the
  trace store and the reconciliation runbook.
- **Supported servers.** Cross-process conditional writes are provided by the
  PostgreSQL adapter (PostgreSQL 16 or later) and the single-node Redis
  adapter (Redis 7.4 or later). `InMemoryPersistenceStore` provides them
  within one process only.
