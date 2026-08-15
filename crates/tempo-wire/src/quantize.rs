//! Quantization: mapping fixed-point values onto the smallest integer that carries the declared
//! precision.
//!
//! Implements `docs/spec/wire-protocol.md` §5.1 and §5.2. Every computation here is exact integer
//! arithmetic on raw `Fx` values in 128 bits. Two reasons, both load-bearing:
//!
//! - **No floats**, so the result is identical in every language.
//! - **No `Fx` division**, which would saturate for wide ranges and would round twice.
//!
//! Quantization is lossy *by design*: `dequantize(quantize(v))` is the nearest representable step,
//! not `v`. Values outside the declared range **saturate silently** rather than erroring, because a
//! range mistake must not disconnect a player mid-match.

use tempo_fixed::{Fx, Quat};

/// Clamps a 128-bit intermediate into the `Fx` raw range.
#[inline]
fn sat_raw(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// Largest quantized index for the given range and step.
#[inline]
pub fn max_index(min: Fx, max: Fx, step: Fx) -> u64 {
    debug_assert!(step.raw() > 0 && max.raw() > min.raw());
    let span = max.raw() as i128 - min.raw() as i128;
    (span / step.raw() as i128).max(0) as u64
}

/// Quantizes `v` into an integer index, rounding half away from zero.
///
/// The value is clamped into `[min, max]` first, so the result always fits the field's declared
/// bit width.
pub fn quantize(v: Fx, min: Fx, max: Fx, step: Fx) -> u64 {
    debug_assert!(step.raw() > 0 && max.raw() > min.raw());
    let clamped = v.clamp(min, max);
    let num = clamped.raw() as i128 - min.raw() as i128; // never negative after clamping
    let den = step.raw() as i128;
    let q = (num + den / 2) / den;
    let hi = max_index(min, max, step) as i128;
    q.clamp(0, hi) as u64
}

/// Reconstructs a value from a quantized index.
#[inline]
pub fn dequantize(q: u64, min: Fx, step: Fx) -> Fx {
    Fx::from_raw(sat_raw(min.raw() as i128 + q as i128 * step.raw() as i128))
}

/// Quantizes a value known to lie in `[-1/√2, 1/√2]` into `bits` bits.
///
/// Used for the three retained components of a smallest-three quaternion.
fn quantize_unit_component(v: Fx, bits: u32) -> u64 {
    let range = (1u64 << bits) - 1;
    let f = Fx::FRAC_1_SQRT_2.raw() as i128;
    let clamped = v.clamp(Fx::FRAC_1_SQRT_2.neg(), Fx::FRAC_1_SQRT_2);
    let num = (clamped.raw() as i128 + f) * range as i128;
    let den = 2 * f;
    let q = (num + den / 2) / den;
    q.clamp(0, range as i128) as u64
}

/// Inverse of [`quantize_unit_component`].
fn dequantize_unit_component(q: u64, bits: u32) -> Fx {
    let range = (1u64 << bits) - 1;
    let f = Fx::FRAC_1_SQRT_2.raw() as i128;
    let den = range as i128;
    let num = q as i128 * 2 * f;
    Fx::from_raw(sat_raw((num + den / 2) / den - f))
}

/// A quaternion compressed with the smallest-three scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmallestThree {
    /// Index of the omitted (largest-magnitude) component, 0..=3 for x, y, z, w.
    pub largest: u8,
    /// The three retained components, quantized.
    pub components: [u64; 3],
}

/// Compresses a unit quaternion by dropping its largest-magnitude component.
///
/// A unit quaternion has three degrees of freedom, so one component is redundant. Dropping the
/// largest keeps the reconstruction well conditioned, and negating so that it is positive means the
/// sign need not be transmitted (`q` and `-q` are the same rotation).
pub fn compress_quat(q: Quat, bits: u32) -> SmallestThree {
    let comps = [q.x, q.y, q.z, q.w];

    let mut largest = 0usize;
    let mut best = comps[0].abs();
    for (i, c) in comps.iter().enumerate().skip(1) {
        if c.abs().raw() > best.raw() {
            best = c.abs();
            largest = i;
        }
    }

    let negate = comps[largest].raw() < 0;
    let mut components = [0u64; 3];
    let mut slot = 0;
    for (i, c) in comps.iter().enumerate() {
        if i == largest {
            continue;
        }
        let v = if negate { c.neg() } else { *c };
        components[slot] = quantize_unit_component(v, bits);
        slot += 1;
    }

    SmallestThree {
        largest: largest as u8,
        components,
    }
}

/// Reconstructs a unit quaternion from its smallest-three encoding.
pub fn decompress_quat(s: SmallestThree, bits: u32) -> Quat {
    let a = dequantize_unit_component(s.components[0], bits);
    let b = dequantize_unit_component(s.components[1], bits);
    let c = dequantize_unit_component(s.components[2], bits);

    // The clamp at zero is required, not defensive: quantization can push the sum of squares
    // marginally above one, and taking the square root of a negative value is exactly the kind of
    // edge case that desyncs one platform and not another.
    let sum = a.mul(a).add(b.mul(b)).add(c.mul(c));
    let largest = Fx::ONE.sub(sum).max(Fx::ZERO).sqrt();

    match s.largest {
        0 => Quat::new(largest, a, b, c),
        1 => Quat::new(a, largest, b, c),
        2 => Quat::new(a, b, largest, c),
        _ => Quat::new(a, b, c, largest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_fixed::Vec3;

    const MILLI: Fx = Fx::from_raw(0x0041_8937); // 0.001

    #[test]
    fn quantization_round_trips_within_one_step() {
        let (min, max) = (Fx::from_int(-1000), Fx::from_int(1000));
        for v in [-1000, -37, 0, 1, 999, 1000] {
            let v = Fx::from_int(v);
            let q = quantize(v, min, max, MILLI);
            let back = dequantize(q, min, MILLI);
            let err = back.sub(v).abs();
            assert!(err.raw() <= MILLI.raw(), "{v:?} -> {q} -> {back:?}");
        }
    }

    #[test]
    fn endpoints_map_to_the_extreme_indices() {
        let (min, max) = (Fx::from_int(-1000), Fx::from_int(1000));
        assert_eq!(quantize(min, min, max, MILLI), 0);
        assert_eq!(quantize(max, min, max, MILLI), max_index(min, max, MILLI));
    }

    #[test]
    fn out_of_range_values_saturate_silently() {
        // Specified behaviour, not an accident: a range mistake must not disconnect a player.
        let (min, max) = (Fx::ZERO, Fx::ONE);
        assert_eq!(quantize(Fx::from_int(-50), min, max, MILLI), 0);
        assert_eq!(
            quantize(Fx::from_int(50), min, max, MILLI),
            max_index(min, max, MILLI)
        );
    }

    #[test]
    fn half_steps_round_away_from_zero() {
        let (min, max, step) = (Fx::ZERO, Fx::from_int(10), Fx::ONE);
        assert_eq!(
            quantize(Fx::HALF, min, max, step),
            1,
            "0.5 rounds up to index 1"
        );
        assert_eq!(
            quantize(Fx::from_ratio(3, 2), min, max, step),
            2,
            "1.5 rounds up to 2"
        );
    }

    #[test]
    fn quantization_index_fits_the_declared_bit_width() {
        let (min, max) = (Fx::from_int(-1000), Fx::from_int(1000));
        let bits = crate::schema::quantized_bits(min, max, MILLI);
        let hi = quantize(max, min, max, MILLI);
        assert!(
            hi < (1u64 << bits),
            "index {hi} does not fit in {bits} bits"
        );
    }

    #[test]
    fn smallest_three_round_trips_identity() {
        let s = compress_quat(Quat::IDENTITY, 10);
        let back = decompress_quat(s, 10);
        assert_eq!(s.largest, 3, "w is the largest component of the identity");
        assert!((back.w.to_f64_lossy() - 1.0).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn smallest_three_round_trips_arbitrary_rotations() {
        for (axis, turns) in [
            (Vec3::X, 1),
            (Vec3::Y, 3),
            (Vec3::Z, 5),
            (Vec3::new(Fx::ONE, Fx::ONE, Fx::ONE), 7),
        ] {
            for i in 0..8 {
                let angle = Fx::TAU.mul(Fx::from_ratio(i * turns, 16));
                let q = Quat::from_axis_angle(axis, angle).normalize();
                let back = decompress_quat(compress_quat(q, 10), 10);

                // q and -q are the same rotation, so compare via the dot product's magnitude.
                let dot =
                    q.x.mul(back.x)
                        .add(q.y.mul(back.y))
                        .add(q.z.mul(back.z))
                        .add(q.w.mul(back.w));
                assert!(
                    dot.abs().to_f64_lossy() > 0.999,
                    "rotation about {axis:?} by {angle:?}: {q:?} -> {back:?} (dot {dot:?})"
                );
            }
        }
    }

    #[test]
    fn smallest_three_handles_a_negative_largest_component() {
        // Negating the whole quaternion when the largest component is negative is what lets the
        // sign go untransmitted. This is the case that breaks if that step is missed.
        let q = Quat::new(Fx::ZERO, Fx::ZERO, Fx::ZERO, Fx::NEG_ONE);
        let s = compress_quat(q, 10);
        let back = decompress_quat(s, 10);
        assert!((back.w.to_f64_lossy() - 1.0).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn decompression_clamps_before_the_square_root() {
        // Craft an encoding whose components sum to slightly more than one. Without the clamp this
        // is a square root of a negative number.
        let bits = 10;
        let range = (1u64 << bits) - 1;
        let s = SmallestThree {
            largest: 3,
            components: [range, range, range],
        };
        let back = decompress_quat(s, bits);
        assert_eq!(
            back.w,
            Fx::ZERO,
            "must clamp to zero rather than produce garbage"
        );
    }

    #[test]
    fn reconstructed_quaternions_are_near_unit_length() {
        for i in 0..16 {
            let q = Quat::from_axis_angle(Vec3::Y, Fx::TAU.mul(Fx::from_ratio(i, 16))).normalize();
            let back = decompress_quat(compress_quat(q, 12), 12);
            let len = back.length().to_f64_lossy();
            assert!((len - 1.0).abs() < 1e-3, "length {len} for {back:?}");
        }
    }
}
