# ADR-0027: Replicated fields are fixed-size

**Status:** accepted

## Context

The world arena stores each component as a flat byte column, `stride` bytes per entity slot
([ADR-0001](0001-core-owns-replicated-state.md)). Snapshot save and restore are then copies of
contiguous buffers, which is what makes rollback ([ADR-0013](0013-rollback-model.md)), lag
compensation ([ADR-0014](0014-lag-compensation.md)) and persistence
([ADR-0007](0007-ephemeral-core-pluggable-durability.md)) affordable from one mechanism.

Variable-length fields break that. A `string` field has no fixed stride, so it needs either an
indirection into a side table or a variable-stride column. Either way:

- A snapshot stops being a flat copy and becomes a copy plus an index rebuild.
- Rollback restore acquires an allocation, in the one code path that must never allocate.
- The per-tick state hash has to hash through indirections rather than over a byte range.
- Delta comparison stops being a byte-range comparison.

Every one of those costs lands on the hot path, in exchange for a feature — replicated strings —
that is common in principle and rare in the tick loop. Player names, chat, and item labels change
seconds or minutes apart, not at 60 Hz.

## Decision

**Only fixed-size field types may be stored in the arena.**

Storable: `bool` (1 byte), `enum` (4), `uint`/`int`/`fx` (8), `vec2` (16), `vec3` (24), `quat` (32).

Not storable: `string`, `bytes`.

`ComponentLayout::new` rejects a variable-length field at **registration** time, not at write time,
so the error names the schema — which is where the mistake actually is — rather than surfacing
later at an arbitrary call site.

Variable-length data remains fully supported outside the arena:

- **Commands and RPCs** carry `string` and `bytes` freely. They travel on the `ReliableOrdered`
  channel ([ADR-0010](0010-reliability-channels.md)), are length-prefixed, and are not part of
  per-tick snapshot state.
- **Connect-time metadata** — player names, cosmetic identifiers, team assignments — is naturally a
  command sent once, not a field diffed every tick.
- **Eventual-sync mode** ([ADR-0023](0023-eventual-sync-mode.md)) has its own storage for the
  `RgaSequence` text CRDT and is not bound by this decision.

## Consequences

- Snapshots stay a flat copy, so the rollback path allocates nothing and the state hash stays a
  hash over a byte range.
- Component stride is known at registration, so a column is one `Vec<u8>` and slot addressing is
  multiplication.
- **A user cannot write `#[derive(Replicate)] struct Player { name: String }`.** This will be
  surprising, and the error message must therefore explain the reason and name the alternative
  rather than simply refusing.
- Fixed-size types cover essentially all *simulated* state, which is what tick-rate replication is
  for. Strings in game state are almost always set once and read thereafter.
- The restriction is revisitable per field: a future `string` field could be stored as an
  `(offset, len)` pair into a side blob, paying the extra copy only for components that use one.
  Deliberately not built now, because the cost would be paid in the design of the hot path rather
  than only at the sites that use it.

## Alternatives considered

**Side table with inline `(offset, len)` handles.** Keeps the column fixed-stride while supporting
variable data. Rejected for v1: the blob has to be copied and compacted alongside every snapshot, so
rollback restore acquires an allocation and the tick-time cost becomes data-dependent. This is the
most likely future implementation if demand justifies it.

**Variable-stride columns.** Most general. Rejected outright — slot addressing becomes a lookup,
which defeats the columnar layout's entire purpose.

**Fixed-capacity inline strings**, e.g. `string(max_len = 32)` stored as 32 bytes inline.
Genuinely tempting: it preserves the flat copy and covers player names, the dominant use. Rejected
because it makes the arena size a function of the declared maximum rather than actual use — a 64-byte
name field across 10,000 entities is 640 KB copied on every snapshot, tens of times per second under
rollback, almost all of it padding. Worth reconsidering with a low cap for specific components.

**Allow them and accept slower snapshots.** Rejected because the cost is not confined to users who
opt in: the snapshot, hash and delta paths would carry the indirection unconditionally, so every
game would pay for a feature most do not use.
