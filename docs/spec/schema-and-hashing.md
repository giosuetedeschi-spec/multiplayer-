# Specification: canonical schema form and schema ID

**Status:** normative. Schema format version **1**.

Rationale: [ADR-0003](../adr/0003-native-first-schema-derivation.md). This document defines the
language-independent canonical form that native declarations are reduced to, and the hash computed
over it. Two peers agree if and only if their schema IDs match.

---

## 1. Why a canonical form exists

The wire format is positional and not self-describing ([ADR-0009](../adr/0009-custom-bitpacked-wire-format.md)).
A single disagreement about field order, width, or quantization misaligns the entire bit stream, and
the result is not an error but corrupted state.

Six languages declare schemas six different ways. The canonical form is the single agreed
interpretation of all of them: a Rust `#[derive(Replicate)]`, a Python `@replicated` class, and a Go
struct with tags describing the same component MUST reduce to byte-identical canonical forms.

---

## 2. The type vocabulary

A field's type is one of:

| Type | Parameters |
|---|---|
| `bool` | — |
| `u8` `u16` `u32` `u64` | optional `bits` |
| `i8` `i16` `i32` `i64` | optional `bits` |
| `fx` | optional `quantize`, `min`, `max` |
| `vec2` `vec3` | same optional parameters as `fx`, applied per component |
| `quat` | optional `quantize_bits` (smallest-three `k`) |
| `enum` | `variants` (count) |
| `string` | optional `max_len` |
| `bytes` | optional `max_len` |

Every parameter that affects encoding is part of the canonical form. Parameters that do not affect
encoding — `base_priority` being the notable one — are **excluded**, so tuning priority never breaks
wire compatibility.

---

## 3. Canonical form

The canonical form is UTF-8 text with `\n` line endings and no trailing newline. It is designed to be
readable, because it is what a developer sees in a schema-mismatch diff.

```
tempo-schema 1
component <component_name>
  field <field_name> <type> <param>=<value> ...
  field ...
component ...
```

Rules, all normative:

1. **Components are sorted** by name, byte-wise ascending on the UTF-8 encoding.
2. **Fields within a component are sorted** by name, byte-wise ascending. Declaration order in the
   host language is therefore irrelevant — this is what makes reordering a non-breaking change and
   renaming a breaking one ([ADR-0017](../adr/0017-versioning-and-compatibility.md)).
3. **Parameters are sorted** by parameter name and only emitted when explicitly set. A defaulted
   parameter is absent, never emitted with its default value; emitting defaults would make adding a
   new parameter with a default a breaking change for everyone.
4. **Names** match `[A-Za-z_][A-Za-z0-9_]*`. Names are compared byte-wise; case matters.
5. **Indentation** is exactly two spaces for `field` lines. Component lines are not indented.
6. **Numeric parameters** are rendered as follows, with no other forms permitted:
   - integers: decimal, no leading zeros, `-` for negatives
   - `Fx` values (`quantize`, `min`, `max`): the **raw `i64`** in lowercase hexadecimal with a `0x`
     prefix and no leading zero padding

Point 6 is the one that matters most. Rendering `Fx` parameters as their raw integer rather than a
decimal string removes float formatting from the canonicalisation path entirely. Two languages
formatting `0.001` as `0.001` and `1e-3` would produce different hashes for identical schemas; two
languages formatting the same `i64` cannot.

### 3.1 Example

A Rust declaration:

```rust
#[derive(Replicate)]
struct Player {
    #[replicate(quantize = "0.001", min = "-1000", max = "1000", priority = 2.0)]
    position: Vec2,
    health: Fx,
    #[replicate(bits = 10)]
    score: u32,
    alive: bool,
}
```

canonicalises to:

```
tempo-schema 1
component Player
  field alive bool
  field health fx
  field position vec2 max=0x3e800000000 min=-0x3e800000000 quantize=0x418937
  field score u32 bits=10
```

Fields are name-sorted, `priority` is absent because it does not affect encoding, and the `Fx`
parameters appear as raw values.

---

## 4. Schema ID

```
schema_id = first 16 bytes of BLAKE3(canonical_form_utf8_bytes)
```

Rendered in diagnostics as 32 lowercase hex characters. Transmitted in the handshake as 16 bytes.

128 bits truncated from BLAKE3 is far beyond collision risk for this population, and keeps the
handshake small.

---

## 5. Mismatch diagnostics

On mismatch a peer MUST produce a structured diff, not a generic error. The required content:

- components present on one side only, by name
- fields present on one side only, by component and name
- fields present on both sides whose type or encoding parameters differ, with both renderings

Recommended presentation:

```
schema mismatch: local 8f3a…c21b, remote 4d19…77e0

  component Player
    - field armour u16          (remote only)
    ~ field position vec2       quantize: local 0x418937, remote 0x10624dd
    + field stamina fx          (local only)
```

This is normative because it is the difference between a five-minute fix and an afternoon: the
underlying failure — a positional bit stream misaligning — gives the user no other signal to work
from.

---

## 6. Registration requirements

Implementations MUST:

- reject duplicate component names, and duplicate field names within a component
- reject names not matching the identifier pattern
- reject `quantize` with a non-positive step, or `min >= max`
- compute `bits` for quantized fields exactly as specified in
  [wire-protocol.md §5.1](wire-protocol.md#51-quantized-fx), using fixed-point arithmetic only
- freeze the schema after the first connection attempt; late registration changes the ID under a
  live connection and must be an error rather than a silent renegotiation

---

## 7. Conformance

`conformance/vectors/schema.json` pairs declarations with their expected canonical form and schema
ID. Every binding must produce byte-identical canonical text and identical IDs, including for:

- names requiring byte-wise rather than locale-aware sorting (`Z` before `a`)
- defaulted parameters, which must be absent
- `Fx` parameters whose decimal representation is not exactly representable
- empty components, and components with a single field
