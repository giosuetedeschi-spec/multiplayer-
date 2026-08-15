//! The `tempo` wire codec: bit packing, quantization, and schema canonicalisation.
//!
//! This crate implements the parts of `docs/spec/wire-protocol.md` and
//! `docs/spec/schema-and-hashing.md` that turn game state into bytes. It knows nothing about
//! sockets, ticks, or entities — it is the encoding layer alone.
//!
//! # Why a custom format
//!
//! Bandwidth is the binding constraint in realtime multiplayer, and general-purpose serialisation
//! formats optimise for goals we do not have. Protobuf and MessagePack are self-describing, paying
//! field tags and lengths on every field; FlatBuffers optimises random access to a received buffer,
//! paying alignment and vtables. All of them are byte-aligned, so a boolean costs 8 bits and an
//! angle needing 9 bits of precision costs 32.
//!
//! Here the schema is negotiated up front, so the stream can be positional and bit-granular. See
//! [ADR-0009](../../../docs/adr/0009-custom-bitpacked-wire-format.md).
//!
//! # The safety property that pays for it
//!
//! Because nothing is self-describing, a schema mismatch does not produce a clean error — it
//! misaligns the whole stream and corrupts state. That is why [`Schema::schema_id`] exists and is
//! exchanged during the handshake, and why [`Schema::diff`] must produce a readable difference
//! rather than a generic failure. The two decisions are load-bearing for each other.
//!
//! # Example
//!
//! ```
//! use tempo_wire::{BitReader, BitWriter, ComponentDesc, FieldDesc, FieldType, Schema, Value};
//! use tempo_wire::{decode_field, encode_field};
//! use tempo_fixed::Fx;
//!
//! let position = FieldDesc::new("position", FieldType::Fx)
//!     .with_quantize(Fx::from_raw(0x418937), Fx::from_int(-1000), Fx::from_int(1000));
//!
//! let mut schema = Schema::new();
//! schema.register(ComponentDesc::new("Player", vec![position.clone()]))?;
//!
//! let mut w = BitWriter::new();
//! encode_field(&mut w, &position, &Value::Fx(Fx::from_int(42)))?;
//! assert_eq!(w.bit_len(), 21); // not 64
//!
//! let bytes = w.finish();
//! let mut r = BitReader::new(&bytes);
//! let back = decode_field(&mut r, &position)?;
//! # Ok::<(), tempo_wire::WireError>(())
//! ```

#![forbid(unsafe_code)]

pub mod bits;
pub mod quantize;
pub mod schema;
pub mod value;

pub use bits::{zigzag_decode, zigzag_encode, BitReader, BitWriter};
pub use quantize::{
    compress_quat, decompress_quat, dequantize, quantize as quantize_value, SmallestThree,
};
pub use schema::{
    is_identifier, quantized_bits, render_fx, ComponentDesc, FieldDesc, FieldType, Schema,
    SchemaId, SCHEMA_FORMAT_VERSION,
};
pub use value::{decode_field, encode_field, enum_bits, Value};

use core::fmt;

/// Errors produced by the wire codec.
///
/// Every variant is a condition a well-behaved peer never causes. They exist because a hostile or
/// mismatched peer can, and the decoder must fail rather than misbehave.
#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    /// The reader ran out of bits before the value was complete.
    UnexpectedEnd,
    /// A varint carried more groups than any 64-bit value needs.
    MalformedVarint,
    /// A value's type did not match its field declaration.
    TypeMismatch {
        /// The field being encoded.
        field: String,
        /// The type the schema declares.
        expected: FieldType,
        /// The type the value actually had.
        found: FieldType,
    },
    /// A value fell outside what its declaration permits.
    ValueOutOfRange {
        /// The field being encoded or decoded.
        field: String,
        /// What specifically was out of range.
        detail: String,
    },
    /// A schema declaration was rejected during validation.
    InvalidSchema(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::UnexpectedEnd => write!(f, "unexpected end of packet"),
            WireError::MalformedVarint => write!(f, "malformed varint"),
            WireError::TypeMismatch {
                field,
                expected,
                found,
            } => write!(
                f,
                "field {field}: schema declares {}, value is {}",
                expected.canonical_name(),
                found.canonical_name()
            ),
            WireError::ValueOutOfRange { field, detail } => {
                write!(f, "field {field}: {detail}")
            }
            WireError::InvalidSchema(detail) => write!(f, "invalid schema: {detail}"),
        }
    }
}

impl core::error::Error for WireError {}
