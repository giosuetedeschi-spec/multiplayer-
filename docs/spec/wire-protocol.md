# Specification: the tempo wire protocol

**Status:** normative. Protocol version **1**.

Rationale for a custom bit-packed format: [ADR-0009](../adr/0009-custom-bitpacked-wire-format.md).
Rationale for the channel model: [ADR-0010](../adr/0010-reliability-channels.md).

---

## 1. Conventions

**Byte order.** All multi-byte integers in byte-aligned regions are **little-endian**.

**Bit order.** Within bit-packed regions, bits are written **least-significant-bit first**: the first
bit written occupies bit 0 of byte 0, the ninth bit occupies bit 0 of byte 1. A value of `n` bits is
written with its least significant bit first. Trailing bits of the final byte are zero-filled.

This is stated first because it is the single most common source of cross-implementation
disagreement, and it is not the convention every developer assumes.

**MTU.** The maximum datagram payload is **1200 bytes**, chosen to sit below the smallest path MTU
we expect on the public internet. Anything larger is fragmented by us
([§7](#7-fragmentation)) rather than by IP.

**`varuint`.** A bit-granular LEB128: repeated groups of `{ 1 continuation bit, 7 payload bits }`,
payload least-significant group first, continuation bit set on all but the last group. `varint` is
the same applied to a zig-zag encoded signed value (`(n << 1) ^ (n >> 63)`).

---

## 2. Datagram framing

Every datagram begins with a byte-aligned header. The header is **associated data** for the AEAD and
is transmitted in the clear; everything after it is sealed.

```
offset  size  field
0       1     packet_type            u8
1       2     protocol_version       u16   (currently 1)
3       4     sequence               u32   per-connection, starts at 0, monotonic
--------------- end of associated data ---------------
7       n     ciphertext             AEAD-sealed body
7+n     16    auth tag
```

`packet_type`:

| Value | Type | Sealed? |
|---|---|---|
| 0 | `ConnectionRequest` | token-sealed, see [ADR-0015](../adr/0015-connect-tokens-and-security.md) |
| 1 | `ConnectionChallenge` | yes |
| 2 | `ConnectionResponse` | yes |
| 3 | `KeepAlive` | yes |
| 4 | `Payload` | yes |
| 5 | `Disconnect` | yes |

**AEAD.** ChaCha20-Poly1305 (IETF, 96-bit nonce). The nonce is `8 zero bytes || sequence (u32 LE)`.
Keys are the per-session ephemeral keys from the connect token, so sequence restarts at zero are safe
across sessions. A connection MUST be re-keyed or terminated before `sequence` wraps.

`ConnectionRequest` packets are padded to exactly 1200 bytes so that the request is never smaller
than the response, denying amplification.

---

## 3. Payload body

The sealed body of a `Payload` packet is:

```
offset  size  field
0       2     ack            u16   low 16 bits of the highest sequence received
2       4     ack_bits       u32   bit i set = (ack - 1 - i) was received
6       ...   one or more blocks
```

One acknowledgement therefore covers up to 33 packets, and because acks ride on every packet the
redundancy makes them robust to loss without needing their own reliability.

**Blocks** are byte-aligned at their boundaries; their contents may be bit-packed internally.

```
0       1     block_type     u8
1       var   block_length   varuint, byte-aligned encoding, in bytes
...     n     block payload  (padded with zero bits to a byte boundary)
```

| `block_type` | Contents |
|---|---|
| 0 | `Snapshot` — replication delta ([§4](#4-the-snapshot-block)) |
| 1 | `InputBatch` — client inputs for a tick range |
| 2 | `ReliableOrdered` messages |
| 3 | `ReliableUnordered` messages |
| 4 | `UnreliableSequenced` messages |
| 5 | `Fragment` ([§7](#7-fragmentation)) |
| 6 | `TimeSync` ([§6](#6-timesync-block)) |
| 7 | `SchemaDiff` — handshake only ([§8](#8-handshake)) |
| 8 | `StateHash` — desync detection |
| 9 | `Command` — spawn/despawn/RPC, always on channel `ReliableOrdered` |

A receiver MUST skip unknown block types using `block_length` rather than failing, so that additive
protocol extensions do not break older peers within the same protocol version.

---

## 4. The snapshot block

This is where the bandwidth is won or lost. All contents are bit-packed.

```
u32   tick                    the tick this snapshot describes
u32   baseline_tick           the acked snapshot this is encoded against;
                              0xFFFFFFFF means "full state, no baseline"
varuint  entity_count
repeat entity_count times:
    varuint  entity_index_delta      index minus previous index, minus 1; entities ascending
    2 bits   entity_op               0 = update, 1 = spawn, 2 = despawn, 3 = reserved
    if entity_op == spawn:
        varuint  archetype_id
        u32      generation
    if entity_op != despawn:
        C bits   component_present_mask     C = component count in this archetype
        for each present component, in canonical order:
            F bits   field_dirty_mask       F = field count in this component
            for each dirty field, in canonical order:
                <field encoding, §5>
```

Notes that matter for implementers:

- Entities are **sorted ascending by index** and encoded as gaps, so a snapshot of nearby entities
  costs a few bits per entity for identity.
- `entity_op == spawn` carries the generation so that a client can distinguish a reused index from
  the entity it replaced. Omitting this is a classic source of ghost entities.
- Against a baseline, `field_dirty_mask` marks fields differing from the baseline value. Against no
  baseline (`0xFFFFFFFF`) every field is present and the mask is all ones — encoded anyway, so the
  decoder has one code path rather than two.
- Canonical order for components and fields is defined in
  [schema-and-hashing.md](schema-and-hashing.md) and is **name-sorted**, never declaration order.

---

## 5. Field encodings

The encoding of a field is fully determined by its schema declaration, which both peers agree on via
the negotiated schema ID.

| Declared type | Encoding |
|---|---|
| `bool` | 1 bit |
| `u8`…`u64` | `varuint`, or exactly `n` bits if `bits = n` is declared |
| `i8`…`i64` | `varint` (zig-zag), or `n` bits if declared |
| `enum(k variants)` | `ceil(log2(k))` bits |
| `Fx` unquantized | 64 bits, raw two's-complement value |
| `Fx` quantized | see below |
| `Vec2` / `Vec3` | each component encoded per the field's `Fx` rule |
| `Quat` quantized | smallest-three, see below |
| `string` | `varuint` byte length, then bytes, byte-aligned |
| `bytes` | `varuint` length, then bytes, byte-aligned |

### 5.1 Quantized `Fx`

Declared as `quantize = p` with `min = lo`, `max = hi`. Let

```
steps = floor((hi - lo) / p) + 1
bits  = ceil(log2(steps))
```

All of it is computed on **raw `Fx` values in 128-bit integer arithmetic**, never with `Fx`
division and never with floats. `Fx` division would round twice and would saturate for wide ranges;
raw arithmetic is exact because both operands carry the same `2^32` scale, so the ratio is unscaled.

```
steps = (max_raw - min_raw) / step_raw + 1          // integer division, exact
bits  = bit_width(steps - 1)                        // ceil(log2(steps)); 0 when steps == 1

clamped = clamp(v, lo, hi)
num     = clamped_raw - min_raw                     // never negative after clamping
q       = (num + step_raw / 2) / step_raw           // round half away from zero
q       = clamp(q, 0, steps - 1)
emit q as `bits` bits
```

Decoding:

```
v_raw = saturate(min_raw + q * step_raw)
```

A step coarser than the range yields `steps == 1` and therefore `bits == 0`: the field occupies no
space and always decodes to `min`. This is degenerate but reachable, and both the encoder and the
decoder must handle a zero-width field rather than treating it as an error.

Quantization is **lossy and specified**: `decode(encode(v))` is the nearest representable step, not
`v`. Values outside `[lo, hi]` **saturate silently**. This is deliberate — a range error mid-match
must not disconnect a player — and it is the reason the guide is emphatic about choosing ranges with
headroom.

`lo`, `hi` and `p` are part of the schema hash, so both peers compute identical `bits`.

### 5.2 Quantized `Quat` — smallest-three

A unit quaternion has three degrees of freedom, so one component is redundant.

```
i        = index of the component with the largest absolute value      (2 bits)
sign     = if that component is negative, negate all four components   (implicit, not transmitted)
remaining three components, each in [-1/sqrt(2), 1/sqrt(2)], quantized to `k` bits each
```

Total `2 + 3k` bits; at `k = 10` that is 32 bits for a rotation with roughly 0.1° of error, against
256 bits raw. Decoding reconstructs the omitted component as
`sqrt(max(0, 1 - a² - b² - c²))` using the fixed-point `sqrt`, and the `max(0, …)` clamp is required
— quantization can make the sum marginally exceed one, and taking the square root of a negative value
is exactly the kind of edge case that desyncs one platform and not another.

---

## 6. `TimeSync` block

Supports the clock model in [ADR-0016](../adr/0016-clock-sync-and-time-dilation.md).

```
u32  client_tick             tick the sender is currently simulating
u32  last_processed_input    server → client only: last input tick consumed
i16  dilation_hint           server → client: parts-per-thousand rate adjustment, clamped to ±50
u8   input_buffer_occupancy  server → client: ticks currently buffered
u64  echo_timestamp          opaque; echoed back unmodified for RTT measurement
```

`echo_timestamp` is opaque to the receiver and MUST be echoed byte-for-byte. Interpreting it is the
sender's business, which keeps clock representation out of the protocol.

---

## 7. Fragmentation

Payloads exceeding the MTU are split.

```
u32      message_id
varuint  fragment_index
varuint  fragment_count
bytes    fragment payload
```

Requirements, all of which are about not being a denial-of-service target:

- Reassembly buffers are **bounded** — a configured maximum of in-flight messages and total bytes.
- Incomplete reassemblies **time out** (default 2 s) and are discarded.
- `fragment_count` is validated against the configured maximum **before** any allocation.
- A fragment index at or beyond `fragment_count` is a protocol violation; the packet is dropped.

Snapshots are never fragmented. If a snapshot does not fit, the priority accumulator
([ADR-0011](../adr/0011-interest-management.md)) sends less this tick — that is precisely what it is
for. Fragmentation exists for large reliable messages: initial state transfer, schema diffs, host
migration payloads.

---

## 8. Handshake

```
client → server   ConnectionRequest    connect token, padded to 1200 bytes
server → client   ConnectionChallenge  challenge nonce, sealed
client → server   ConnectionResponse   challenge echo, sealed
server → client   KeepAlive            connection established; carries assigned client index
client ↔ server   SchemaDiff           schema ID exchange
```

The challenge round trip completes **before the server allocates connection state**, which is what
makes source-address spoofing unprofitable.

The `SchemaDiff` block carries the 128-bit schema ID. On mismatch the server responds with a
`SchemaDiff` containing the structured difference — component name, field name, and the nature of the
disagreement — and then disconnects. Producing a useful diff rather than a generic error is a
normative requirement, not a quality-of-implementation detail: a bit-packed format that fails on
schema mismatch is unusable without it.

---

## 9. Conformance

Encoders and decoders MUST reproduce the vectors in
[`conformance/vectors/wire.json`](../../conformance/vectors/), which cover:

- bit-order edge cases: values spanning byte boundaries, single-bit fields, empty blocks
- `varuint` and `varint` at every group boundary, and at maximum width
- quantization at range endpoints, at exact half-steps, and outside the declared range
- smallest-three quaternion encoding including the negative-largest-component case, and the
  clamped reconstruction near unit length
- snapshot deltas with spawns, despawns, index gaps, and generation reuse
- baseline-less full snapshots

Round-trip property tests are required in addition to the fixed vectors: for any schema and any
state, `decode(encode(s))` must equal `quantize(s)`, and applying a delta to its baseline must equal
the full snapshot.
