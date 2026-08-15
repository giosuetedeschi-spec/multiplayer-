# ADR-0003: Schemas declared natively, canonicalised at runtime

**Status:** accepted

## Context

Because the core owns replicated state ([ADR-0001](0001-core-owns-replicated-state.md)), it must
know the shape of that state: which components exist, which fields they have, each field's type,
quantization, delta strategy and priority. Two peers can only exchange deltas if they agree on this
schema exactly — a one-field disagreement misaligns the entire bit stream, and because the format is
bit-packed rather than self-describing, the failure is not a clean error but garbage state.

There are two established ways to establish that agreement, and they pull in opposite directions.

An **interface definition language** — a `.proto`-style file compiled to types in each language —
guarantees agreement at build time and handles mixed-version fleets well. It also puts a mandatory
code generation step in front of every user in every project, which is precisely the friction that
"plug and play, just import it" is meant to eliminate. `pip install tempo` followed by "now install
the schema compiler and wire it into your build" is a different product.

Declaring schemas **natively** — a decorator, a derive macro, a struct tag — has no build step and
feels like the language you are already writing. But nothing then forces two languages to agree, and
drift surfaces at runtime, in the field.

## Decision

Schemas are declared **natively in each language**, and the core **derives a canonical form** from
them at registration time.

Declaration is idiomatic per language:

| Language | Mechanism |
|---|---|
| Rust | `#[derive(Replicate)]` with `#[replicate(...)]` field attributes |
| Python | `@replicated` class decorator over annotated class attributes |
| TypeScript | class decorators, or a `defineComponent` builder for decorator-free setups |
| Go | struct tags: `` tempo:"quantize=0.001,delta=dirty_mask" `` |
| C / C++ | an explicit descriptor table, or the C++ macro helpers |

At registration the core normalises each declaration into the **canonical schema form** specified in
[`spec/schema-and-hashing.md`](../spec/schema-and-hashing.md) — a deterministic, language-independent
encoding of component names, field names, types, quantization parameters, delta strategies and
ordering. Field order is canonicalised by sorting on field name, so declaration order in the host
language is irrelevant and cannot cause drift.

That canonical form is hashed (BLAKE3, truncated to 128 bits) into a **schema ID**, which is
exchanged during the connection handshake. A mismatch is refused at connect time with a **structured
diff** naming the component, the field, and the specific disagreement — not a generic "protocol
error", because a bad error message here costs hours.

Code generation still exists, but as an **optional accelerator**, not a requirement. `tempo schema
export` writes the canonical form to a file, and `tempo schema gen` produces native declarations for
other languages from it. A polyglot team can put the exported schema in CI and fail the build on
drift, getting IDL-grade safety without imposing it on the person trying the library for the first
time.

## Consequences

- The zero-friction path is genuinely zero-friction: import, decorate, run. No build step, no
  compiler, no plugin.
- Schema disagreement is caught at **connect time**, not at build time. That is later than an IDL,
  but it is deterministic, immediate, and produces a readable diff — not a corrupted simulation.
- Name-based canonical ordering means renaming a field is a wire-breaking change while reordering is
  free. This is the right trade for our field-name-addressed model, but it must be prominent in the
  compatibility guide ([ADR-0017](0017-versioning-and-compatibility.md)).
- Each binding must implement schema reflection, and all six must produce byte-identical canonical
  forms. This is conformance-tested; a binding that canonicalises differently is a build failure,
  not a runtime surprise.
- Runtime declaration costs a small amount of startup work and means the schema is not available to
  static analysis. Type-level ergonomics (autocomplete on replicated fields) come from the native
  declaration itself, which is the point of declaring natively.

## Alternatives considered

**Mandatory IDL with code generation.** Bulletproof for rolling deploys and mixed-version fleets,
and gives compile-time errors instead of connect-time ones. Rejected as the default because a
required build step in six ecosystems contradicts the core promise of the project. Preserved in full
as the optional path, so teams that need it lose nothing.

**Native declaration with no canonical form and no hash.** The lightest possible design. Rejected
because bit-packed formats fail catastrophically rather than gracefully on schema mismatch; without
a negotiated hash, the first symptom of drift is corrupted state on a player's machine.

**Self-describing wire format (field tags on every message, protobuf-style).** Makes mismatch
survivable rather than fatal and removes the need for negotiation. Rejected on bandwidth: tags and
lengths on every field defeat the entire point of quantized bit-packing
([ADR-0009](0009-custom-bitpacked-wire-format.md)), typically tripling snapshot size, and bandwidth
is the binding constraint at the entity counts we target.

**Schema negotiation with automatic structural migration** — peers exchange schemas and translate
between versions on the fly. Attractive and genuinely useful for long-lived persistent worlds.
Rejected for v1 on complexity: it requires a full type-compatibility lattice and per-field migration
rules. Deferred to [ADR-0017](0017-versioning-and-compatibility.md) as future work.
