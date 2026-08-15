# ADR-0004: Full language parity, with published performance ceilings

**Status:** accepted

## Context

`tempo` targets six languages. The obvious question is whether all six are equals — able to host an
authoritative server as well as a client — or whether some are client-only.

Normally this would be an expensive question, because "write a game server in Python" usually means
Python is doing the per-entity work and the GIL is in the hot path. Under
[ADR-0001](0001-core-owns-replicated-state.md) that is not what happens: the Rust core holds the
arena, runs the replication sweep, computes deltas, does interest management and drives the
transport. The host language runs gameplay logic once per tick over batched views. The expensive
work is already in Rust regardless of who called it.

So parity is unusually cheap here. But it is not free, and pretending otherwise would be dishonest:
Python's per-tick gameplay logic still runs under the GIL, TypeScript still garbage-collects, and a
user who reads "full parity" as "Python scales like Rust" will discover the difference at the worst
possible moment.

## Decision

**All six languages can host an authoritative server and a client.** Browser TypeScript is
client-only, which is a property of browsers rather than a decision.

Alongside that, we **measure and publish per-language performance ceilings**. A soak harness in
`benches/` drives synthetic clients against a server in each language and reports, for a fixed
reference workload:

- sustainable concurrent connections at a given tick rate
- the tick-time budget consumed by the binding layer versus the core
- p50/p99 tick time under load
- allocation and GC pressure where the runtime has a collector

Results land in `docs/guides/performance-ceilings.md` with the workload defined precisely enough to
reproduce, and they are regenerated on release rather than written once and left to rot.

The guide states the practical guidance plainly: Rust and Go for large authoritative deployments,
Python and TypeScript excellent for game logic at moderate scale, for tooling, for bots, for
prototyping and for the enormous number of games that never exceed a few hundred concurrent
players.

## Consequences

- The polyglot claim is real rather than aspirational, and a Python team can ship a production
  server for a game of appropriate scale.
- We are on the hook for benchmarking infrastructure across six languages, and for keeping the
  numbers current. Stale benchmarks are worse than none.
- Published ceilings will sometimes be unflattering. That is the intent — the alternative is a user
  discovering the ceiling in production.
- Every feature must be exposed in every binding, so no feature is "done" until it is done six
  times. This is a permanent tax on scope, and it is the reason the C ABI surface is deliberately
  narrow ([ADR-0008](0008-c-abi-single-boundary.md)).

## Alternatives considered

**Tiered: Rust and Go for production, Python and TypeScript for development.** Smaller surface,
much less benchmarking, and a clean story. Rejected because it contradicts the premise — "plug and
play in every one of them" cannot mean "except when it matters".

**Rust is the only real server; everything else is a client.** Simplest to make fast and correct.
Rejected as the weakest possible polyglot claim; it would make the other five bindings second-class
by construction.

**Full parity with no published caveats.** Cleanest marketing. Rejected as dishonest engineering:
the ceilings exist whether or not we document them, and the only question is whether the user finds
out from us or from an outage.
