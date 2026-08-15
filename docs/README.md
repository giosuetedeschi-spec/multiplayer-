# tempo documentation

Everything about `tempo` that is not code lives here. The specifications are normative: when the
implementation and a spec in [`spec/`](spec/) disagree, the spec is right and the code is a bug.

## Reading order

If you are new, read in this order:

1. **[ROADMAP.md](ROADMAP.md)** — what exists today versus what is designed but unbuilt.
2. **[ADR-0001: the core owns replicated state](adr/0001-core-owns-replicated-state.md)** — the one
   decision every other decision hangs off. Read this even if you read nothing else.
3. **[design/replication-pipeline.md](design/replication-pipeline.md)** — how a component write
   becomes bytes on the wire and state on a peer.
4. **[guides/quickstart-rust.md](guides/quickstart-rust.md)** (or your language) — get something
   running.
5. **[design/trust-model.md](design/trust-model.md)** — before you ship anything competitive.

## Layout

### [`adr/`](adr/) — architectural decision records

One file per decision, immutable once accepted. Each records what we chose, what we rejected, and
what it costs us. If you are about to ask "why on earth does it work like that?", the answer is
here. See [ADR-0000](adr/0000-adr-process.md) for the format.

| ADR | Decision |
|---|---|
| [0001](adr/0001-core-owns-replicated-state.md) | The Rust core owns replicated state |
| [0002](adr/0002-fixed-point-determinism.md) | Determinism via fixed-point math, not floats |
| [0003](adr/0003-native-first-schema-derivation.md) | Schemas declared natively, canonicalised at runtime |
| [0004](adr/0004-language-parity-and-perf-ceilings.md) | Full language parity with published performance ceilings |
| [0005](adr/0005-transport-matrix.md) | Five transports behind one trait |
| [0006](adr/0006-topology-agnostic-api.md) | Topology is configuration, not a rewrite |
| [0007](adr/0007-ephemeral-core-pluggable-durability.md) | Ephemeral core, pluggable durability |
| [0008](adr/0008-c-abi-single-boundary.md) | One narrow C ABI as the only boundary |
| [0009](adr/0009-custom-bitpacked-wire-format.md) | A custom bit-packed wire format |
| [0010](adr/0010-reliability-channels.md) | Four reliability channels over one datagram layer |
| [0011](adr/0011-interest-management.md) | Interest management and priority accumulation |
| [0012](adr/0012-prediction-and-reconciliation.md) | Client prediction with server reconciliation |
| [0013](adr/0013-rollback-model.md) | GGPO-class rollback over core-owned state |
| [0014](adr/0014-lag-compensation.md) | Server-side rewind for hit resolution |
| [0015](adr/0015-connect-tokens-and-security.md) | netcode.io-compatible connect tokens |
| [0016](adr/0016-clock-sync-and-time-dilation.md) | Clients run ahead; the server dilates their clocks |
| [0017](adr/0017-versioning-and-compatibility.md) | Protocol and schema version negotiation |
| [0018](adr/0018-licensing-deferred.md) | Licensing deliberately deferred |
| [0019](adr/0019-build-and-distribution.md) | Prebuilt binaries in every package ecosystem |
| [0020](adr/0020-observability.md) | Metrics, tracing, capture and deterministic replay |
| [0021](adr/0021-p2p-nat-traversal.md) | ICE/STUN/TURN with relay fallback |
| [0022](adr/0022-host-migration.md) | Deterministic successor election |
| [0023](adr/0023-eventual-sync-mode.md) | A CRDT mode for non-game realtime sync |
| [0024](adr/0024-phasing-and-sequencing.md) | Spec first, Rust slice second, bindings third |
| [0025](adr/0025-naming.md) | The name `tempo` |

### [`spec/`](spec/) — normative specifications

- [wire-protocol.md](spec/wire-protocol.md) — packet layout to the bit
- [fixed-point.md](spec/fixed-point.md) — the exact semantics every language must reproduce
- [schema-and-hashing.md](spec/schema-and-hashing.md) — canonical schema form and its content hash
- [abi.md](spec/abi.md) — the C ABI contract
- [conformance.md](spec/conformance.md) — the vectors every implementation must pass

### [`design/`](design/) — how it actually works

- [replication-pipeline.md](design/replication-pipeline.md)
- [rollback.md](design/rollback.md)
- [interest-management.md](design/interest-management.md)
- [trust-model.md](design/trust-model.md)

### [`guides/`](guides/) — task-oriented

Per-language quickstarts, choosing a topology, bandwidth tuning, debugging desync.

### [`ops/`](ops/) — running it

Deployment, orchestration, observability, and the hosted control plane design.
