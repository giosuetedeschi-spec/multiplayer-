# ADR-0017: Protocol and schema version negotiation

**Status:** accepted

## Context

Three things can independently disagree between two `tempo` participants:

1. **The protocol** — packet headers, handshake, channel semantics.
2. **The schema** — the user's components and fields ([ADR-0003](0003-native-first-schema-derivation.md)).
3. **The ABI** — the boundary between a binding and the core ([ADR-0008](0008-c-abi-single-boundary.md)).

Because the wire format is not self-describing ([ADR-0009](0009-custom-bitpacked-wire-format.md)),
disagreement on (1) or (2) does not degrade gracefully — it produces silently corrupted state. That
makes explicit negotiation mandatory rather than a nicety.

There is also an operational reality: rolling deploys mean two versions of a game server run
simultaneously, and shipped clients cannot be forced to update instantly.

## Decision

**Three independent version identifiers**, each negotiated or checked at the appropriate point.

**Protocol version** — a `u16` in every packet header. Incremented on any wire-format change. A
mismatch is rejected at handshake with a specific error. Servers may accept a *range* of protocol
versions and encode to the client's version, which is what makes rolling deploys possible.

**Schema ID** — a 128-bit BLAKE3 hash of the canonical schema form
([`spec/schema-and-hashing.md`](../spec/schema-and-hashing.md)), exchanged in the handshake. Mismatch
is refused with a **structured diff** — the component, the field, and the nature of the disagreement.
A generic "protocol error" here would cost every user hours, so the diff is a requirement of the
design rather than a debugging aid.

**ABI version** — a semantic version exposed by `tempo_abi_version()` and checked by each binding at
load. A binding built against an incompatible core fails loudly at import rather than corrupting
memory later.

**What is a breaking schema change.** Because canonical ordering is by field *name*:

| Change | Breaking? |
|---|---|
| Reordering fields in a declaration | No — canonicalisation sorts by name |
| Renaming a field | **Yes** — it is a different field |
| Adding or removing a field or component | **Yes** |
| Changing a type, quantization, or delta strategy | **Yes** |
| Changing `base_priority` | No — a scheduling hint, not wire-visible |

This is stated as a table because "renaming is breaking but reordering is not" is exactly the kind of
rule that is obvious once known and expensive to discover.

**Compatibility policy.**

- Protocol versions are supported for a documented window, so servers span at least one client
  release cycle.
- ABI changes are additive within a major version; functions are never removed or resignatured.
- The committed `tempo.h` is diffed in CI, so ABI changes are visible in review.

**Deliberately deferred: automatic schema migration** — negotiating between differing schemas and
translating on the fly. Genuinely valuable for long-lived persistent worlds, and genuinely large: it
needs a type-compatibility lattice, per-field migration rules, and a bidirectional translation layer
in the hot path. Recorded here as future work rather than pretended away.

## Consequences

- Version mismatch fails fast and legibly instead of corrupting state.
- Rolling deploys work for protocol changes; they do **not** work for schema changes, which require a
  coordinated update. This is a real operational constraint and is called out in
  [`ops/deployment.md`](../ops/deployment.md).
- Three version numbers is more to track than one, but conflating them would mean an ABI change
  forcing a wire-protocol bump for no reason.
- The structured diff requires shipping enough schema metadata in the handshake to describe
  disagreements. It is a one-time connect cost, not per-packet, and is worth it.

## Alternatives considered

**A single version number for everything.** Simplest to reason about. Rejected because the three
things change at genuinely different rates; coupling them means unnecessary breakage in two of the
three.

**Self-describing wire format so mismatch degrades gracefully.** Would remove the need for schema
negotiation entirely. Rejected on bandwidth grounds in ADR-0009 — this ADR is part of the price paid
for that decision.

**Schema evolution rules like protobuf's** — optional fields, reserved tags, forward compatibility by
construction. The right long-term answer. Rejected for v1 because it requires field-tag-addressed
encoding, which conflicts with positional bit-packing. Revisitable via an explicitly versioned
component variant scheme.

**No negotiation; trust operators to deploy matching versions.** Zero implementation. Rejected
because the failure mode is silent state corruption in production, which is the worst possible
outcome to trade implementation effort against.
