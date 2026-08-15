# ADR-0013: GGPO-class rollback over core-owned state

**Status:** accepted

## Context

For competitive games — fighting games above all — even the prediction-and-reconciliation model of
[ADR-0012](0012-prediction-and-reconciliation.md) is not enough, because it only predicts *your own*
inputs. Remote players still arrive late, so their actions appear delayed, and in a genre decided by
frame-accurate reactions that delay is the game.

Rollback, as popularised by GGPO, addresses this by predicting *everyone's* inputs and correcting
afterwards. It is well understood in the fighting game community and essentially unavailable outside
purpose-built engines, because it demands two things most frameworks cannot provide: bit-identical
determinism, and state save/restore fast enough to run many times per second.

`tempo` has both, and not by coincidence. Fixed-point maths ([ADR-0002](0002-fixed-point-determinism.md))
supplies determinism; the core-owned arena ([ADR-0001](0001-core-owns-replicated-state.md)) makes
save/restore a memcpy. Rollback is the feature those two decisions were made to enable.

## Decision

A full GGPO-class rollback implementation in `tempo-rollback`, available in mesh and listen-server
topologies, and reused as the reconciliation engine in dedicated topologies.

**Input prediction.** When a remote peer's input for tick *N* has not arrived, predict it by
repeating their last known input. For human-controlled characters this is correct the large majority
of the time, because inputs are held across many frames.

**Confirmed frame tracking.** The confirmed frame is the newest tick for which every peer's real
input is known. State at or before it is final. Everything after is speculative.

**Saved state ring.** Arena snapshots for every tick in the rollback window, in a preallocated ring
buffer. No allocation occurs during rollback — allocation in the rollback path would make frame times
unpredictable precisely when they matter.

**Rollback and re-simulate.** When a real input arrives that contradicts a prediction for tick *N*:
restore the arena to *N*, then re-simulate to the present with corrected inputs. At 60 Hz with a
7-frame window that is up to seven simulation steps in one frame, which is why the simulation step
must be cheap and allocation-free.

**Input delay.** A configurable delay (typically 1–3 frames) applied to *local* input, buying time
for remote inputs to arrive and reducing rollback frequency. The classic trade: a little uniform
latency against fewer visual corrections. Exposed as a tunable because the right answer is
genre-specific, and can adapt automatically to measured RTT.

**Max rollback window.** Bounded (default 8 frames). Beyond it, the session stalls briefly rather
than rolling back further — an unbounded window means unbounded frame time.

**Desync detection.** BLAKE3 hash of the arena per tick, exchanged between peers and compared at the
confirmed frame. Divergence is detected within a tick.

**Sync-test mode.** A development mode that rolls back and re-simulates *every* frame, comparing
hashes against the original run. Any nondeterminism in user code — a `HashMap` iteration, a
wall-clock read, an uninitialised field, a stray float — surfaces immediately on the developer's
machine. This is the single most valuable debugging tool in a rollback system and is why it ships as
a first-class mode rather than a test helper.

## Consequences

- Rollback becomes available to every language `tempo` supports, which is genuinely unusual.
- Simulation code must be deterministic and free of side effects. The rules — no wall clock, no
  unseeded randomness, no unordered iteration, no I/O — are documented in
  [`design/rollback.md`](../design/rollback.md) and enforced in practice by sync-test mode.
- Visual artifacts are inherent. A mispredicted remote input produces a correction the player can
  see. The rollback community's accumulated wisdom — animations tolerate correction, hit sparks and
  audio should be deferred to confirmed frames — is documented rather than papered over.
- Memory is `window × arena size`. At 8 frames and a 4 MB arena that is 32 MB, which is fine; at 10k
  entities it is not, which is why rollback is for small-session topologies and dedicated
  server-authoritative play uses reconciliation instead.
- Peak frame cost is `window × tick cost`. Simulation must be fast enough that eight steps fit in
  one frame budget.
- Side effects in user code — spawning particles, playing audio — must be deferred to confirmed
  frames or they fire repeatedly during re-simulation. The API provides a confirmed-frame event
  channel for exactly this.

## Alternatives considered

**Delay-based netcode** — hold local input until remote input arrives. Trivial, deterministic, no
rollback machinery. Rejected as the primary model: it converts network latency directly into input
latency for every player, which is the problem rollback exists to solve. It is available as
`input_delay` with rollback disabled, and is the right choice for slower genres.

**Rollback in the host language rather than the core.** Would let users snapshot native objects.
Rejected because it requires each of six languages to implement fast, correct, allocation-free state
save/restore — the exact duplication ADR-0001 exists to avoid.

**Unbounded rollback window.** Never stalls, always correct. Rejected because worst-case frame time
becomes unbounded; a peer that vanishes for two seconds would trigger a 120-frame re-simulation and a
visible freeze. Bounded windows with a stall are more predictable.

**Rollback everywhere, including large authoritative sessions.** Conceptually uniform. Rejected on
memory and CPU: at 10k entities the snapshot ring and re-simulation cost are both prohibitive, and
reconciliation achieves the necessary result there at a fraction of the cost.
