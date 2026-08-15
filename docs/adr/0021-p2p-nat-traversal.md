# ADR-0021: ICE/STUN/TURN with relay fallback

**Status:** accepted

## Context

Peer-to-peer topologies ([ADR-0006](0006-topology-agnostic-api.md)) require two consumer machines,
each behind a NAT router, to exchange packets directly. Neither has a publicly routable address, and
neither can accept an unsolicited inbound packet.

NAT traversal is a solved problem in the sense that the techniques are well established, and an
unsolved problem in the sense that a meaningful fraction of connections cannot be made to work at
all. Symmetric NATs, carrier-grade NAT on mobile networks, and restrictive corporate firewalls
defeat hole punching. Real-world direct-connection success rates sit around 80–90% on residential
networks and considerably worse on mobile.

So any honest P2P design needs two things: the best traversal available, and a fallback for when it
fails. A framework offering only hole punching offers a feature that silently fails for one player in
ten.

## Decision

Standard **ICE** (Interactive Connectivity Establishment), with **STUN** for discovery, **hole
punching** for direct connection, and a **TURN-style relay** for the cases that cannot be punched.

**Signalling.** Peers cannot exchange candidates without a rendezvous point. `tempo-relay` includes a
signalling service over WebSocket; the exchange is small, infrequent, and not latency-critical, so
WebSocket is the right tool. It carries only candidates and session metadata, never game traffic.

**Candidate gathering.** Each peer collects host candidates (local interfaces), server-reflexive
candidates (its public mapping, learned from STUN), and relay candidates (allocated on the TURN
server). Candidates are exchanged through signalling and paired.

**Connectivity checks.** Candidate pairs are probed in priority order — host, then reflexive, then
relay — and the best working pair wins. Checks run concurrently so a working path is found in
parallel rather than by sequential timeout, which is the difference between a one-second and a
ten-second connect.

**Relay fallback.** When no direct path works, traffic flows through `tempo-relay`. This costs
bandwidth and adds a hop of latency, and is unambiguously better than failing to connect. The
application is told which path was selected, so it can surface it.

**Continuous reassessment.** Networks change; a player moves from Wi-Fi to cellular. ICE restarts
renegotiate without dropping the session.

**Operational honesty.** Running P2P at scale means running STUN and TURN servers. TURN in particular
relays real game traffic and therefore costs real bandwidth. `tempo-relay` implements both, deploys
as a single binary, and its capacity planning — including the roughly 10–20% of sessions expected to
need relaying — is documented in [`ops/relay.md`](../ops/relay.md) rather than discovered from a
bandwidth bill.

## Consequences

- P2P works for the large majority of players directly, and for essentially all players via relay.
- Running P2P requires infrastructure. "Serverless P2P" is a widespread misconception this design
  refuses to encourage: signalling is always needed, and relay is needed often enough to budget for.
- ICE adds connection latency — typically a few hundred milliseconds of candidate gathering and
  checking before gameplay starts.
- Relayed connections have materially different latency from direct ones. The application is told,
  because a player on a relayed path in a competitive match should be able to know.
- We implement or depend on a substantial amount of the WebRTC stack. Where possible this reuses
  existing Rust crates rather than reimplementing ICE, which is a large and subtle specification.

## Alternatives considered

**Hole punching only, no relay.** Simpler and infrastructure-free once signalling exists. Rejected:
it fails for 10–20% of players with no recourse, which is not an acceptable failure rate for a
shipping game.

**Relay everything; skip direct connections.** Trivially reliable and simple to operate. Rejected on
cost and latency — it forfeits P2P's entire advantage while retaining its trust weaknesses, at which
point a dedicated server is simply better.

**A full WebRTC stack dependency.** Would provide ICE, DTLS and SCTP complete and battle-tested.
Partially adopted: the WebRTC transport uses it, since browsers require genuine WebRTC. Rejected for
native-to-native P2P, where DTLS and SCTP duplicate our own security
([ADR-0015](0015-connect-tokens-and-security.md)) and reliability
([ADR-0010](0010-reliability-channels.md)) layers.

**UPnP / NAT-PMP port mapping.** Asks the router to open a port directly, giving a clean path when it
works. Rejected as a primary mechanism — widely disabled by default and inconsistently implemented —
but included opportunistically as an additional candidate source, because when it does work it works
very well.
