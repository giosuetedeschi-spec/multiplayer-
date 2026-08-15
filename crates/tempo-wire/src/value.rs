//! Encoding and decoding of individual field values.
//!
//! Implements `docs/spec/wire-protocol.md` §5. The encoding of a field is fully determined by its
//! [`FieldDesc`], which both peers agree on via the negotiated schema ID — so nothing self-
//! describing is written, not even a type tag.
//!
//! [`Value`] is the dynamic representation used by the codec and by tests. The world arena encodes
//! straight from its columnar storage on the hot path; this module defines the semantics that the
//! faster path must reproduce.

use crate::bits::{BitReader, BitWriter};
use crate::quantize::{compress_quat, decompress_quat, dequantize, quantize, SmallestThree};
use crate::schema::{FieldDesc, FieldType};
use crate::WireError;
use tempo_fixed::{Fx, Quat, Vec2, Vec3};

/// A dynamically typed field value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// An unsigned integer.
    Uint(u64),
    /// A signed integer.
    Int(i64),
    /// A fixed-point scalar.
    Fx(Fx),
    /// A two-dimensional vector.
    Vec2(Vec2),
    /// A three-dimensional vector.
    Vec3(Vec3),
    /// A rotation.
    Quat(Quat),
    /// An enum discriminant.
    Enum(u32),
    /// A UTF-8 string.
    Str(String),
    /// An opaque byte string.
    Bytes(Vec<u8>),
}

impl Value {
    /// The field type this value can be encoded as.
    pub fn field_type(&self) -> FieldType {
        match self {
            Value::Bool(_) => FieldType::Bool,
            Value::Uint(_) => FieldType::Uint,
            Value::Int(_) => FieldType::Int,
            Value::Fx(_) => FieldType::Fx,
            Value::Vec2(_) => FieldType::Vec2,
            Value::Vec3(_) => FieldType::Vec3,
            Value::Quat(_) => FieldType::Quat,
            Value::Enum(_) => FieldType::Enum,
            Value::Str(_) => FieldType::Str,
            Value::Bytes(_) => FieldType::Bytes,
        }
    }
}

/// Number of bits needed to distinguish `variants` alternatives.
#[inline]
pub fn enum_bits(variants: u32) -> u32 {
    if variants <= 1 {
        0
    } else {
        32 - (variants - 1).leading_zeros()
    }
}

/// Applies a field's lossy transform without touching the wire — what the receiver will hold
/// after decoding this value.
///
/// Delta compression **must** diff against this, not against the sender's raw state. Quantization
/// is lossy, so a peer that compares raw values sees every quantized field as permanently changed:
/// it sends an update, the receiver stores the quantized value, the sender compares its raw value
/// against its own raw baseline and finds a difference again next tick. The field is re-sent every
/// tick forever and delta compression is defeated entirely for exactly the fields it matters most
/// for.
pub fn lossy_round_trip(desc: &FieldDesc, v: &Value) -> Value {
    let fx = |x: Fx| match (desc.quantize, desc.min, desc.max) {
        (Some(step), Some(min), Some(max)) => dequantize(quantize(x, min, max, step), min, step),
        _ => x,
    };
    match v {
        Value::Fx(x) => Value::Fx(fx(*x)),
        Value::Vec2(x) => Value::Vec2(Vec2::new(fx(x.x), fx(x.y))),
        Value::Vec3(x) => Value::Vec3(Vec3::new(fx(x.x), fx(x.y), fx(x.z))),
        Value::Quat(x) => match desc.quantize_bits {
            Some(k) => Value::Quat(decompress_quat(compress_quat(*x, k), k)),
            None => Value::Quat(*x),
        },
        other => other.clone(),
    }
}

/// Encodes a single `Fx` component according to a field's quantization rule.
fn encode_fx(w: &mut BitWriter, desc: &FieldDesc, v: Fx) {
    match (desc.quantize, desc.min, desc.max) {
        (Some(step), Some(min), Some(max)) => {
            let bits = crate::schema::quantized_bits(min, max, step);
            w.write_bits(quantize(v, min, max, step), bits);
        }
        // Unquantized values go out raw. This is 64 bits, which is expensive — the guide is
        // emphatic that replicated scalars should declare a range.
        _ => w.write_bits(v.raw() as u64, 64),
    }
}

/// Decodes a single `Fx` component according to a field's quantization rule.
fn decode_fx(r: &mut BitReader, desc: &FieldDesc) -> Result<Fx, WireError> {
    match (desc.quantize, desc.min, desc.max) {
        (Some(step), Some(min), Some(max)) => {
            let bits = crate::schema::quantized_bits(min, max, step);
            Ok(dequantize(r.read_bits(bits)?, min, step))
        }
        _ => Ok(Fx::from_raw(r.read_bits(64)? as i64)),
    }
}

/// Encodes `value` per `desc`.
///
/// Returns [`WireError::TypeMismatch`] if the value's type does not match the declaration.
pub fn encode_field(w: &mut BitWriter, desc: &FieldDesc, value: &Value) -> Result<(), WireError> {
    if value.field_type() != desc.ty {
        return Err(WireError::TypeMismatch {
            field: desc.name.clone(),
            expected: desc.ty,
            found: value.field_type(),
        });
    }

    match value {
        Value::Bool(v) => w.write_bool(*v),
        Value::Uint(v) => match desc.bits {
            Some(bits) => w.write_bits(*v, bits),
            None => w.write_varuint(*v),
        },
        Value::Int(v) => match desc.bits {
            // Two's complement in the low `bits` bits; the decoder sign-extends.
            Some(bits) => w.write_bits(*v as u64, bits),
            None => w.write_varint(*v),
        },
        Value::Fx(v) => encode_fx(w, desc, *v),
        Value::Vec2(v) => {
            encode_fx(w, desc, v.x);
            encode_fx(w, desc, v.y);
        }
        Value::Vec3(v) => {
            encode_fx(w, desc, v.x);
            encode_fx(w, desc, v.y);
            encode_fx(w, desc, v.z);
        }
        Value::Quat(v) => match desc.quantize_bits {
            Some(k) => {
                let s = compress_quat(*v, k);
                w.write_bits(s.largest as u64, 2);
                for c in s.components {
                    w.write_bits(c, k);
                }
            }
            None => {
                for c in [v.x, v.y, v.z, v.w] {
                    w.write_bits(c.raw() as u64, 64);
                }
            }
        },
        Value::Enum(v) => {
            let variants = desc.variants.ok_or_else(|| {
                WireError::InvalidSchema(format!("enum field {} has no variants", desc.name))
            })?;
            if *v >= variants {
                return Err(WireError::ValueOutOfRange {
                    field: desc.name.clone(),
                    detail: format!("enum discriminant {v} exceeds {variants} variants"),
                });
            }
            w.write_bits(*v as u64, enum_bits(variants));
        }
        Value::Str(s) => {
            check_len(desc, s.len())?;
            w.write_varuint(s.len() as u64);
            w.write_bytes(s.as_bytes());
        }
        Value::Bytes(b) => {
            check_len(desc, b.len())?;
            w.write_varuint(b.len() as u64);
            w.write_bytes(b);
        }
    }
    Ok(())
}

/// Decodes a value per `desc`.
pub fn decode_field(r: &mut BitReader, desc: &FieldDesc) -> Result<Value, WireError> {
    let v = match desc.ty {
        FieldType::Bool => Value::Bool(r.read_bool()?),
        FieldType::Uint => Value::Uint(match desc.bits {
            Some(bits) => r.read_bits(bits)?,
            None => r.read_varuint()?,
        }),
        FieldType::Int => Value::Int(match desc.bits {
            Some(bits) => sign_extend(r.read_bits(bits)?, bits),
            None => r.read_varint()?,
        }),
        FieldType::Fx => Value::Fx(decode_fx(r, desc)?),
        FieldType::Vec2 => Value::Vec2(Vec2::new(decode_fx(r, desc)?, decode_fx(r, desc)?)),
        FieldType::Vec3 => Value::Vec3(Vec3::new(
            decode_fx(r, desc)?,
            decode_fx(r, desc)?,
            decode_fx(r, desc)?,
        )),
        FieldType::Quat => Value::Quat(match desc.quantize_bits {
            Some(k) => {
                let largest = r.read_bits(2)? as u8;
                let components = [r.read_bits(k)?, r.read_bits(k)?, r.read_bits(k)?];
                decompress_quat(
                    SmallestThree {
                        largest,
                        components,
                    },
                    k,
                )
            }
            None => Quat::new(
                Fx::from_raw(r.read_bits(64)? as i64),
                Fx::from_raw(r.read_bits(64)? as i64),
                Fx::from_raw(r.read_bits(64)? as i64),
                Fx::from_raw(r.read_bits(64)? as i64),
            ),
        }),
        FieldType::Enum => {
            let variants = desc.variants.ok_or_else(|| {
                WireError::InvalidSchema(format!("enum field {} has no variants", desc.name))
            })?;
            let raw = r.read_bits(enum_bits(variants))? as u32;
            if raw >= variants {
                // A hostile or mismatched peer can produce an in-width but out-of-range
                // discriminant. Rejecting keeps it from being interpreted as a valid variant.
                return Err(WireError::ValueOutOfRange {
                    field: desc.name.clone(),
                    detail: format!("decoded discriminant {raw} exceeds {variants} variants"),
                });
            }
            Value::Enum(raw)
        }
        FieldType::Str => {
            let len = read_checked_len(r, desc)?;
            let bytes = r.read_bytes(len)?;
            Value::Str(
                core::str::from_utf8(bytes)
                    .map_err(|_| WireError::ValueOutOfRange {
                        field: desc.name.clone(),
                        detail: "string field is not valid UTF-8".into(),
                    })?
                    .to_owned(),
            )
        }
        FieldType::Bytes => {
            let len = read_checked_len(r, desc)?;
            Value::Bytes(r.read_bytes(len)?.to_vec())
        }
    };
    Ok(v)
}

/// Sign-extends a two's-complement value read from `bits` bits.
#[inline]
fn sign_extend(raw: u64, bits: u32) -> i64 {
    if bits >= 64 {
        return raw as i64;
    }
    let shift = 64 - bits;
    ((raw << shift) as i64) >> shift
}

fn check_len(desc: &FieldDesc, len: usize) -> Result<(), WireError> {
    if let Some(max) = desc.max_len {
        if len > max as usize {
            return Err(WireError::ValueOutOfRange {
                field: desc.name.clone(),
                detail: format!("length {len} exceeds max_len {max}"),
            });
        }
    }
    Ok(())
}

/// Reads a length prefix and validates it **before** any allocation.
///
/// Validating first is the point: an unchecked length from a hostile peer is a memory-exhaustion
/// vector, and `Vec::with_capacity` on an attacker-chosen number is the classic way to hit it.
fn read_checked_len(r: &mut BitReader, desc: &FieldDesc) -> Result<usize, WireError> {
    let len = r.read_varuint()?;
    if let Some(max) = desc.max_len {
        if len > max as u64 {
            return Err(WireError::ValueOutOfRange {
                field: desc.name.clone(),
                detail: format!("declared length {len} exceeds max_len {max}"),
            });
        }
    }
    let len = len as usize;
    if len > r.bits_remaining() / 8 {
        return Err(WireError::UnexpectedEnd);
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(desc: &FieldDesc, v: &Value) -> Value {
        let mut w = BitWriter::new();
        encode_field(&mut w, desc, v).expect("encode");
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        decode_field(&mut r, desc).expect("decode")
    }

    #[test]
    fn bool_costs_one_bit() {
        let d = FieldDesc::new("f", FieldType::Bool);
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Bool(true)).unwrap();
        assert_eq!(w.bit_len(), 1, "a boolean must not be rounded up to a byte");
        assert_eq!(round_trip(&d, &Value::Bool(true)), Value::Bool(true));
        assert_eq!(round_trip(&d, &Value::Bool(false)), Value::Bool(false));
    }

    #[test]
    fn fixed_width_integers_use_exactly_that_width() {
        let d = FieldDesc::new("f", FieldType::Uint).with_bits(10);
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Uint(1023)).unwrap();
        assert_eq!(w.bit_len(), 10);
        assert_eq!(round_trip(&d, &Value::Uint(1023)), Value::Uint(1023));
    }

    #[test]
    fn signed_fixed_width_integers_sign_extend() {
        let d = FieldDesc::new("f", FieldType::Int).with_bits(8);
        for v in [-128i64, -1, 0, 1, 127] {
            assert_eq!(round_trip(&d, &Value::Int(v)), Value::Int(v), "value {v}");
        }
    }

    #[test]
    fn varint_fields_round_trip() {
        let u = FieldDesc::new("u", FieldType::Uint);
        let i = FieldDesc::new("i", FieldType::Int);
        assert_eq!(
            round_trip(&u, &Value::Uint(u64::MAX)),
            Value::Uint(u64::MAX)
        );
        assert_eq!(round_trip(&i, &Value::Int(i64::MIN)), Value::Int(i64::MIN));
    }

    #[test]
    fn unquantized_fx_costs_sixty_four_bits() {
        let d = FieldDesc::new("f", FieldType::Fx);
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Fx(Fx::PI)).unwrap();
        assert_eq!(w.bit_len(), 64);
        assert_eq!(round_trip(&d, &Value::Fx(Fx::PI)), Value::Fx(Fx::PI));
        assert_eq!(round_trip(&d, &Value::Fx(Fx::MIN)), Value::Fx(Fx::MIN));
    }

    #[test]
    fn quantized_fx_is_far_cheaper() {
        // The headline claim of ADR-0009: declaring a range turns 64 bits into 21.
        let d = FieldDesc::new("f", FieldType::Fx).with_quantize(
            Fx::from_raw(0x0041_8937),
            Fx::from_int(-1000),
            Fx::from_int(1000),
        );
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Fx(Fx::from_int(42))).unwrap();
        assert_eq!(w.bit_len(), 21);

        let back = round_trip(&d, &Value::Fx(Fx::from_int(42)));
        match back {
            Value::Fx(v) => assert!(v.sub(Fx::from_int(42)).abs().raw() <= 0x0041_8937),
            other => panic!("expected Fx, got {other:?}"),
        }
    }

    #[test]
    fn quantized_vec2_costs_two_components() {
        let d = FieldDesc::new("p", FieldType::Vec2).with_quantize(
            Fx::from_raw(0x0041_8937),
            Fx::from_int(-1000),
            Fx::from_int(1000),
        );
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Vec2(Vec2::from_ints(3, -4))).unwrap();
        assert_eq!(w.bit_len(), 42, "21 bits per component");
    }

    #[test]
    fn enum_uses_minimum_bits_and_rejects_out_of_range() {
        let d = FieldDesc::new("e", FieldType::Enum).with_variants(5);
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Enum(4)).unwrap();
        assert_eq!(w.bit_len(), 3, "five variants need three bits");
        assert_eq!(round_trip(&d, &Value::Enum(4)), Value::Enum(4));

        assert!(matches!(
            encode_field(&mut BitWriter::new(), &d, &Value::Enum(5)),
            Err(WireError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn enum_bit_widths() {
        assert_eq!(enum_bits(1), 0);
        assert_eq!(enum_bits(2), 1);
        assert_eq!(enum_bits(3), 2);
        assert_eq!(enum_bits(4), 2);
        assert_eq!(enum_bits(5), 3);
        assert_eq!(enum_bits(256), 8);
        assert_eq!(enum_bits(257), 9);
    }

    #[test]
    fn decoding_rejects_an_out_of_range_discriminant() {
        // Three bits can hold 5, 6 and 7 while only five variants are declared. A mismatched or
        // hostile peer must not have those interpreted as valid.
        let d = FieldDesc::new("e", FieldType::Enum).with_variants(5);
        let mut w = BitWriter::new();
        w.write_bits(7, 3);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(matches!(
            decode_field(&mut r, &d),
            Err(WireError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn strings_and_bytes_round_trip() {
        let s = FieldDesc::new("s", FieldType::Str);
        let b = FieldDesc::new("b", FieldType::Bytes);
        assert_eq!(
            round_trip(&s, &Value::Str("héllo".into())),
            Value::Str("héllo".into())
        );
        assert_eq!(
            round_trip(&s, &Value::Str(String::new())),
            Value::Str(String::new())
        );
        assert_eq!(
            round_trip(&b, &Value::Bytes(vec![0, 255, 7])),
            Value::Bytes(vec![0, 255, 7])
        );
    }

    #[test]
    fn max_len_is_enforced_on_both_sides() {
        let d = FieldDesc::new("s", FieldType::Str).with_max_len(4);
        assert!(matches!(
            encode_field(&mut BitWriter::new(), &d, &Value::Str("toolong".into())),
            Err(WireError::ValueOutOfRange { .. })
        ));

        // The decoder must reject an oversized declared length before allocating for it.
        let mut w = BitWriter::new();
        w.write_varuint(1_000_000);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(matches!(
            decode_field(&mut r, &d),
            Err(WireError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn an_absurd_length_without_max_len_still_cannot_allocate() {
        let d = FieldDesc::new("b", FieldType::Bytes);
        let mut w = BitWriter::new();
        w.write_varuint(u64::MAX / 2);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(matches!(
            decode_field(&mut r, &d),
            Err(WireError::UnexpectedEnd)
        ));
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let d = FieldDesc::new("s", FieldType::Str);
        let mut w = BitWriter::new();
        w.write_varuint(2);
        w.write_bytes(&[0xFF, 0xFE]);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(matches!(
            decode_field(&mut r, &d),
            Err(WireError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn quantized_quaternions_cost_two_plus_three_k_bits() {
        let d = FieldDesc::new("r", FieldType::Quat).with_quantize_bits(10);
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Quat(Quat::IDENTITY)).unwrap();
        assert_eq!(w.bit_len(), 32, "2 + 3*10 bits, against 256 raw");
    }

    #[test]
    fn a_single_step_field_occupies_no_bits() {
        // Degenerate but reachable: a step coarser than the range leaves one representable value,
        // so the field costs nothing and always decodes to `min`. The bit primitives must handle
        // a zero-width read and write rather than treating it as an error.
        let d = FieldDesc::new("f", FieldType::Fx).with_quantize(Fx::ZERO, Fx::ZERO, Fx::ONE);
        let d = FieldDesc {
            quantize: Some(Fx::from_int(2)),
            ..d
        };
        let mut w = BitWriter::new();
        encode_field(&mut w, &d, &Value::Fx(Fx::HALF)).unwrap();
        assert_eq!(w.bit_len(), 0);

        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_field(&mut r, &d).unwrap(), Value::Fx(Fx::ZERO));
    }

    #[test]
    fn type_mismatches_are_rejected() {
        let d = FieldDesc::new("f", FieldType::Bool);
        assert!(matches!(
            encode_field(&mut BitWriter::new(), &d, &Value::Uint(1)),
            Err(WireError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn several_fields_pack_without_padding_between_them() {
        // Adjacent small fields must share bytes; this is where bit packing earns its keep.
        let bool_f = FieldDesc::new("a", FieldType::Bool);
        let enum_f = FieldDesc::new("b", FieldType::Enum).with_variants(4);
        let mut w = BitWriter::new();
        encode_field(&mut w, &bool_f, &Value::Bool(true)).unwrap();
        encode_field(&mut w, &enum_f, &Value::Enum(2)).unwrap();
        encode_field(&mut w, &bool_f, &Value::Bool(false)).unwrap();
        assert_eq!(w.bit_len(), 4);
        assert_eq!(w.finish().len(), 1, "four fields in a single byte");
    }
}
