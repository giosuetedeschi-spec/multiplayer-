# ADR-0007: Ephemeral core, pluggable durability

**Status:** accepted

## Context

Two of the four target workloads imply state that outlives a process. A persistent-world MMO cannot
lose everything on deploy, and collaborative non-game sync ([ADR-0023](0023-eventual-sync-mode.md))
is meaningless if the document vanishes when the server restarts.

That pulls toward making the replicated store durable — the SpacetimeDB direction, where the
database *is* the game state, with a write-ahead log, transactions and queries. It is a genuinely
strong product idea and would make "nothing else needed" literally true.

It would also change what this project is. Storage engines are a specialty: durability guarantees,
fsync semantics, crash consistency, compaction, index maintenance, backup and restore. Every hour
spent there is an hour not spent on rollback and interest management, and the resulting core would
be slower on the hot path because durability constrains layout.

The hot path matters here more than usual: the arena layout exists to make replication sweeps
cache-friendly and rollback a memcpy ([ADR-0001](0001-core-owns-replicated-state.md)). Bolting
transactional durability onto that is not additive.

## Decision

**The core is in-memory and fast. Durability is a layer above it, and it is optional.**

`tempo-persist` provides:

- **World snapshots.** The arena serialised with its schema ID, written on an interval or on demand.
  Cheap because it is the same mechanism rollback already uses.
- **An append-only event log.** Inputs and authoritative commands appended per tick. With
  fixed-point determinism, snapshot + subsequent log replays to *exactly* the state that existed —
  which is worth stating plainly, because it means the log is a complete recovery mechanism rather
  than a partial one.
- **Crash recovery.** On start, load the newest valid snapshot and replay the log after it.
- **Storage adapters** behind a small trait: filesystem, S3-compatible object storage, Postgres,
  Redis. Users implement the trait for anything else.
- **Point-in-time restore**, since it is nearly free once snapshot-plus-log exists: pick a tick, load
  the preceding snapshot, replay to it.

What we deliberately do **not** provide: transactions, secondary indexes, a query language, or
multi-writer concurrency control. Applications that need those run a real database alongside and use
`tempo` for the realtime layer.

## Consequences

- The hot path stays free of durability concerns; no fsync in the tick loop.
- Determinism pays a second dividend: it makes the event log an exact recovery mechanism, not a
  best-effort one.
- Snapshot-plus-log replay is O(ticks since snapshot), so snapshot interval is a tunable recovery-
  time-versus-write-volume trade. Documented in [`ops/persistence.md`](../ops/persistence.md).
- Users needing queryable state must run a database. "Nothing else needed" is honest about
  *networking*, not about being a database.
- Log volume at 10k entities and 60 Hz is substantial. The log records inputs and commands, not
  state, which keeps it proportional to player action rather than world size — but it still needs
  retention policy and compaction, and that is documented rather than solved automatically.

## Alternatives considered

**Durable-first, SpacetimeDB style — the replicated store *is* the database.** Real differentiation,
and makes persistent worlds trivial. Rejected on focus and performance: it roughly doubles the core's
scope, permanently commits us to storage-engine problems, and constrains the memory layout that the
rollback and replication designs depend on.

**Strictly ephemeral; persistence entirely the user's problem.** Smallest core, cleanest boundary.
Rejected because every MMO and every collaborative-sync user needs it immediately, and each would
build a worse version of the same snapshot-plus-log mechanism.

**Ephemeral open source, durability as a hosted-only feature.** A clean commercial split. Rejected
because it makes self-hosting second-class, and self-hosting is how anyone will evaluate this at all.
