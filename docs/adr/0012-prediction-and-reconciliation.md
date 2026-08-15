# ADR-0012: Client prediction with server reconciliation

**Status:** accepted

## Context

In an authoritative topology, a naive client sends an input and waits for the server's response
before showing anything. At 80 ms round trip that is 80 ms between pressing a key and the character
moving. Players perceive input latency above roughly 50 ms as sluggishness and above 100 ms as
brokenness, so the naive design is unshippable for anything with real-time movement.

The server must remain authoritative — that is the entire point of the topology — so the client
cannot simply do what it likes. Something has to reconcile the two.

## Decision

The standard three-part solution, implemented in the core so no user code is required.

**1. Prediction.** The client applies local input to its own entities immediately, using the same
simulation it would run as a server. Input feels instantaneous because locally it *is*
instantaneous. Each predicted tick is recorded as `(tick, input, resulting_state)` in a ring buffer.

**2. Reconciliation.** The server's snapshots are stamped with the last input tick it processed for
that client. On arrival for tick *T*, the client compares the server's authoritative state against
its own recorded prediction for *T*:

- **Match** — discard history up to *T*. Nothing visible happens, which is the overwhelmingly common
  case.
- **Mismatch** — restore state to the server's version at *T*, then re-simulate ticks *T+1* through
  *now* using the buffered inputs. This is the same machinery rollback uses
  ([ADR-0013](0013-rollback-model.md)); reconciliation is rollback with a single authoritative source.

**3. Interpolation for everything else.** Entities the client does not own are rendered in the past
by `interp_delay` (default: two snapshot intervals) and interpolated between the two snapshots
bracketing that render time. This trades a small, constant, unnoticeable delay on remote entities for
completely smooth motion, which is the correct trade — nobody perceives that another player is 100 ms
behind, but everybody perceives stutter.

When snapshots are late, extrapolation continues motion for a **clamped** window (default 150 ms)
before freezing. Unbounded extrapolation produces the characteristic rubber-band snap that is worse
than a brief pause.

**Error smoothing.** A reconciliation correction is applied to simulation state immediately, but the
*rendered* position blends toward it exponentially over ~100 ms. Simulation stays authoritative and
correct while the player sees a smooth adjustment rather than a teleport. Corrections beyond a
threshold snap instead, because smoothing a large error just means being visibly wrong for longer.

## Consequences

- Owned-entity input latency is zero in the common case; the network is invisible when predictions
  hold.
- Predicted entities must use deterministic fixed-point maths
  ([ADR-0002](0002-fixed-point-determinism.md)); otherwise prediction and server state disagree every
  tick and reconciliation runs constantly.
- Prediction is *wrong* when it depends on information the client lacks — another player's action
  that has not arrived. This is inherent, and manifests as the familiar "I was already behind cover"
  disagreement. Lag compensation ([ADR-0014](0014-lag-compensation.md)) addresses the other side of
  the same coin.
- Re-simulation cost is O(ticks since the server's last processed input), which grows with RTT. A
  configurable cap bounds worst-case CPU; exceeding it forces a hard resync.
- The input ring buffer must hold at least `max_rtt × tick_rate` entries. Sized from configuration
  and documented, because getting it wrong produces reconciliation failures only on bad connections.
- Remote entities are always slightly in the past. Any gameplay logic comparing local and remote
  positions must account for it — most importantly hit detection, which is precisely why lag
  compensation exists.

## Alternatives considered

**No prediction; wait for the server.** Simple, always correct, no reconciliation. Rejected: 80 ms of
input latency is unacceptable for real-time movement. It remains the right choice for turn-based and
slow strategy games, and is available by disabling prediction.

**Prediction without reconciliation** — predict and trust the client. Removes an entire subsystem.
Rejected because it is not authoritative at all; the client's word becomes final, which is the thing
the topology exists to prevent.

**Full client-side rollback against the server, always.** Treat every server snapshot as a rollback
regardless of agreement. Simpler control flow — no comparison step. Rejected on cost: re-simulating
every tick when predictions almost always match wastes CPU proportional to RTT, for nothing.

**Interpolation only, no prediction for owned entities.** Perfectly smooth and always
server-consistent. Rejected for the same latency reason as waiting for the server; it is the same
design with better-looking remote entities.
