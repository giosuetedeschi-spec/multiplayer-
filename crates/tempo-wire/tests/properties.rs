//! Property tests for the wire codec.
//!
//! `docs/spec/conformance.md` §4 requires these alongside the fixed vectors. Fixed vectors catch
//! the edge cases somebody thought of; properties catch the ones nobody did. Every property here
//! is stated in that section.

use proptest::prelude::*;
use tempo_fixed::{Fx, Quat, Vec2, Vec3};
use tempo_wire::{
    decode_field, encode_field, quantize_value, quantized_bits, BitReader, BitWriter,
    ComponentDesc, FieldDesc, FieldType, Schema, Value,
};

/// A quantized position field, the shape most games actually replicate.
fn quantized_field(name: &str, ty: FieldType) -> FieldDesc {
    FieldDesc::new(name, ty).with_quantize(
        Fx::from_raw(0x0041_8937), // 0.001
        Fx::from_int(-1000),
        Fx::from_int(1000),
    )
}

fn round_trip(desc: &FieldDesc, v: &Value) -> Value {
    let mut w = BitWriter::new();
    encode_field(&mut w, desc, v).expect("encode");
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    decode_field(&mut r, desc).expect("decode")
}

proptest! {
    /// `bit_read(bit_write(x, n), n) == x` for any `x` fitting in `n` bits.
    #[test]
    fn bits_round_trip(value: u64, bits in 1u32..=64) {
        let masked = if bits == 64 { value } else { value & ((1u64 << bits) - 1) };
        let mut w = BitWriter::new();
        w.write_bits(masked, bits);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.read_bits(bits).unwrap(), masked);
    }

    /// A sequence of differently sized writes must decode back in order. This is where an
    /// off-by-one in the accumulator shows up, which a single write cannot reveal.
    #[test]
    fn interleaved_widths_round_trip(items in prop::collection::vec((any::<u64>(), 1u32..=40), 1..40)) {
        let masked: Vec<(u64, u32)> = items
            .iter()
            .map(|&(v, b)| (v & ((1u64 << b) - 1), b))
            .collect();

        let mut w = BitWriter::new();
        for &(v, b) in &masked {
            w.write_bits(v, b);
        }
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        for &(v, b) in &masked {
            prop_assert_eq!(r.read_bits(b).unwrap(), v);
        }
    }

    #[test]
    fn varuint_round_trips(v: u64) {
        let mut w = BitWriter::new();
        w.write_varuint(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.read_varuint().unwrap(), v);
    }

    #[test]
    fn varint_round_trips(v: i64) {
        let mut w = BitWriter::new();
        w.write_varint(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.read_varint().unwrap(), v);
    }

    /// `decode(encode(v)) == quantize(v)`: the round trip is lossy, but it lands on exactly the
    /// step quantization selected — never one step off.
    #[test]
    fn quantized_round_trip_is_the_selected_step(raw in -1_000i64..=1_000, milli in 0i64..1_000) {
        let field = quantized_field("p", FieldType::Fx);
        let v = Fx::from_int(raw as i32).add(Fx::from_ratio(milli as i32, 1000));

        let step = field.quantize.unwrap();
        let (min, max) = (field.min.unwrap(), field.max.unwrap());
        let expected = tempo_wire::dequantize(quantize_value(v, min, max, step), min, step);

        match round_trip(&field, &Value::Fx(v)) {
            Value::Fx(got) => prop_assert_eq!(got, expected),
            other => prop_assert!(false, "expected Fx, got {:?}", other),
        }
    }

    /// The quantization error never exceeds half a step, which is what "0.001 precision" has to
    /// mean for the declaration to be honest.
    #[test]
    fn quantization_error_is_bounded_by_half_a_step(raw in -999_000i64..=999_000) {
        let field = quantized_field("p", FieldType::Fx);
        let v = Fx::from_ratio(raw as i32, 1000);
        let step = field.quantize.unwrap();

        match round_trip(&field, &Value::Fx(v)) {
            Value::Fx(got) => {
                let err = got.sub(v).abs();
                prop_assert!(
                    err.raw() * 2 <= step.raw() + 1,
                    "error {:?} exceeds half of step {:?}", err, step
                );
            }
            other => prop_assert!(false, "expected Fx, got {:?}", other),
        }
    }

    /// A quantized value always fits the bit width its declaration implies. If this failed, the
    /// encoder would silently truncate and every subsequent field would be misaligned.
    #[test]
    fn quantized_indices_fit_their_declared_width(
        raw in -3_000i64..=3_000,
        step_milli in 1i64..500,
    ) {
        let step = Fx::from_ratio(step_milli as i32, 1000);
        let (min, max) = (Fx::from_int(-1000), Fx::from_int(1000));
        let bits = quantized_bits(min, max, step);
        let q = quantize_value(Fx::from_int(raw as i32), min, max, step);
        prop_assert!(bits >= 64 || q < (1u64 << bits), "index {} needs more than {} bits", q, bits);
    }

    /// Encoded output must be exactly as long as the declaration says, with no hidden padding.
    #[test]
    fn encoded_width_matches_the_declaration(raw in -1_000i64..=1_000) {
        let field = quantized_field("p", FieldType::Vec2);
        let per_component = quantized_bits(
            field.min.unwrap(), field.max.unwrap(), field.quantize.unwrap(),
        );
        let mut w = BitWriter::new();
        encode_field(
            &mut w, &field,
            &Value::Vec2(Vec2::from_ints(raw as i32, -(raw as i32))),
        ).unwrap();
        prop_assert_eq!(w.bit_len(), 2 * per_component as usize);
    }

    #[test]
    fn vec3_round_trips(x in -900i64..=900, y in -900i64..=900, z in -900i64..=900) {
        let field = quantized_field("p", FieldType::Vec3);
        let v = Vec3::from_ints(x as i32, y as i32, z as i32);
        match round_trip(&field, &Value::Vec3(v)) {
            Value::Vec3(got) => {
                let step = field.quantize.unwrap();
                for (a, b) in [(got.x, v.x), (got.y, v.y), (got.z, v.z)] {
                    prop_assert!(a.sub(b).abs().raw() <= step.raw());
                }
            }
            other => prop_assert!(false, "expected Vec3, got {:?}", other),
        }
    }

    /// Smallest-three encoding preserves the rotation, up to the sign ambiguity of `q` and `-q`.
    #[test]
    fn quaternion_compression_preserves_rotation(turns in 0i64..64, axis in 0usize..3) {
        let a = match axis {
            0 => Vec3::X,
            1 => Vec3::Y,
            _ => Vec3::Z,
        };
        let angle = Fx::TAU.mul(Fx::from_ratio(turns as i32, 64));
        let q = Quat::from_axis_angle(a, angle).normalize();
        let field = FieldDesc::new("r", FieldType::Quat).with_quantize_bits(10);

        match round_trip(&field, &Value::Quat(q)) {
            Value::Quat(back) => {
                let dot = q.x.mul(back.x).add(q.y.mul(back.y)).add(q.z.mul(back.z)).add(q.w.mul(back.w));
                prop_assert!(dot.abs().to_f64_lossy() > 0.998, "dot {:?} for {:?} -> {:?}", dot, q, back);
            }
            other => prop_assert!(false, "expected Quat, got {:?}", other),
        }
    }

    /// Field declaration order must not affect the canonical form or the schema ID. This is the
    /// property that lets six languages declare components however they like.
    #[test]
    fn canonical_form_is_independent_of_declaration_order(seed: u64) {
        let names = ["alpha", "Beta", "gamma", "_delta", "e1"];
        let mut fields: Vec<FieldDesc> = names
            .iter()
            .map(|n| FieldDesc::new(*n, FieldType::Bool))
            .collect();

        // Deterministic shuffle driven by the generated seed.
        let mut state = seed | 1;
        for i in (1..fields.len()).rev() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            fields.swap(i, (state >> 33) as usize % (i + 1));
        }

        let mut a = Schema::new();
        a.register(ComponentDesc::new("C", fields)).unwrap();

        let mut b = Schema::new();
        b.register(ComponentDesc::new(
            "C",
            names.iter().map(|n| FieldDesc::new(*n, FieldType::Bool)).collect(),
        )).unwrap();

        prop_assert_eq!(a.canonical_form(), b.canonical_form());
        prop_assert_eq!(a.schema_id(), b.schema_id());
    }

    /// Any change to an encoding-affecting parameter must change the schema ID, or two peers could
    /// agree on an ID while disagreeing on the bit layout — the exact failure the ID prevents.
    #[test]
    fn encoding_parameters_are_all_covered_by_the_id(bits in 1u32..=63) {
        let mk = |b: u32| {
            let mut s = Schema::new();
            s.register(ComponentDesc::new(
                "C",
                vec![FieldDesc::new("v", FieldType::Uint).with_bits(b)],
            )).unwrap();
            s.schema_id()
        };
        prop_assert_ne!(mk(bits), mk(bits + 1));
    }

    /// Decoding arbitrary bytes must never panic. A hostile peer controls this input entirely.
    #[test]
    fn decoding_arbitrary_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..64)) {
        let fields = [
            FieldDesc::new("a", FieldType::Bool),
            FieldDesc::new("b", FieldType::Uint),
            FieldDesc::new("c", FieldType::Int).with_bits(13),
            FieldDesc::new("d", FieldType::Str).with_max_len(32),
            FieldDesc::new("e", FieldType::Bytes),
            FieldDesc::new("f", FieldType::Enum).with_variants(7),
            FieldDesc::new("g", FieldType::Quat).with_quantize_bits(10),
            quantized_field("h", FieldType::Vec3),
        ];
        for f in &fields {
            let mut r = BitReader::new(&data);
            // The result may be Ok or Err; it must simply not panic or hang.
            let _ = decode_field(&mut r, f);
        }
    }
}
