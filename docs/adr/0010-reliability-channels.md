# ADR-0010: Four reliability channels over one datagram layer

**Status:** accepted

## Context

Different game traffic wants opposite delivery guarantees, and forcing one guarantee on all of it is
the classic mistake that makes games feel bad on lossy networks.

- **Position snapshots** must never wait. A lost snapshot is worthless by the time it could be
  retransmitted, because a newer one has already arrived. Retransmitting it actively harms the game
  by delaying fresher data behind it.
- **"Player fired a rocket"** must arrive, and must arrive after "player picked up rocket launcher".
- **Chat messages** must arrive, but their order relative to other chat is irrelevant and blocking
  on a lost one is pointless.
- **Voice or emotes** want the newest only; an older packet arriving late should be dropped.

Running everything over TCP gives head-of-line blocking on all of it. Running everything unreliably
means reimplementing acknowledgement per message type in user code.

## Decision

Four named channels above a single unreliable datagram transport
([ADR-0005](0005-transport-matrix.md)):

| Channel | Guarantee | Used for |
|---|---|---|
| `Unreliable` | Best effort, may arrive out of order or not at all | Rarely used directly; the escape hatch |
| `UnreliableSequenced` | Best effort, stale packets dropped on arrival | State snapshots, inputs, voice |
| `ReliableOrdered` | Delivered exactly once, in order, blocking on gaps | Spawns, despawns, RPCs, state machine transitions |
| `ReliableUnordered` | Delivered exactly once, any order | Chat, telemetry, independent events |

Shared machinery underneath:

- **Sequence numbers with a 32-bit ack bitfield.** Each packet header carries its own sequence, the
  latest received sequence, and a bitfield of the 32 before it. One ack packet acknowledges up to 33
  packets, and the redundancy means acks survive loss without needing their own reliability.
- **Ack state is shared with replication.** This is the reason we do not adopt an external
  reliability library: delta compression must know precisely which snapshot each client has
  acknowledged in order to pick a baseline. Reliability and replication read the same ack state
  rather than maintaining two views of it.
- **RTT estimation** by exponentially weighted moving average with variance, feeding retransmission
  timeouts and the clock synchronisation in [ADR-0016](0016-clock-sync-and-time-dilation.md).
- **Fragmentation and reassembly** for payloads over the 1200-byte MTU, with a bounded reassembly
  buffer and a timeout, because unbounded reassembly buffers are a denial-of-service vector.
- **Per-client bandwidth budget.** A configured send rate in bytes per second is the hard constraint
  the priority accumulator ([ADR-0011](0011-interest-management.md)) allocates against. Congestion
  signals (loss rate, RTT inflation) reduce the budget adaptively.

## Consequences

- Users pick a channel per message type and get correct behaviour without writing acknowledgement
  logic.
- Shared ack state makes delta baseline selection exact rather than heuristic, which meaningfully
  improves compression versus systems that guess.
- `ReliableOrdered` can still head-of-line block *within its own channel* under loss. That is
  inherent to the guarantee; the mitigation is not putting position data on it, which the channel
  names are designed to discourage.
- We are implementing congestion control, which is genuinely hard to do well. The initial
  implementation is deliberately conservative — a bandwidth budget with additive-increase,
  multiplicative-decrease on loss — rather than clever. Being a poor network citizen is worse than
  being slightly slow.
- Over QUIC and WebSocket some of this duplicates transport-level machinery. Accepted for uniformity
  (ADR-0005).

## Alternatives considered

**TCP for everything.** Trivial, and works. Rejected: head-of-line blocking on state snapshots is
the single worst thing you can do to a realtime game's feel under packet loss.

**QUIC streams as the reliability layer.** QUIC already provides multiplexed reliable streams and
mature congestion control, and using them would remove a large amount of hand-written code.
Rejected because it only works on the QUIC transport — UDP, WebRTC and loopback would need a
parallel implementation, so we would maintain two reliability layers instead of one — and because it
does not expose the ack visibility that delta compression needs.

**Adopt `laminar`, `ENet`, or `renet`'s reliability layer.** Mature and battle-tested. Rejected on
the ack-sharing requirement above, and because their packet headers are fixed while ours must carry
tick and baseline identifiers that the replication layer defines.

**More channels, user-defined.** Some frameworks allow arbitrary numbered channels with per-channel
configuration. Rejected as unnecessary flexibility: four guarantees cover the space, and named
channels teach the right mental model in a way that `channel(7)` does not.
