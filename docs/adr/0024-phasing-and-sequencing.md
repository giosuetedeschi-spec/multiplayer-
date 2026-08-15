# ADR-0024: Spec first, Rust slice second, bindings third

**Status:** accepted

## Context

The scope recorded across ADRs 0001–0023 is large: a deterministic maths library, a bit-packed
codec, a columnar state store, five transports, a reliability layer, prediction, rollback, interest
management, lag compensation, NAT traversal, persistence, observability, six language bindings, and
a set of platform services.

Build order is not a project-management detail here. It determines whether the design errors that
certainly exist are found once, cheaply, or six times, expensively. A mistake in the wire format
discovered after six bindings exist costs six corrections plus six sets of tests plus a protocol
version bump.

There is a countervailing pull. Building the browser client early would prove the hardest
integration and produce something clickable, which is motivating and generates useful feedback. That
is a real argument, not a naive one.

## Decision

**Specifications and decision records first. Then one complete Rust vertical slice. Then bindings.**

**Phase 0 — specifications.** Every ADR, plus the normative specs: wire protocol, fixed-point
semantics, schema canonicalisation, C ABI, conformance format. Two reasons this comes first, beyond
the stated requirement to document every decision:

1. Writing a spec surfaces contradictions that prose hides. The interaction between
   non-self-describing encoding and schema negotiation
   ([ADR-0009](0009-custom-bitpacked-wire-format.md) ↔ [ADR-0003](0003-native-first-schema-derivation.md))
   is a dependency that is obvious in a spec and easy to miss in code.
2. Six implementations of bit-exact behaviour need a normative reference. Without one, "correct"
   means "matches Rust", which makes Rust's incidental bugs into protocol requirements.

**Phase 1 — one Rust vertical slice.** A complete path through the system — state store, tick,
delta, prediction, transport — playable, before any binding exists. Depth over breadth: an
end-to-end slice exercises the interactions between subsystems, which is where design errors
actually live. Six shallow bindings would exercise none of them.

**Phase 2 — the features that justify the architecture.** Rollback, lag compensation, interest
management. These are the reason the core owns state ([ADR-0001](0001-core-owns-replicated-state.md)),
so they validate that decision before it is cast into an ABI.

**Phase 3 onward — the ABI, then bindings, then transports, then services, then integrations.** The
ABI is designed once the core's shape is known from use rather than from speculation.

**Phase 8 — the hosted control plane is designed but not built.** A multi-tenant hosted platform is a
company, not a repository. Conflating the two is a reliable way to finish neither. The design is
recorded in [`ops/`](../ops/) so the engine does not foreclose it.

## Consequences

- Design errors are found once, in Rust, where they cost one fix.
- Nothing is importable from another language for a significant stretch, which delays external
  validation and feels slow. Accepted deliberately.
- The specs are written before the implementation and will contain errors. They are corrected as
  implementation reveals them — the spec stays normative, but "normative" means "the agreed
  reference", not "known to be perfect".
- Each phase is independently reviewable, so course correction is possible at every boundary rather
  than only at the end.
- Later phases may reveal that an early spec decision was wrong. The ADR process
  ([ADR-0000](0000-adr-process.md)) handles this: supersede, do not silently amend.

## Alternatives considered

**Rust server plus browser client first.** Proves the hardest integration — wasm, WebTransport, the
ABI — earliest, and produces a demo on day one. Rejected because it means designing the core and the
FFI boundary simultaneously, so an error in either is discovered while both are in flux, which is the
most expensive time to find it.

**Breadth-first: a minimal echo in all six languages.** De-risks packaging and CI early, which is
where polyglot projects usually die ([ADR-0019](0019-build-and-distribution.md)). Genuinely tempting.
Rejected because it validates the boundary against a trivial workload; an echo exercises none of the
zero-copy views, command buffering, or per-tick lifetime rules that the real design depends on.

**Rollback demo first.** The flashiest feature, and a forcing function for determinism. Rejected as
inverted: rollback sits on top of the arena, the fixed-point maths and the snapshot mechanism, so
building it first means building all of those badly first.

**Everything at once across a team.** Viable with more people and clear interface contracts —
which is what Phase 0 produces. Available later; not applicable now.
