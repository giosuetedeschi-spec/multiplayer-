# Roadmap

`tempo` is built in phases. Each phase is independently useful and independently reviewable. The
ordering is deliberate and is itself a decision — see
[ADR-0024](adr/0024-phasing-and-sequencing.md).

Legend: **done** · *in progress* · planned

## P0 — Foundation

**done** · Monorepo scaffolding, Cargo workspace, lint and format configuration, CI matrix with a
dedicated determinism gate, and this documentation tree: an ADR for every decision plus the
normative specifications the implementation is written against.

## P1 — The Rust vertical slice

*in progress* · One complete, playable path through the whole system in Rust alone, before any
binding exists. Proving the design once beats porting a wrong design six times.

- **done** `tempo-fixed` — Q32.32 deterministic math, vectors, quaternions
- **done** `tempo-wire` — bit packing, quantization, schema canonicalisation and hashing
- **done** `tempo-core` — world arena, entity generations, snapshots, snapshot ring, baseline deltas
- **done** `tempo-transport` — loopback, simulated-link, UDP
- `tempo-netcode` — connect tokens, encryption, replay protection
- `tempo-reliability` — four channels, ack bitfields, bandwidth budgets
- `tempo-predict` — client prediction, reconciliation, interpolation, clock sync
- `tempo` — the facade crate and derive macros
- a playable 2D authoritative-server demo

## P2 — Advanced simulation

planned · The features that justify the core owning state.

- `tempo-rollback` — GGPO-class rollback, input prediction, sync-test mode
- lag compensation via server-side rewind
- `tempo-interest` — GridAOI, spatial hash, priority accumulator, per-client budgets

## P3 — The boundary and the first bindings

planned · `tempo-abi` and the `cbindgen` header, then Python (PyO3) and TypeScript (napi-rs for
runtimes, wasm-bindgen for browsers), plus the cross-language conformance runners and a
Rust-server ↔ Python-bot ↔ browser-client demo.

## P4 — The remaining bindings

planned · Go over cgo, the C header as a first-class artifact, a header-only C++ RAII wrapper, and
prebuilt distribution for every ecosystem ([ADR-0019](adr/0019-build-and-distribution.md)).

## P5 — Transports and peer-to-peer

planned · QUIC/WebTransport, WebRTC DataChannel, WebSocket fallback, ICE/STUN/TURN, the relay
binary, mesh topology, and host migration.

## P6 — Platform services

planned · Matchmaker, lobbies and parties, session orchestrator, dedicated-server harness,
persistence adapters, the observability stack, and container/Kubernetes deployment.

## P7 — Integrations

planned · Bevy plugin, Godot GDExtension, Three.js/Pixi/React adapters, and example games.

## P8 — Control plane

designed, not built · The multi-tenant hosted platform. Design lives in [`ops/`](ops/); building it
is a separate undertaking from building the engine, and conflating the two is how engines don't get
finished.
