# ADR-0016: Clients run ahead; the server dilates their clocks

**Status:** accepted

## Context

An authoritative server simulates tick *N* and needs every client's input for tick *N* to have
arrived before it does. If an input arrives late the server must either stall — unacceptable, one
bad connection would freeze everyone — or drop it, which the player experiences as their input being
ignored.

Clients therefore cannot simulate the same tick as the server. They must run *ahead*, sending input
for tick *N* early enough that it lands before the server reaches *N*. How far ahead depends on
one-way latency and jitter, both of which vary continuously as routes change and networks congest.

Too little lead and inputs are dropped. Too much and the player pays unnecessary input latency. The
correct lead is a moving target that must be tracked continuously.

## Decision

**Clients run ahead of the server, and the server continuously nudges each client's clock rate.**

**Estimating the offset.** Ping/pong exchanges piggybacked on ordinary traffic yield RTT samples,
smoothed by an exponentially weighted moving average with a variance estimate for jitter. The
client's target lead is:

```
lead = RTT/2 + jitter_margin + safety_buffer
```

where `jitter_margin` is a multiple of the measured RTT deviation (default 2σ) and `safety_buffer`
is a small configurable constant.

**Time dilation, not clock jumps.** When a client's lead is wrong, it does not jump — jumping means
either replaying or skipping ticks, both visible. Instead the client's tick *rate* is adjusted by a
small percentage (bounded, default ±5%) until the lead is correct. A client running 3 ms too tight
speeds up imperceptibly for a second and arrives at the right lead having skipped nothing.

**The server is the reference.** Server snapshots carry the server tick and the last input tick
processed for that client. The client observes the difference between the input tick the server
consumed and what it sent, which measures the lead *as the server experienced it* — directly, rather
than inferring it from RTT.

**Server-side input buffer.** A small jitter buffer (default 2 ticks) absorbs residual variance. Its
occupancy is the control signal: consistently empty means the client is too tight, consistently full
means it is running further ahead than necessary and paying latency for nothing. The server sends a
dilation hint each snapshot.

**Adapting to change.** Route changes produce step changes in RTT. The estimator uses a fast-attack,
slow-decay response: it reacts quickly to increased latency to avoid dropping inputs, and reduces the
lead conservatively so a brief improvement does not cause a tightening that immediately has to be
undone.

## Consequences

- Inputs arrive on time across a wide range of network conditions, without the player noticing the
  adjustment.
- Each client naturally runs at the lead its own connection requires; a 20 ms player is not penalised
  by a 200 ms player in the same session.
- Bounded dilation means a sudden latency spike still drops inputs briefly. This is deliberate:
  unbounded dilation would visibly speed up or slow down the game, which is worse than losing a
  frame of input.
- Clock rate adjustment interacts with rendering. The interpolation clock is driven from the same
  time source so that dilation does not introduce judder; this coupling is documented, because
  driving rendering from an independent clock reintroduces exactly the stutter the design avoids.
- In mesh topologies there is no server reference. Peers agree on a shared timeline via the confirmed
  frame ([ADR-0013](0013-rollback-model.md)) instead, and dilation targets keeping all peers within
  the rollback window of each other.

## Alternatives considered

**Fixed input delay** — every client leads by a constant. Trivial, predictable, and the classic
fighting-game answer. Rejected as the default because the constant must be sized for the worst
connection, penalising everyone else. Available as a configuration option, and the right choice when
uniform fairness matters more than individual latency.

**Server stalls waiting for inputs.** Perfectly fair — nobody's input is ever dropped. Rejected
because one bad connection degrades everyone, which is the wrong failure mode for anything above two
players.

**NTP or wall-clock synchronisation.** Uses existing infrastructure. Rejected because what matters is
the offset *including queueing and processing*, not absolute time agreement; measuring the game's own
path directly is both more accurate and dependency-free.

**Client-chosen lead, unmanaged.** Lets sophisticated clients tune themselves. Rejected: it is a
cheat vector — a client claiming a large lead gets more time to react to remote actions — and most
clients would choose badly.
