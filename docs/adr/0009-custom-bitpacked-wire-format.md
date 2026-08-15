# ADR-0009: A custom bit-packed wire format

**Status:** accepted

## Context

Bandwidth is the binding constraint in realtime multiplayer. A server sending 20 snapshots per
second to 100 clients, each snapshot describing 200 visible entities, sends 400,000 entity-updates
per second. At 20 bytes per entity that is 8 MB/s; at 6 bytes it is 2.4 MB/s. The difference decides
how many players fit on a machine and whether players on poor connections can play at all.

General-purpose serialisation formats are built for different goals. Protobuf and MessagePack are
self-describing and schema-evolvable, paying field tags and length prefixes on every field.
FlatBuffers and Cap'n Proto optimise for zero-copy random access, paying alignment padding and
vtables. Both families are byte-aligned, which means a boolean costs 8 bits and an angle that needs
9 bits of precision costs 32.

For our access pattern — sequential encode, sequential decode, schema known on both sides from the
negotiated hash ([ADR-0003](0003-native-first-schema-derivation.md)) — every one of those costs buys
nothing.

## Decision

A custom **bit-packed, schema-driven** wire format, specified normatively in
[`spec/wire-protocol.md`](../spec/wire-protocol.md).

- **Bit-granular.** A boolean is 1 bit. An enum with five variants is 3 bits. A value in a known
  range uses `ceil(log2(range))` bits.
- **Quantization is declared in the schema.** A position field with `quantize = 0.001` over a
  ±1000 m world becomes a 21-bit integer instead of a 32-bit float. The declaration is part of the
  schema hash, so both sides agree by construction.
- **No field tags, no lengths.** Both sides know the schema; the bit stream is positional.
- **Dirty bitmask per component.** One bit per field indicates presence, so unchanged fields cost
  exactly one bit.
- **Varints for unbounded integers**, zig-zag encoded when signed.
- **Delta against a per-client acknowledged baseline** — the largest win of all, because most fields
  do not change between snapshots. See [`design/replication-pipeline.md`](../design/replication-pipeline.md).
- **1200-byte MTU** with our own fragmentation ([ADR-0010](0010-reliability-channels.md)).

Encoding is specified precisely enough — bit order, integer endianness, rounding of quantized values
at the half-step, saturation behaviour outside the declared range — that six independent
implementations produce identical bytes. Conformance vectors enforce it
([`spec/conformance.md`](../spec/conformance.md)).

Deliberately excluded: general-purpose compression over the packet. It costs CPU per packet per
client and delivers little once data is quantized and delta-encoded, because the entropy that
remains is genuinely random.

## Consequences

- Typical entity updates land in single-digit bytes rather than tens. This is the decision that
  makes the 10k-entity workload feasible.
- The format is **not self-describing**, so a schema mismatch corrupts the stream rather than
  producing a clean error. This is exactly why schema hashes are negotiated at connect and refused
  on mismatch (ADR-0003); the two decisions are load-bearing for each other.
- Debugging raw packets requires tooling, since a hex dump is meaningless. `tempo inspect` decodes
  captures using the schema ([ADR-0020](0020-observability.md)) — this is a hard requirement, not a
  nicety.
- Six implementations of bit-exact encoding is a real risk. Conformance vectors are the control.
- Users must declare ranges and precision for quantized fields. Values outside the declared range
  saturate, which is a specified behaviour rather than an error, and a source of confusing bugs if
  the range is chosen carelessly. The guide is blunt about this.

## Alternatives considered

**Protobuf.** Ubiquitous, excellent tooling, mature libraries in all six languages, and schema
evolution solved. Rejected on size: field tags and byte alignment typically triple our payloads, and
it cannot express quantization at all, so floats stay 32 bits.

**FlatBuffers / Cap'n Proto.** Zero-copy access is genuinely valuable and would pair well with the
arena. Rejected because their zero-copy property optimises random access to a received buffer, while
our decode is a sequential sweep into the arena — we pay the alignment and vtable overhead for a
benefit we do not use.

**MessagePack / CBOR.** Compact and self-describing with tiny libraries. Rejected for the same
reason as protobuf: self-description is overhead when the schema is negotiated.

**Bincode or a similar Rust-native format.** Fast and compact for Rust. Rejected because it is
byte-aligned, has no quantization, and has no serious cross-language story.

**Byte-aligned custom format.** Simpler to implement and debug, and captures most of the delta
compression win. Rejected because booleans and small enums are extremely common in game state, and
rounding each to a byte gives up a large fraction of the remaining benefit for a modest
implementation saving.
