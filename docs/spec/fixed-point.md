# Specification: deterministic fixed-point arithmetic

**Status:** normative. Version 1.

This document specifies `tempo`'s fixed-point arithmetic exactly. Every language implementation must
reproduce these results **bit for bit**. Where this document and an implementation disagree, the
implementation is wrong.

Rationale for using fixed point at all: [ADR-0002](../adr/0002-fixed-point-determinism.md).

---

## 1. The scalar type `Fx`

`Fx` is a signed **Q32.32** fixed-point number stored in a two's-complement 64-bit integer.

| | |
|---|---|
| Storage | `i64` (the *raw* value) |
| Interpretation | `value = raw / 2^32` |
| `ONE` | `raw = 0x0000_0001_0000_0000` |
| Resolution | `2^-32` ≈ 2.328306e-10 |
| Range | `[-2147483648.0, 2147483647.999999999767)` |
| `MIN` / `MAX` | `raw = i64::MIN` / `raw = i64::MAX` |

Implementations MUST expose the raw value and construction from a raw value. Serialisation of an
`Fx` is the serialisation of its raw `i64`.

### 1.1 Constants

Committed as exact raw values so that no implementation computes them:

| Name | Raw value (hex) | Approx |
|---|---|---|
| `ZERO` | `0x0000_0000_0000_0000` | 0 |
| `ONE` | `0x0000_0001_0000_0000` | 1 |
| `HALF` | `0x0000_0000_8000_0000` | 0.5 |
| `PI` | `0x0000_0003_243F_6A89` | 3.14159265358... |
| `TAU` | `0x0000_0006_487E_D511` | 6.28318530717... |
| `FRAC_PI_2` | `0x0000_0001_921F_B544` | 1.57079632679... |
| `INV_TAU` | `0x0000_0000_28BE_60DC` | 0.15915494309... |
| `E` | `0x0000_0002_B7E1_5163` | 2.71828182845... |

Each is the mathematical value rounded to nearest representable `Fx`, ties away from zero.

### 1.2 Rounding and overflow

Two rules, applied everywhere:

- **Truncation is toward negative infinity** (arithmetic shift right), not toward zero, wherever a
  product or quotient is reduced back to Q32.32 by shifting. This is stated explicitly because it is
  the most common source of cross-language divergence: several languages' shift operators differ for
  negative operands.
- **Overflow saturates.** Results exceeding the representable range clamp to `MAX` or `MIN`. Wrapping
  is forbidden; it converts a small numeric error into a catastrophic one, and a saturated value is
  at least monotonic. `checked_*` variants returning an optional/none are provided for callers that
  need to detect it.

### 1.3 Arithmetic

Let `a`, `b` be raw `i64` values.

**Addition, subtraction, negation.** Saturating integer operations on the raw values.

```
add(a, b) = saturate_i64(a + b)          // computed in 128-bit
sub(a, b) = saturate_i64(a - b)
neg(a)    = saturate_i64(-a)             // neg(MIN) = MAX
```

**Multiplication.** Computed in 128 bits, then arithmetic-shifted right by 32.

```
mul(a, b) = saturate_i64( (i128(a) * i128(b)) >> 32 )
```

The shift is arithmetic (sign-propagating) and therefore floors. `mul(-1, HALF)` yields raw `-1`
scaled — specifically `(-2^32 * 2^31) >> 32 = -2^31`, exactly `-0.5`. Where the mathematical product
is not representable in Q32.32, the result is the next value **toward negative infinity**.

**Division.**

```
div(a, b) = saturate_i64( (i128(a) << 32) / i128(b) )   // truncating toward ZERO
```

Note the asymmetry with `mul`, which is deliberate and must be reproduced: integer division in every
target language truncates toward zero, and specifying anything else would require every
implementation to add a correction step. Division is the one operation that truncates toward zero.

Division by zero does not trap:

```
div(a, 0) = MAX  if a > 0
          = MIN  if a < 0
          = ZERO if a == 0
```

**Remainder.** `rem(a, b) = a - mul(b, trunc(div(a, b)))`, with `rem(a, 0) = ZERO`.

### 1.4 Conversions

```
from_int(i)   = saturate_i64(i64(i) << 32)
to_int_floor  = a >> 32                    // arithmetic, floors
to_int_trunc  = (a >= 0) ? (a >> 32) : -((-a) >> 32)
floor(a)      = a & ~0xFFFFFFFF
ceil(a)       = floor(a + (ONE - 1))
round(a)      = floor(a + HALF)            // ties toward +infinity
frac(a)       = a - floor(a)               // always in [0, 1)
abs(a)        = saturate_i64(a < 0 ? -a : a)
signum(a)     = ONE, ZERO, or -ONE
```

Conversion **from** floating point exists only for construction convenience (parsing constants, test
setup). It is `raw = round_half_away_from_zero(x * 2^32)`, saturating. Implementations MUST NOT use
float conversion anywhere in the simulation path, and the conformance suite treats any float in a
simulation-path code path as a defect.

---

## 2. Transcendental functions

All transcendentals are computed from **committed lookup tables plus linear interpolation**, or from
exact integer algorithms. No implementation may call its platform's `libm`.

The tables are *data*, not computation: they are generated once, committed to the repository as
exact integer arrays, and are byte-identical in every language. How they were generated is
irrelevant to determinism; that they are fixed is the entire point.

Table data lives in [`conformance/tables/`](../../conformance/tables/) and is generated by
`cargo run -p tempo-tablegen`.

### 2.1 `sqrt`

Exact integer square root — no table, no approximation error beyond truncation.

For `a >= 0`, the result is the largest `r` such that `r² ≤ a · 2^32`:

```
sqrt(a) = isqrt_u128( u128(a) << 32 )      // floor of the exact square root
sqrt(a) = ZERO for a <= 0
```

`isqrt_u128` MUST be the exact floor of the integer square root. The bit-by-bit restoring algorithm
in Appendix A is the reference; any algorithm producing the identical exact floor is conforming.
Newton's method is acceptable **only** with a final correction step that guarantees exactness — an
uncorrected Newton iteration is off by one for some inputs and is therefore non-conforming.

### 2.2 `sin` and `cos`

Table `SIN_TABLE`: **4097** `i64` entries covering one full turn, where entry `i` is
`round_half_away_from_zero(sin(2π · i / 4096) · 2^32)`. Entry 4096 duplicates entry 0, removing the
wraparound branch.

```
sin(a):
    t    = mul(a, INV_TAU)                 // turns
    f    = t & 0xFFFF_FFFF                 // fractional turn, in [0,1) even for negative t
    i    = f >> 20                         // table index, 0..4095
    rem  = f & 0xF_FFFF                    // 20-bit interpolation weight
    lo   = SIN_TABLE[i]
    hi   = SIN_TABLE[i + 1]
    return lo + (( (i128(hi) - i128(lo)) * i128(rem) ) >> 20)

cos(a) = sin(add(a, FRAC_PI_2))
tan(a) = div(sin(a), cos(a))
```

Masking with `0xFFFF_FFFF` yields the positive fractional part for negative `t` as well, because
two's-complement `AND` computes `t mod 2^32`. This is why no explicit negative-angle branch is
needed, and implementations MUST NOT add one — a branch here is a divergence risk for no benefit.

Maximum absolute error versus exact `sin` is below `3e-7`.

### 2.3 `atan2`

Table `ATAN_TABLE`: **1026** `i64` entries where entry `i` is
`round_half_away_from_zero(atan(i / 1024) · 2^32)` for `i` in `0..=1024`, covering the ratio range
`[0, 1]`. Entry 1025 duplicates entry 1024: the ratio reaches exactly `1.0` whenever `|y| == |x|`,
which yields index 1024 and an interpolation read of index 1025. The sentinel keeps that read in
bounds and contributes zero, exactly as entry 4096 does for the sine table.

```
atan2(y, x):
    if x == 0 and y == 0: return ZERO      // defined, not an error
    ay = abs(y); ax = abs(x)
    if ay <= ax:  r = div(ay, ax); swapped = false
    else:         r = div(ax, ay); swapped = true
    i    = r >> 22                          // 0..1024
    rem  = r & 0x3F_FFFF                    // 22-bit weight
    base = ATAN_TABLE[i] + (((i128(ATAN_TABLE[i+1]) - i128(ATAN_TABLE[i])) * i128(rem)) >> 22)
    if swapped: base = FRAC_PI_2 - base
    // octant / quadrant fixup
    if x < 0:   base = PI - base
    if y < 0:   base = -base
    return base
```

`atan2(0, 0)` returns zero rather than being undefined, so that normalising a zero vector is total.

### 2.4 `exp2`, `log2`, `exp`, `ln`

Table `EXP2_TABLE`: **1025** entries, entry `i` = `round(2^(i/1024) · 2^32)`, covering `[1, 2)`.
Table `LOG2_TABLE`: **1025** entries, entry `i` = `round(log2(1 + i/1024) · 2^32)`.

```
exp2(a):
    n = to_int_floor(a)                     // integer part
    f = frac(a)                             // in [0,1)
    i = f >> 22 ; rem = f & 0x3F_FFFF
    m = EXP2_TABLE[i] + (((i128(EXP2_TABLE[i+1]) - i128(EXP2_TABLE[i])) * i128(rem)) >> 22)
    return saturate_i64( n >= 0 ? m << n : m >> (-n) )   // arithmetic shift

log2(a):
    if a <= 0: return MIN
    p = 63 - leading_zeros(a)                // index of the most significant set bit
    int_part = from_int(p - 32)
    m = (a << (63 - p)) as u64               // mantissa normalised to [2^63, 2^64)
    idx_bits = (m >> 53) & 0x3FF             // top 10 bits after the implicit leading 1
    rem      = (m >> 31) & 0x3F_FFFF
    frac_part = LOG2_TABLE[idx_bits]
              + (((i128(LOG2_TABLE[idx_bits+1]) - i128(LOG2_TABLE[idx_bits])) * i128(rem)) >> 22)
    return add(int_part, frac_part)

exp(a) = exp2(mul(a, LOG2_E))    // LOG2_E = 1/ln(2), raw 0x0000_0001_7154_7653
ln(a)  = mul(log2(a), LN_2)      // LN_2  = ln(2),    raw 0x0000_0000_B172_17F8
pow(a, b) = exp2(mul(log2(a), b))   // defined for a > 0 only; returns ZERO for a <= 0
```

---

## 3. Vector and quaternion types

`Vec2 { x, y }`, `Vec3 { x, y, z }`, `Quat { x, y, z, w }` — all components `Fx`.

All operations are componentwise applications of the scalar operations above, so their determinism
follows from section 1. The composite operations are defined in terms of them, and **evaluation
order is normative** because fixed-point addition is not associative under saturation:

```
dot(a, b)       = add(add(mul(a.x,b.x), mul(a.y,b.y)), mul(a.z,b.z))   // strictly left to right
cross(a, b)     = Vec3(sub(mul(a.y,b.z), mul(a.z,b.y)),
                       sub(mul(a.z,b.x), mul(a.x,b.z)),
                       sub(mul(a.x,b.y), mul(a.y,b.x)))
length_sq(a)    = dot(a, a)
length(a)       = sqrt(length_sq(a))
normalize(a)    = length(a) == 0 ? ZERO_VECTOR : scale(a, div(ONE, length(a)))
distance(a, b)  = length(sub(a, b))
lerp(a, b, t)   = add(a, scale(sub(b, a), t))          // not clamped; t may exceed [0,1]
```

`normalize` of a zero vector returns the zero vector rather than dividing by zero, making it total.

Quaternion multiplication uses the Hamilton convention, evaluated in the order written:

```
mul(p, q) = Quat(
    add(add(add(mul(p.w,q.x), mul(p.x,q.w)), mul(p.y,q.z)), neg(mul(p.z,q.y))),
    add(add(add(mul(p.w,q.y), neg(mul(p.x,q.z))), mul(p.y,q.w)), mul(p.z,q.x)),
    add(add(add(mul(p.w,q.z), mul(p.x,q.y)), neg(mul(p.y,q.x))), mul(p.z,q.w)),
    sub(sub(sub(mul(p.w,q.w), mul(p.x,q.x)), mul(p.y,q.y)), mul(p.z,q.z)))
```

---

## 4. Conformance

Every implementation MUST pass the vectors in
[`conformance/vectors/fixed-point.json`](../../conformance/vectors/) — see
[conformance.md](conformance.md). The vectors cover:

- exhaustive edge cases: `MIN`, `MAX`, zero, `±ONE`, values adjacent to saturation
- signed truncation for `mul`, `div`, and every conversion, on both signs
- division by zero in all three sign cases
- transcendentals at table boundaries, at interpolation midpoints, and at negative and
  large-magnitude angles
- composite vector operations where intermediate saturation occurs

A conforming implementation produces byte-identical raw values for every vector. There is no
tolerance; a one-bit difference is a failure, because a one-bit difference is a desync.

---

## Appendix A: reference integer square root

```
isqrt_u128(n: u128) -> u64:
    if n == 0: return 0
    // largest power of four not exceeding n
    bit = 1u128 << (126 - (leading_zeros(n) & !1))
    x = 0u128
    rem = n
    while bit != 0:
        if rem >= x + bit:
            rem = rem - (x + bit)
            x = (x >> 1) + bit
        else:
            x = x >> 1
        bit = bit >> 2
    return x as u64
```

Exact: returns `floor(sqrt(n))` for every input, with no floating point and no iteration-count
dependence.
