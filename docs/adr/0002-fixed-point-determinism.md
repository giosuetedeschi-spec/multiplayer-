# ADR-0002: Determinism via fixed-point math, not floats

**Status:** accepted

## Context

Rollback ([ADR-0013](0013-rollback-model.md)) works by re-simulating past ticks with corrected
inputs and expecting the result to match what the other peers computed. If two peers simulate the
same inputs from the same state and get even a one-bit different answer, they desync, and the
divergence compounds every tick until the game is visibly broken.

So rollback requires **bit-identical re-simulation across every machine in a session**. `tempo`
targets Rust, Go, Python, TypeScript, C and C++, across x86-64 and ARM, native and browser. Under
those constraints, IEEE-754 floating point does not deliver bit-identical results:

- **Transcendentals are not specified.** `sin`, `cos`, `atan2`, `pow` are accurate to within an ULP
  or two, but which way they round is a property of the platform's libm. glibc, musl, macOS's
  libm, MSVC's CRT, and every JavaScript engine differ.
- **Compilers reassociate.** `a + b + c` may be evaluated in either grouping, and fused
  multiply-add contracts `a*b + c` into a single rounding on ARM and on x86 with FMA enabled, but
  not on older targets. The results differ in the last bit.
- **JITs make it worse.** JavaScript and Python JITs may keep an intermediate in an 80-bit or
  register-width representation for one run and spill it to 64 bits on the next. The same code can
  disagree with *itself* between the interpreter and the optimised tier.
- **Languages disagree about basics.** Go forbids x87 excess precision; C historically permitted
  it. Python floats are C doubles, but `math.fsum` and literal parsing differ from JavaScript's.

Every one of these is individually fixable with enough discipline. Collectively, across six
languages and two architectures, they are not.

## Decision

`tempo` ships **`tempo-fixed`**, a deterministic fixed-point math library, and requires that
anything participating in rollback-eligible simulation use it.

- **`Fx`** — a Q32.32 signed fixed-point scalar backed by `i64`. 32 integer bits, 32 fractional
  bits; resolution `2^-32` ≈ 2.3e-10; range ±2.1e9. Wide enough for world coordinates in
  centimetres and fine enough that accumulated integration error is not observable.
- **`Vec2`, `Vec3`, `Quat`** built on `Fx`.
- **Transcendentals by table**, not by libm: `sqrt` by integer Newton–Raphson with a fixed iteration
  count; `sin`, `cos`, `atan2`, `exp`, `ln` by a committed lookup table with linear interpolation.
  Every one is pure integer arithmetic with a specified iteration count and rounding mode.
- **Overflow is saturating by default**, with a `checked_` family available. Wrapping overflow is a
  desync waiting to happen because it turns a small numerical problem into a large one.
- The algorithms are **specified to the bit** in [`spec/fixed-point.md`](../spec/fixed-point.md) and
  pinned by shared test vectors in [`conformance/`](../../conformance/). Every language binding runs
  the same vectors in CI; drift fails the build.

Determinism is verified, not assumed:

- **Per-tick state hash.** BLAKE3 over the arena's replicated range, computed every tick, exchanged
  between peers. Divergence is detected within one tick rather than discovered as a gameplay bug.
- **Sync-test mode.** A development mode that rolls back and re-simulates every single frame and
  compares hashes, surfacing nondeterminism on the developer's machine instead of in a player's
  ranked match.
- **Resync.** On confirmed divergence, the authority (or, in a mesh, the elected reference peer)
  ships a full state snapshot and the divergent peer restarts from it.

Floating point remains entirely available for rendering, audio, UI, and any non-replicated logic.
The constraint applies to simulated state, not to your whole program.

## Consequences

- **Simulation code must use our types.** `player.position += velocity * dt` uses `Fx`, not `f32`.
  This is the real cost and it is not small: physics, easing curves, and any third-party maths
  library a user wants to bring must be adapted or avoided.
- Fixed-point has uniform absolute precision, unlike float's uniform *relative* precision. Very
  large and very small magnitudes behave differently than users expect. Documented in the guide,
  with the practical rule: choose units so gameplay quantities sit in a sane range.
- Division and transcendentals are meaningfully slower than hardware float. Benchmarks in
  `benches/` quantify it; for typical entity counts the replication sweep dominates anyway.
- Six independent ports of the same maths is a real maintenance burden and a real source of
  divergence. The conformance vectors are what makes it tractable — they are not optional
  infrastructure, they are the mechanism that makes this decision work.
- Games that do not need rollback (pure authoritative-server topologies with no client-side
  re-simulation) may use floats freely for non-replicated state and only pay the fixed-point cost on
  replicated fields.

## Alternatives considered

**Floats plus desync detection and forced resync.** Simple, fast, and idiomatic; peers that diverge
just get corrected. Rejected for competitive play: divergence would be routine rather than
exceptional, so resyncs would be frequent and visible, and rollback's whole value — an
indistinguishable-from-local feel — evaporates. Kept as an explicitly-opted-into mode for casual and
co-op titles, documented as such.

**Compile the simulation to WebAssembly and host it from every language.** Technically the best
answer: wasm mandates IEEE-754 semantics with no excess precision and no reassociation, so floats
genuinely are deterministic there. Rejected because it requires gameplay logic to be written once in
a wasm-targetable language rather than natively per ecosystem, which is the opposite of this
project's premise. Revisit as an opt-in "maximum determinism" mode.

**Softfloat — a portable software IEEE-754 implementation.** Gives bit-identical float semantics
including the familiar precision curve, so user code keeps its usual shape. Rejected as strictly
worse than fixed-point for our purposes: comparable or greater cost per operation, a much larger
surface to port and verify six times, and it still leaves transcendentals to be specified by hand —
so we would do all the fixed-point work anyway, on top of a softfloat core.

**Restrict rollback to Rust-only sessions.** Would let every other language use native floats.
Rejected because it makes rollback a Rust feature with a polyglot veneer, and mixed-language
sessions are an explicit goal.
