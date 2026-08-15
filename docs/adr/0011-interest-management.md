# ADR-0011: Interest management and priority accumulation

**Status:** accepted

## Context

Sending every entity to every client is O(entities × clients) bandwidth. At 10,000 entities and 500
clients that is five million entity-updates per snapshot — impossible at any tick rate, and pointless
because a client can only see a small fraction of the world.

Filtering by visibility helps but does not finish the job. Even after filtering, a client in a
crowded area may have 800 relevant entities and a budget for 150 updates this tick. Something must
decide *which* 150, and it must make a different choice next tick so that everything eventually
converges rather than a fixed subset being starved forever.

The naive answers both fail. Round-robin ignores that a duelling opponent matters more than a distant
crate. Strict distance ordering starves everything past the cutoff permanently.

## Decision

Two cooperating mechanisms.

**1. Interest management — what is relevant at all.** A pluggable filter per client:

| Strategy | Behaviour |
|---|---|
| `Everything` | No filtering. Correct default for small sessions; anything else is premature. |
| `GridAoi { cell_size, radius }` | Uniform spatial grid; entities within a cell radius are relevant. O(1) insert and query, ideal for evenly distributed worlds. |
| `SpatialHash { .. }` | Hashed sparse grid for large or unbounded worlds where a dense grid would waste memory. |
| `Custom(fn)` | User predicate — team visibility, room membership, line of sight, gameplay-specific rules. |

Relevance transitions are events, not silent changes: entering relevance produces a spawn with a
full baseline, leaving produces a despawn. Without that, a client's view of a re-entering entity
would be delta-encoded against a baseline it no longer holds.

**2. Priority accumulation — what to send now, within budget.** For each `(entity, client)` pair the
core keeps an accumulator:

```
priority += base_priority × distance_factor × staleness_factor
```

Each tick, relevant entities are sorted by accumulated priority and the highest are packed until the
client's bandwidth budget ([ADR-0010](0010-reliability-channels.md)) is exhausted. Entities that are
sent have their accumulator reset to zero; entities that are not keep accumulating.

The consequence is the important part: **nothing starves.** A low-priority entity's accumulator grows
every tick it is skipped until it eventually outranks the busy ones. The system degrades into
"important things update at full rate, unimportant things update slowly" rather than "unimportant
things never update". This is what makes bounded bandwidth compatible with eventual consistency.

`base_priority` is declared per component in the schema, so a player is naturally more urgent than a
decorative prop, and can be overridden at runtime for gameplay reasons — the objective being
contested, the entity that just fired.

**Room sharding.** Above a configurable entity count, a world may be split into rooms handled by
separate server processes, with handoff at boundaries. Designed in
[`design/interest-management.md`](../design/interest-management.md); implemented in P6.

## Consequences

- Bandwidth per client is bounded by configuration rather than by world size, which is what makes
  the 10k-entity target achievable.
- Behaviour under congestion is graceful and predictable: update *rate* degrades before anything
  disappears.
- The accumulator is per `(entity, client)` pair, so memory is O(relevant entities × clients). For
  large sessions this is the dominant allocation in the replication system and is stored as a compact
  columnar array, not a hash map.
- Priority tuning is genuinely game-specific. Defaults are reasonable; the tuning guide exists
  because the defaults will not be right for every game.
- Relevance transitions cost a full baseline each. Hysteresis on the relevance radius prevents an
  entity oscillating at the boundary from repeatedly paying that cost.

## Alternatives considered

**Send everything, let bandwidth sort it out.** Correct and simple below roughly 50 entities, which
is why `Everything` is the default. Rejected as the only option because it caps the product at small
sessions.

**Pure distance culling with a hard cutoff.** Simple and intuitive. Rejected because it starves
everything beyond the cutoff permanently, so a distant-but-important entity — a sniper, a moving
objective — is simply invisible.

**Round-robin over relevant entities.** Guarantees eventual delivery with no priority bookkeeping.
Rejected because it treats a duel opponent and a distant crate identically, spending scarce bandwidth
on the wrong things exactly when bandwidth is scarce.

**Client-driven subscription — clients request what they want.** Flexible, and shifts policy to the
application. Rejected as a cheat vector in authoritative topologies: a client that can request
arbitrary entities can request the whole map and see through walls. Available inside `Custom` for
topologies where peers are trusted.
