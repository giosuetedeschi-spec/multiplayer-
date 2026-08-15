# ADR-0015: netcode.io-compatible connect tokens

**Status:** accepted

## Context

A UDP game server exposed to the internet faces problems a TCP service does not:

- **No handshake by default.** Anyone can send a packet claiming to be anyone.
- **Source address spoofing.** UDP source addresses are trivially forged.
- **Amplification.** If a small request produces a large response, the server becomes a DDoS
  reflector pointed at a spoofed victim.
- **Replay.** A captured packet can be resent.
- **Unauthenticated connections.** Without identity, matchmaking and bans are meaningless.

Meanwhile the game server should not be doing authentication. It should not hold user credentials,
should not query an auth database on the connect path, and should not become unavailable when the
auth service is.

Glenn Fiedler's **netcode.io** protocol solves exactly this and is well analysed. Reinventing it
would be a poor use of effort and a good way to introduce a subtle cryptographic flaw.

## Decision

Implement **netcode.io 1.02-compatible connect tokens** in `tempo-netcode`.

**The flow.** The client authenticates with the game's own backend, by whatever means the game
already uses. The backend — which holds a private key shared with the game servers — issues a
**connect token**:

- a list of server addresses the token is valid for
- a client ID and arbitrary user data
- an expiry timestamp
- ephemeral client-to-server and server-to-client keys
- all of it encrypted and authenticated with **XChaCha20-Poly1305** under the shared private key

The client presents the token to a game server, which decrypts and validates it **offline**. The
game server never contacts the backend on the connect path, never sees credentials, and cannot be
taken down by the auth service being down.

**Protections that follow.**

- **Address spoofing** — a challenge/response round trip before any connection state is allocated.
- **Amplification** — connection request packets are padded to 1200 bytes so a request is never
  smaller than its response; the response ratio is below one.
- **Replay** — a sliding window of recently seen sequence numbers per connection; a repeat is
  dropped. Tokens are single-use, tracked by their nonce until expiry.
- **Per-packet encryption and authentication** — every packet after connection is AEAD-sealed, so
  packets cannot be forged, modified, or read in transit.
- **Rate limiting** on unconnected packets, per source address, with a hard cap on half-open
  connections.
- **Short token expiry** (default 30 s) to bound the value of a stolen token.

**Key management.** The private key is shared between the backend and game servers via configuration
or a secrets manager, and is rotatable with an overlap window so tokens issued under the previous key
remain valid until expiry. Documented in [`ops/security.md`](../ops/security.md).

**What this is not.** Connect tokens authenticate *who is connecting*. They say nothing about whether
the connected client is behaving honestly. Anti-cheat is the authoritative simulation's job — the
server accepts inputs, never state — and in mesh topologies there is no such protection at all
([ADR-0006](0006-topology-agnostic-api.md)).

## Consequences

- Game servers are stateless with respect to authentication and survive backend outages.
- The protocol is compatible with the wider netcode.io ecosystem, so existing analysis and tooling
  apply.
- Games must run a backend that issues tokens. A reference implementation ships in `tempo-server` so
  this is not a blocker for getting started.
- Clock skew between backend and game servers affects expiry validation. The tolerance is
  configurable and the operations guide requires NTP.
- The shared private key is a serious secret: possession allows minting tokens for any player. Key
  handling gets its own section in the security guide.
- Encryption costs per packet. XChaCha20-Poly1305 is fast enough that this is not measurable against
  the replication sweep, and it is confirmed in benchmarks rather than assumed.

## Alternatives considered

**DTLS.** Standard, well analysed, mature implementations. Rejected because it solves transport
security but not identity or matchmaking integration, so tokens would be needed anyway — and its
handshake costs more round trips than netcode.io's, which matters on the connect path.

**QUIC's built-in TLS.** Genuinely excellent, and used when the QUIC transport is selected. Rejected
as *the* answer because it only covers one of five transports; UDP, WebRTC and loopback would still
need this, so we would maintain two security models.

**Roll our own token format.** Tempting for a bespoke feature set. Rejected on principle: novel
cryptographic protocol design is how security bugs happen, and netcode.io already matches our
requirements closely.

**Application-level authentication over the game connection** — connect first, authenticate after.
Rejected because it allocates connection state before establishing identity, which is precisely the
resource-exhaustion vector the challenge/response is designed to close.
