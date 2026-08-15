# ADR-0005: Five transports behind one trait

**Status:** accepted

## Context

The target list spans dedicated Linux servers, native desktop clients, browsers, and peer-to-peer
sessions behind consumer NAT. No single transport covers that:

- Browsers cannot open raw UDP sockets, so a TypeScript client rules out UDP-only.
- Peer-to-peer in a browser is only possible over WebRTC.
- Corporate and mobile networks block UDP often enough that a TCP-shaped fallback is the difference
  between "playable" and "cannot connect".
- Deterministic tests need a transport with no real network at all.

Meanwhile the layers above — reliability, replication, prediction — must not care which of these is
in use.

## Decision

A single `Transport` trait exposing unreliable datagrams with a bounded MTU, plus connection
lifecycle events. Everything above it is transport-agnostic. Five implementations ship:

| Transport | Role |
|---|---|
| **Loopback / in-memory** | Tests, single-player, local co-op. Zero network, deterministic ordering, no serialization skipped — the same bytes are produced, so it exercises the real codec. |
| **Simulated link** | Loopback plus configurable latency, jitter, loss, duplication and reordering. This is how prediction, reconciliation and rollback get tested under 200 ms RTT and 10% loss in CI, without a network. |
| **UDP + netcode tokens** | The baseline for dedicated servers. Lowest latency, encrypted and authenticated per [ADR-0015](0015-connect-tokens-and-security.md). |
| **QUIC / WebTransport** | One transport for native *and* browser. Datagrams for state, reliable streams for control. TLS is built in. Broadly available in browsers since early 2026. |
| **WebRTC DataChannel** | The only peer-to-peer path, and the only P2P path that works in a browser. Unreliable-unordered mode. Drags in ICE/STUN/TURN — see [ADR-0021](0021-p2p-nat-traversal.md). |
| **WebSocket** | Last-resort fallback for hostile networks. Head-of-line blocking is accepted explicitly; the reliability layer degrades to pass-through. |

The MTU is fixed conservatively at **1200 bytes** of payload, below the smallest path MTU we expect
to encounter, so fragmentation ([ADR-0010](0010-reliability-channels.md)) is under our control
rather than IP's.

Transport selection is configuration. A client may be given an ordered preference list and will
negotiate downward — QUIC, then WebSocket, for example — without the application observing anything
beyond a slightly longer connect.

## Consequences

- The simulated-link transport is arguably the most valuable one in the list: it makes bad-network
  behaviour reproducible and CI-testable, which is normally the hardest thing to test in this domain.
- Six transports is six sets of platform quirks, TLS configuration, and connection-teardown edge
  cases. This is a large ongoing maintenance surface.
- The lowest-common-denominator interface (unreliable datagrams, 1200-byte MTU) means QUIC's
  reliable streams and congestion control are partly redundant with our own reliability layer. We
  accept the duplication for uniformity; the alternative is transport-specific code paths above the
  trait, which is worse.
- WebSocket's head-of-line blocking cannot be hidden. It is documented as degraded, not equivalent.

## Alternatives considered

**QUIC/WebTransport only.** Genuinely tempting: one transport, native and browser, encryption
included. Rejected because it cannot do peer-to-peer at all, and adds measurable handshake and
per-packet overhead versus raw UDP for dedicated servers where we already have our own encryption.

**UDP only, with a separate browser product.** Simplest core. Rejected — TypeScript is a first-class
target, and a browser client that cannot connect is not a client.

**Adopt an existing transport library wholesale** (`renet`, `laminar`, `ENet`). Reasonable and would
save real work. Rejected because the reliability layer must be co-designed with the replication
layer to share ack state: delta compression needs to know exactly which snapshot each client has
acknowledged, and bolting that onto a transport that owns its own acks means either duplicating ack
tracking or reaching through an abstraction that was not built for it.
