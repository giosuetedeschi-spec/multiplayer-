# ADR-0020: Metrics, tracing, capture and deterministic replay

**Status:** accepted

## Context

Multiplayer bugs are the hardest category of bug to diagnose, for reasons that compound:

- They depend on **timing** — a race that appears at 120 ms RTT and never at 20 ms.
- They depend on **loss patterns** that are not reproducible by rerunning.
- They involve **multiple processes** whose logs must be correlated to make sense.
- The interesting state lives in a **bit-packed binary format** that a hex dump cannot explain
  ([ADR-0009](0009-custom-bitpacked-wire-format.md)).
- They frequently only appear at **scale**, with hundreds of real clients on real networks.

A framework that hides all of this behind an opaque `send()` leaves users with printf debugging
against a binary protocol. Since the core owns state and transport, it is the only component
positioned to observe the whole picture — so if it does not, nobody can.

## Decision

Observability is a core feature, not an add-on.

**Metrics.** Exported in Prometheus format, and available programmatically in every binding:

- tick duration histograms (p50/p95/p99), separated into simulation, replication, and transport
- bandwidth in and out, per client and aggregate, broken down by channel
- packet loss, RTT and jitter per client
- entities replicated per snapshot, and the fraction of the bandwidth budget used
- rollback frequency and depth; reconciliation frequency and correction magnitude
- interest-management set sizes and priority-accumulator starvation age
- connection lifecycle counts, including handshake failures by reason

**Tracing.** OpenTelemetry spans across the tick pipeline, with trace context propagated in packet
headers under a debug flag so a single input can be followed from client keypress through server
processing to the resulting snapshot on another client. This cross-process correlation is the thing
that makes distributed timing bugs tractable.

**Packet capture and deterministic replay.** The highest-value tool in the list. Sessions can record
every packet with timestamps and the schema. `tempo replay` then re-runs the session:

- decoding packets into human-readable state using the recorded schema
- reproducing the exact timing, loss and reordering of the original
- stepping tick by tick, inspecting arena state at any point

Because the simulation is deterministic ([ADR-0002](0002-fixed-point-determinism.md)), replay
reproduces the original run *exactly*. A bug that happened once on a player's machine can be
reproduced on a developer's machine as many times as needed. This is determinism paying a third
dividend, after rollback and persistence.

**`tempo inspect`.** A terminal UI for live sessions: entities, per-client bandwidth allocation,
which entities are being starved by the priority accumulator, and rollback activity.

**Structured logging** via `tracing`, with per-subsystem levels, bridged into each binding's native
logging so a Python user sees Python log records.

**Overhead.** Metrics are always on and cheap — atomic counters and preallocated histograms. Tracing
and capture are opt-in, because capture writes every packet to disk. The always-on path is benchmarked
so that "observability is free by default" is a measured claim.

## Consequences

- Users can diagnose their own network problems instead of filing an unreproducible bug report.
- Deterministic replay turns the worst class of bug — happened once, in production, on someone
  else's machine — into an ordinary reproducible one.
- Capture files contain complete game state and are therefore sensitive: they can reveal player
  positions and inputs. Handling requirements are documented in
  [`ops/observability.md`](../ops/observability.md), and capture is off by default.
- Metric surface must be exposed through the ABI and every binding, adding to the parity tax
  ([ADR-0004](0004-language-parity-and-perf-ceilings.md)).
- Replay depends on determinism holding. Where user code is nondeterministic, replay diverges — and
  usefully so, since that divergence is itself the diagnosis.

## Alternatives considered

**Logging only.** Minimal effort. Rejected: logs cannot express bandwidth allocation decisions or
per-client priority starvation, and correlating them across processes by hand does not scale.

**Metrics only, no capture or replay.** Covers monitoring and alerting well, which is most of
production observability. Rejected because it does not help with the debugging case, which is where
users need the most help and where the framework's position gives it a unique advantage.

**External tooling — let users bring Wireshark.** Rejected because the wire format is custom and
bit-packed; without the schema, external tools show noise. A Wireshark dissector generated from the
schema is attractive future work.

**Capture always on.** Maximum debuggability, and would mean every production bug is reproducible.
Rejected on disk cost and privacy: recording every packet of every session is a large data-retention
and player-privacy commitment that should be an explicit choice.
