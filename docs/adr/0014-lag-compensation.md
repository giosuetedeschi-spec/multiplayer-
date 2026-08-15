# ADR-0014: Server-side rewind for hit resolution

**Status:** accepted

## Context

Client prediction and interpolation ([ADR-0012](0012-prediction-and-reconciliation.md)) leave a
specific, unavoidable inconsistency: a client renders remote entities in the past, by
`interp_delay + RTT/2` — commonly 100–150 ms.

So when a player aims at an opponent and fires, they aim at where that opponent was 130 ms ago. If
the server resolves the shot against present-time positions, the shot misses. The player saw their
crosshair on the target, saw the shot land, and the server disagreed. This is the single most
complained-about phenomenon in online shooters, and it is not a bug — it is the geometry of the
situation.

Someone has to be wrong. The question is who, and it is a design decision rather than a technical
one.

## Decision

**Server-side rewind.** The server reconstructs the world as the shooter saw it and resolves the hit
there.

The server maintains a ring buffer of historical arena states — cheap, because it is the same
snapshot mechanism rollback and persistence already use. Each client's view latency is tracked
continuously from RTT and its declared interpolation delay. When a client submits a hit query, the
server rewinds to that client's view time and resolves against those positions.

```rust
let view_tick = server.view_tick_for(client);
let hit = world.rewind_to(view_tick, |past| past.raycast(origin, direction));
```

Rewinding is scoped to the query and does not affect authoritative state. Only entities relevant to
the query are rewound, not the whole world.

**Bounds, because this is an attack surface.** A client that could claim arbitrary view latency could
shoot into the distant past.

- Maximum rewind is capped (default 250 ms). Beyond it, resolution uses the oldest available state.
- Claimed view time is validated against server-measured RTT; implausible claims are clamped and
  logged.
- The history buffer is bounded by the same cap.

**Documented, not hidden.** Lag compensation makes one party's experience correct at the other's
expense, and the trade is visible in gameplay. The consequence — occasionally being shot after
reaching cover, because on the shooter's screen you had not — is inherent to choosing the shooter.
[`design/lag-compensation.md`](../design/lag-compensation.md) explains it, and the cap is the knob
that bounds how bad it can get.

Configurable per hit type: hitscan weapons rewind fully, projectiles are usually spawned at
rewound-time and then simulated forward, and melee often does not rewind at all.

## Consequences

- Shots land where players aimed. This is the difference between a shooter feeling responsive and
  feeling broken.
- The victim can be hit after reaching cover on their own screen, bounded by the rewind cap. This is
  the deliberate trade and it is documented in player-facing terms so studios can explain it.
- Memory is `rewind_cap × tick_rate × arena size`. At 250 ms, 60 Hz and a 4 MB arena that is roughly
  60 MB per session — significant, and the reason the cap is a configuration value rather than
  generous by default.
- Rewind cost is per query, so a shotgun firing twelve pellets rewinds once and resolves twelve rays
  against the same reconstructed state. The API takes a closure specifically to make that the natural
  usage.
- Only meaningful in authoritative topologies. In mesh topologies rollback already gives every peer a
  consistent view, so lag compensation is neither needed nor available.

## Alternatives considered

**No lag compensation; resolve against present positions.** Simple, and the server is unambiguously
correct. Rejected: it makes every shot at a moving target require leading by an amount the player
cannot compute, which players experience as the game not registering their hits.

**Client-authoritative hit detection** — the client reports what it hit. Perfectly accurate from the
shooter's perspective and free of server cost. Rejected as an open cheat: it is the classic aimbot
vector, since a modified client simply reports hits that never happened.

**Client-side interpolation delay of zero; extrapolate remote entities instead.** Removes the
interpolation component of view latency. Rejected because extrapolation is wrong whenever a player
changes direction, replacing a consistent small offset with unpredictable large errors, and the RTT
component of the problem remains anyway.

**Rewind the entire world rather than query-relevant entities.** Simpler and obviously correct.
Rejected on cost at high entity counts; scoped rewind gives the same answer for hit queries at a
fraction of the work.
