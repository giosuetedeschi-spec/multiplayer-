# ADR-0001: The Rust core owns replicated state

**Status:** accepted

## Context

`tempo` promises rollback, delta compression, interest management and lag compensation, in six
languages, from a single import. Those four features share a requirement that is easy to miss:

- **Delta compression** must diff this tick's state against a per-client acknowledged baseline. It
  needs to read every replicated field, and it needs the field's type and quantization to encode it
  in a handful of bits instead of a handful of bytes.
- **Interest management** must decide, per client, which entities are worth sending. It needs
  positions and priorities for entities the client cannot currently see.
- **Lag compensation** must reconstruct the world as it was N ticks ago, cheaply enough to do it
  per hit query.
- **Rollback** must save and restore complete simulation state tens of times per second, and the
  saved states must be byte-comparable to detect desync.

All four are *whole-state* operations. A library that only moves bytes cannot perform any of them,
because it never sees the state — it sees whatever the user chose to hand it, after the user has
already done the hard part.

This is why "multiplayer library" almost always means "transport library plus a tutorial", and why
every team reimplements the same four subsystems. Doing that once per language, across six
languages, would mean six subtly different implementations of rollback, each with its own desync
bugs.

The counter-pressure is real: developers want plain Python objects, Go structs, and TypeScript
classes. Any design where game state lives behind an FFI boundary risks being both unidiomatic and
slow — slow especially, if reading a position costs a foreign function call.

## Decision

The Rust core owns replicated state in a **columnar (structure-of-arrays) world arena**.

- State a user marks as replicated (`#[derive(Replicate)]`, `@replicated`, struct tags, decorators
  — see [ADR-0003](0003-native-first-schema-derivation.md)) is stored in the arena, laid out by
  component and field, not by entity.
- State a user does not mark stays entirely in their language as ordinary native objects. `tempo`
  neither sees it nor cares about it.
- Host languages **read** through zero-copy views: a typed accessor over a slice of the arena, with
  no copy and no call into Rust per field.
- Host languages **write** into a per-tick staged command buffer, flushed once at the tick
  boundary. Writes are batched by construction.

The critical performance rule, which the ABI is designed to enforce: **the boundary is crossed a
constant number of times per tick, not a number of times proportional to entity count.**

## Consequences

**What this buys.**

- Rollback save/restore is a `memcpy` of the arena's dirty pages. It is the same code, at the same
  speed, whether the caller is Rust or Python.
- Delta compression, quantization, interest management and lag compensation are implemented exactly
  once, in Rust, and work identically in every language with no user code.
- Per-tick state hashing for desync detection is a hash over a contiguous byte range.
- Columnar layout makes the replication sweep cache-friendly, which is what makes 10k-entity
  interest management viable at all.

**What this costs.**

- **Replicated data is not a plain native object.** A `Player`'s position is a view into Rust
  memory, not a Python float. It reads and writes like one, but it is not one, and code that tries
  to `pickle` it or hold a reference across a tick boundary will be disappointed.
- Users must declare their replicated schema up front. There is no "just send this dictionary".
- The arena constrains field types to those the wire format can encode
  ([ADR-0009](0009-custom-bitpacked-wire-format.md)). Arbitrary user types cannot be replicated
  without a conversion.
- Bugs in the arena are memory-safety-adjacent and are exposed to six languages at once. This
  raises the testing bar considerably; see [`spec/conformance.md`](../spec/conformance.md).

**Boundary discipline.** The zero-copy read views hand out pointers into Rust-owned memory. Their
validity is scoped to a tick. Every binding must make holding one past `end_tick` either impossible
(Rust lifetimes, C++ RAII) or loudly diagnosed (Python, TypeScript, Go generation counters). This
is the single largest source of foreseeable misuse and gets explicit treatment in
[`spec/abi.md`](../spec/abi.md).

## Alternatives considered

**Transport-only core; the user serializes their own state.** By far the most idiomatic option and
the easiest to adopt — this is roughly what `renet`, `laminar` and most of the field do. Rejected
because it makes the entire feature list impossible to deliver generically. Rollback, delta-vs-
baseline, AOI and lag compensation would each become the user's problem, in each of six languages.
We would be shipping a transport and calling it an engine.

**Core owns state *and* drives the tick loop, user code as registered callbacks.** Marginally more
powerful — the core could parallelise systems and control scheduling. Rejected because it inverts
control: Python and TypeScript applications become plugins to a Rust engine rather than programs
that use a library. It also puts a foreign call in the innermost loop, which is precisely the cost
this design exists to avoid. The user drives; `tempo` is called.

**Sim compiled to WebAssembly, hosted by every language.** Genuinely the strongest determinism
story available — WebAssembly specifies IEEE-754 semantics, so even floats would be safe. Rejected
because it means gameplay logic is written *once*, in a wasm-targetable language, rather than
natively in each of six. That directly contradicts the goal of a native import in every ecosystem.
Retained as a possible future opt-in for teams who want maximum determinism; see
[ADR-0002](0002-fixed-point-determinism.md) for how we get determinism without it.

**Shared-memory IPC to a sidecar process.** Avoids FFI entirely and would make the Go and Python
stories much simpler. Rejected on latency and deployment: a context switch per tick is affordable,
but a second process to supervise, version, and ship in every package manager is not — and it makes
the browser target impossible.
