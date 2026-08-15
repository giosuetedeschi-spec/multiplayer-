# ADR-0008: One narrow C ABI as the only boundary

**Status:** accepted

## Context

Six languages need access to the same Rust core. Each has its own preferred interop mechanism —
PyO3 for Python, napi-rs for Node, cgo for Go, wasm-bindgen for browsers, a plain header for C and
C++ — and it would be possible to write each binding directly against Rust types using its native
tooling.

That path produces the nicest individual bindings and the worst overall system. Six independent
translations of the same semantics means six places for a lifetime rule to be misunderstood, six
places for a schema to be canonicalised slightly differently, and no single artifact to specify or
conformance-test.

The performance constraint from [ADR-0001](0001-core-owns-replicated-state.md) shapes this
decisively: the boundary must be crossed a **constant** number of times per tick, not once per
entity or once per field. A boundary designed around individual getters and setters cannot meet that
no matter how good the binding generator is.

## Decision

`tempo-abi` defines **one narrow C ABI**, specified in [`spec/abi.md`](../spec/abi.md). Every
binding is written against it, including the ones whose language could have talked to Rust directly.

Design rules:

1. **Opaque handles.** Sessions, worlds and views are integer handles, not structs with public
   layout. Adding a field to an internal struct is never an ABI break.
2. **Batched calls only on the hot path.** State is written by filling a command buffer and flushing
   it once at the tick boundary. There is no `tempo_set_field` — such a function would make the
   slowest possible usage the most obvious one.
3. **Zero-copy reads.** Views hand back a base pointer, stride and length. Iterating entities is
   pointer arithmetic in the host language, not calls into Rust.
4. **No callbacks on the hot path.** Callbacks exist for lifecycle events only (connect, disconnect,
   error). Rust never calls into Python during a replication sweep — that would mean acquiring the
   GIL from inside the core.
5. **Integer error codes plus a thread-local last-error string.** No exceptions, no panics crossing
   the boundary. Every `extern "C"` entry point catches unwinding at the edge.
6. **Explicit generation counters on views.** Every view carries the tick generation it was created
   in; using one after its tick is a diagnosable error rather than a use-after-free. This is the
   mitigation for the largest foreseeable misuse identified in ADR-0001.

`cbindgen` generates `tempo.h` from the Rust source, and the header is committed and diffed in CI so
that an unintended ABI change is visible in review rather than discovered by a downstream binding.

The Rust facade crate `tempo` does **not** go through the ABI — it uses the core directly, with real
lifetimes and zero overhead. It serves as the reference for what the ABI is trying to express.

## Consequences

- One place to specify, one place to conformance-test, one place to fix a semantic bug for all six
  languages at once.
- Bindings become thin and mostly mechanical, which is what makes maintaining six of them realistic.
- The ABI is a lowest common denominator: it cannot express Rust generics, lifetimes, or sum types.
  Ergonomics are recovered in each binding's idiomatic wrapper layer, which is where per-language
  taste belongs.
- Handle indirection and command buffering add overhead that native Rust does not pay. It is
  constant per tick rather than proportional to entity count, which is the property that matters.
- Once published, the ABI is a compatibility commitment. It is versioned explicitly and changes are
  additive.

## Alternatives considered

**Direct native bindings per language, no shared ABI.** Best individual ergonomics and idiomatic
error handling throughout. Rejected: six independent translations of subtle semantics — view
lifetimes, schema canonicalisation, command ordering — is six times the surface for divergence, with
nothing to test against.

**WebAssembly Component Model with WIT.** The modern, standardised answer, with generated bindings
for most target languages and real interface types instead of C's flat vocabulary. Genuinely
appealing and likely correct eventually. Rejected for now on maturity and on performance: the
component model's copying semantics at the boundary conflict directly with the zero-copy arena
views that the whole design rests on.

**UniFFI.** Excellent for the languages it covers, and would have made Python and Kotlin nearly
free. Rejected because its record-and-object model is a poor fit for zero-copy array views, and
because Go, C and C++ coverage is third-party or absent.

**A sidecar process with shared-memory IPC.** Eliminates FFI entirely and would simplify Go and
Python enormously. Rejected on deployment cost — a second process to ship, supervise and version in
six package ecosystems — and because it makes the browser target impossible.
