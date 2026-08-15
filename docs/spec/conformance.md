# Specification: the conformance suite

**Status:** normative.

`tempo` will have six independent implementations of bit-exact behaviour — fixed-point arithmetic,
the wire codec, and schema canonicalisation. Nothing about six ports of the same algorithm is
self-correcting. The conformance suite is the mechanism that makes
[ADR-0002](../adr/0002-fixed-point-determinism.md) and
[ADR-0009](../adr/0009-custom-bitpacked-wire-format.md) survivable rather than aspirational.

The suite is not a test *suite* in the usual sense. It is the **operational definition of
correctness**: an implementation that passes is conforming, and one that does not is broken,
regardless of how reasonable its behaviour looks.

---

## 1. Structure

```
conformance/
  vectors/
    fixed-point.json     scalar and vector arithmetic
    wire.json            bit packing, varints, quantization, snapshot deltas
    schema.json          canonical form and schema IDs
    protocol.json        handshake sequences and packet framing
  tables/
    sin.bin  atan.bin  exp2.bin  log2.bin       committed lookup tables
  runners/
    rust/ python/ typescript/ go/ c/ cpp/
```

Vectors are JSON for universal parsing. Tables are raw little-endian `i64` arrays, because they are
large and exactness matters more than readability.

---

## 2. Vector format

Every vector file is:

```json
{
  "format": 1,
  "suite": "fixed-point",
  "cases": [
    { "id": "mul.neg.trunc", "op": "mul",
      "args": ["-0x100000000", "0x80000000"], "expect": "-0x80000000" }
  ]
}
```

- `id` is stable and unique; it appears in failure output and in bug reports, so it must not be
  renumbered when cases are added.
- Integer values are hex strings for `Fx` raw values and plain decimals elsewhere. Strings avoid
  every language's JSON number-precision problem — JavaScript cannot represent an `i64` as a JSON
  number, so numeric encoding would silently corrupt cases on one of the six runners.
- Byte sequences are lowercase hex strings.
- `expect` is exact. **There is no tolerance field, ever.** A tolerance would mean an implementation
  could pass while producing a desync.

---

## 3. Required coverage

A vector file is not complete until it covers its category below.

**Fixed point.** Saturation at `MIN` and `MAX` for every operation; sign behaviour for `mul`
(floors) and `div` (truncates toward zero) on all four sign combinations; division by zero in all
three cases; `frac` and `floor` for negative inputs; transcendentals at table index 0, at the last
index, at exact interpolation midpoints, at negative angles, and at magnitudes large enough to
exercise range reduction; `normalize` of a zero vector; `atan2(0, 0)`; quaternion multiplication
where intermediates saturate.

**Wire.** Fields spanning byte boundaries; a single-bit field as the only content; `varuint` at every
7-bit group boundary and at maximum width; quantization at `min`, at `max`, at an exact half-step,
and outside the range in both directions; smallest-three quaternions including a negative largest
component and a near-unit vector whose reconstruction requires the zero clamp; snapshot deltas
containing spawns, despawns, index gaps, and a reused index with a new generation; a baseline-less
full snapshot.

**Schema.** Byte-wise versus locale-aware name sorting (`Z` must sort before `a`); defaulted
parameters absent from the canonical form; `Fx` parameters whose decimal form is not exactly
representable; an empty component.

**Protocol.** Handshake packet sequences including the challenge round trip; ack bitfield generation
under loss and reordering; fragment reassembly including out-of-order arrival and a duplicate
fragment.

---

## 4. Property tests

Fixed vectors catch known edge cases. Property tests catch the ones nobody thought of, and are
required in every implementation that has a property-testing library available:

- `decode(encode(v)) == quantize(v)` for any field and value
- `apply_delta(baseline, delta(baseline, target)) == target` for any pair of world states
- `bit_read(bit_write(x, n), n) == x` for any `x` fitting in `n` bits
- `canonical(schema) == canonical(shuffle_declaration_order(schema))`
- `sqrt(x)² <= x < (sqrt(x) + ulp)²` for any non-negative `x`
- rollback determinism: simulating ticks `0..n` directly, and simulating `0..k` then rolling back to
  `j` and re-simulating to `n`, produce identical state hashes

The last one is the property the entire project depends on, and it is checked in CI on every commit
rather than trusted.

---

## 5. Cross-language matrix

Beyond per-implementation vectors, CI runs an **interoperability matrix**: for each ordered pair of
languages, a server in one and a client in the other run a scripted session over the simulated-link
transport with a fixed seed, and both must report identical final state hashes.

Six languages give 36 ordered pairs. This is the test that catches divergence the vectors miss,
because it exercises the implementations against each other rather than against a shared expectation.

---

## 6. Generating and changing vectors

`cargo run -p tempo-conformance --bin generate` regenerates vectors from the Rust implementation.

The obvious hazard: if vectors are generated from Rust, they encode Rust's bugs as the standard. Two
controls:

1. **Every vector must be justified by the spec.** A generated case is reviewed against
   [fixed-point.md](fixed-point.md) or [wire-protocol.md](wire-protocol.md) before being committed.
   Where the spec is silent, the spec is amended first — a vector is never the place a behaviour is
   defined for the first time.
2. **Changing an existing vector's expectation is a protocol change.** It requires an ADR or a spec
   amendment, and a protocol version bump if it alters the wire format. Regenerating vectors to make
   a failing build pass is explicitly forbidden, and reviewers should treat a diff that modifies
   `expect` values with suspicion.

Adding new cases is unrestricted and encouraged; every bug fixed in any implementation should arrive
with the vector that would have caught it.
