//! Component and field descriptors, the canonical schema form, and the schema ID.
//!
//! Implements `docs/spec/schema-and-hashing.md`. Two peers can exchange bit-packed deltas only if
//! they agree on the schema exactly: the wire format is positional and not self-describing, so a
//! single disagreement about field order or width misaligns the whole stream and corrupts state
//! rather than producing an error.
//!
//! The canonical form is the single agreed interpretation of six languages' native declarations. A
//! Rust `#[derive(Replicate)]`, a Python `@replicated` class and a Go struct with tags describing
//! the same component must reduce to byte-identical text here.

use crate::WireError;
use core::fmt::Write as _;
use tempo_fixed::Fx;

/// Version of the canonical schema format, emitted as the first line.
pub const SCHEMA_FORMAT_VERSION: u32 = 1;

/// The type of a replicated field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    /// A single bit.
    Bool,
    /// Unsigned integer, `bits` wide if declared, otherwise varint-encoded.
    Uint,
    /// Signed integer, `bits` wide if declared, otherwise zig-zag varint-encoded.
    Int,
    /// A fixed-point scalar, quantized if `quantize` is declared.
    Fx,
    /// A two-dimensional vector, each component encoded per the field's `Fx` rule.
    Vec2,
    /// A three-dimensional vector.
    Vec3,
    /// A rotation, smallest-three encoded when `quantize_bits` is declared.
    Quat,
    /// An enumeration with a declared variant count.
    Enum,
    /// A UTF-8 string.
    Str,
    /// An opaque byte string.
    Bytes,
}

impl FieldType {
    /// The name used in the canonical form.
    pub const fn canonical_name(self) -> &'static str {
        match self {
            FieldType::Bool => "bool",
            FieldType::Uint => "uint",
            FieldType::Int => "int",
            FieldType::Fx => "fx",
            FieldType::Vec2 => "vec2",
            FieldType::Vec3 => "vec3",
            FieldType::Quat => "quat",
            FieldType::Enum => "enum",
            FieldType::Str => "string",
            FieldType::Bytes => "bytes",
        }
    }
}

/// A replicated field's declaration.
///
/// Only parameters that affect encoding appear in the canonical form. [`FieldDesc::base_priority`]
/// deliberately does not, so tuning replication priority never breaks wire compatibility.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDesc {
    /// Field name. Must match `[A-Za-z_][A-Za-z0-9_]*`.
    pub name: String,
    /// The field's type.
    pub ty: FieldType,
    /// Explicit bit width for integer fields.
    pub bits: Option<u32>,
    /// Quantization step for `Fx`, `Vec2` and `Vec3` fields.
    pub quantize: Option<Fx>,
    /// Lower bound of the quantized range.
    pub min: Option<Fx>,
    /// Upper bound of the quantized range.
    pub max: Option<Fx>,
    /// Bits per retained component for smallest-three quaternion encoding.
    pub quantize_bits: Option<u32>,
    /// Number of variants for an `Enum` field.
    pub variants: Option<u32>,
    /// Maximum length for `Str` and `Bytes` fields.
    pub max_len: Option<u32>,
    /// Replication priority multiplier. **Not** part of the canonical form or the schema ID.
    pub base_priority: f32,
}

impl FieldDesc {
    /// Creates a field with no optional parameters set.
    pub fn new(name: impl Into<String>, ty: FieldType) -> FieldDesc {
        FieldDesc {
            name: name.into(),
            ty,
            bits: None,
            quantize: None,
            min: None,
            max: None,
            quantize_bits: None,
            variants: None,
            max_len: None,
            base_priority: 1.0,
        }
    }

    /// Sets an explicit integer bit width.
    pub fn with_bits(mut self, bits: u32) -> FieldDesc {
        self.bits = Some(bits);
        self
    }

    /// Sets the quantization step and range.
    pub fn with_quantize(mut self, step: Fx, min: Fx, max: Fx) -> FieldDesc {
        self.quantize = Some(step);
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// Sets smallest-three bits per component for a quaternion field.
    pub fn with_quantize_bits(mut self, bits: u32) -> FieldDesc {
        self.quantize_bits = Some(bits);
        self
    }

    /// Sets the variant count for an enum field.
    pub fn with_variants(mut self, variants: u32) -> FieldDesc {
        self.variants = Some(variants);
        self
    }

    /// Sets the maximum length for a string or byte field.
    pub fn with_max_len(mut self, max_len: u32) -> FieldDesc {
        self.max_len = Some(max_len);
        self
    }

    /// Sets the replication priority multiplier.
    pub fn with_priority(mut self, priority: f32) -> FieldDesc {
        self.base_priority = priority;
        self
    }

    /// True if this field carries quantization parameters.
    pub fn is_quantized(&self) -> bool {
        self.quantize.is_some() && self.min.is_some() && self.max.is_some()
    }

    /// Number of bits a quantized value of this field occupies.
    ///
    /// Returns `None` for fields without quantization parameters.
    pub fn quantized_bits(&self) -> Option<u32> {
        let (step, min, max) = (self.quantize?, self.min?, self.max?);
        Some(quantized_bits(min, max, step))
    }

    /// Validates the declaration against `docs/spec/schema-and-hashing.md` §6.
    pub fn validate(&self) -> Result<(), WireError> {
        if !is_identifier(&self.name) {
            return Err(WireError::InvalidSchema(format!(
                "field name {:?} is not an identifier",
                self.name
            )));
        }
        if let Some(bits) = self.bits {
            if bits == 0 || bits > 64 {
                return Err(WireError::InvalidSchema(format!(
                    "field {}: bits must be 1..=64, got {bits}",
                    self.name
                )));
            }
        }
        match (self.quantize, self.min, self.max) {
            (None, None, None) => {}
            (Some(step), Some(min), Some(max)) => {
                if step.raw() <= 0 {
                    return Err(WireError::InvalidSchema(format!(
                        "field {}: quantize step must be positive",
                        self.name
                    )));
                }
                if min.raw() >= max.raw() {
                    return Err(WireError::InvalidSchema(format!(
                        "field {}: min must be less than max",
                        self.name
                    )));
                }
            }
            _ => {
                return Err(WireError::InvalidSchema(format!(
                    "field {}: quantize, min and max must be declared together",
                    self.name
                )))
            }
        }
        if self.ty == FieldType::Enum {
            match self.variants {
                Some(v) if v >= 1 => {}
                _ => {
                    return Err(WireError::InvalidSchema(format!(
                        "field {}: enum requires variants >= 1",
                        self.name
                    )))
                }
            }
        }
        if let Some(qb) = self.quantize_bits {
            if qb == 0 || qb > 30 {
                return Err(WireError::InvalidSchema(format!(
                    "field {}: quantize_bits must be 1..=30, got {qb}",
                    self.name
                )));
            }
        }
        Ok(())
    }

    /// Appends this field's canonical line, without a trailing newline.
    fn write_canonical(&self, out: &mut String) {
        let _ = write!(out, "  field {} {}", self.name, self.ty.canonical_name());
        // Parameters are emitted in byte-wise ascending name order, and only when explicitly set.
        // Emitting defaults would make adding a new defaulted parameter a breaking change.
        if let Some(v) = self.bits {
            let _ = write!(out, " bits={v}");
        }
        if let Some(v) = self.max {
            let _ = write!(out, " max={}", render_fx(v));
        }
        if let Some(v) = self.max_len {
            let _ = write!(out, " max_len={v}");
        }
        if let Some(v) = self.min {
            let _ = write!(out, " min={}", render_fx(v));
        }
        if let Some(v) = self.quantize {
            let _ = write!(out, " quantize={}", render_fx(v));
        }
        if let Some(v) = self.quantize_bits {
            let _ = write!(out, " quantize_bits={v}");
        }
        if let Some(v) = self.variants {
            let _ = write!(out, " variants={v}");
        }
    }
}

/// A replicated component's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentDesc {
    /// Component name. Must match `[A-Za-z_][A-Za-z0-9_]*`.
    pub name: String,
    /// The component's fields, in declaration order. Canonicalisation sorts them.
    pub fields: Vec<FieldDesc>,
}

impl ComponentDesc {
    /// Creates a component from a name and fields.
    pub fn new(name: impl Into<String>, fields: Vec<FieldDesc>) -> ComponentDesc {
        ComponentDesc {
            name: name.into(),
            fields,
        }
    }

    /// Returns the fields in canonical (name-sorted) order.
    ///
    /// This is the order the wire format uses, so encoders and decoders must both go through here
    /// rather than iterating `fields` directly.
    pub fn canonical_fields(&self) -> Vec<&FieldDesc> {
        let mut out: Vec<&FieldDesc> = self.fields.iter().collect();
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        out
    }

    /// Validates the component and all its fields.
    pub fn validate(&self) -> Result<(), WireError> {
        if !is_identifier(&self.name) {
            return Err(WireError::InvalidSchema(format!(
                "component name {:?} is not an identifier",
                self.name
            )));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.fields.len());
        for f in &self.fields {
            f.validate()?;
            if seen.contains(&f.name.as_str()) {
                return Err(WireError::InvalidSchema(format!(
                    "component {}: duplicate field {}",
                    self.name, f.name
                )));
            }
            seen.push(&f.name);
        }
        Ok(())
    }
}

/// A complete set of component declarations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Schema {
    /// Components in registration order. Canonicalisation sorts them.
    pub components: Vec<ComponentDesc>,
}

impl Schema {
    /// Creates an empty schema.
    pub fn new() -> Schema {
        Schema::default()
    }

    /// Adds a component, validating it and rejecting duplicate names.
    pub fn register(&mut self, component: ComponentDesc) -> Result<(), WireError> {
        component.validate()?;
        if self.components.iter().any(|c| c.name == component.name) {
            return Err(WireError::InvalidSchema(format!(
                "duplicate component {}",
                component.name
            )));
        }
        self.components.push(component);
        Ok(())
    }

    /// Returns components in canonical (name-sorted) order.
    pub fn canonical_components(&self) -> Vec<&ComponentDesc> {
        let mut out: Vec<&ComponentDesc> = self.components.iter().collect();
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        out
    }

    /// Renders the canonical form: UTF-8, `\n` line endings, no trailing newline.
    pub fn canonical_form(&self) -> String {
        let mut out = String::with_capacity(64 * (1 + self.components.len()));
        let _ = write!(out, "tempo-schema {SCHEMA_FORMAT_VERSION}");
        for c in self.canonical_components() {
            let _ = write!(out, "\ncomponent {}", c.name);
            for f in c.canonical_fields() {
                out.push('\n');
                f.write_canonical(&mut out);
            }
        }
        out
    }

    /// The schema ID: the first 16 bytes of BLAKE3 over the canonical form.
    pub fn schema_id(&self) -> SchemaId {
        let hash = blake3::hash(self.canonical_form().as_bytes());
        let mut id = [0u8; 16];
        id.copy_from_slice(&hash.as_bytes()[..16]);
        SchemaId(id)
    }

    /// Looks up a component by name.
    pub fn component(&self, name: &str) -> Option<&ComponentDesc> {
        self.components.iter().find(|c| c.name == name)
    }

    /// Produces a human-readable difference against `other`.
    ///
    /// Required by `docs/spec/schema-and-hashing.md` §5, and normative rather than a nicety: a
    /// positional bit stream gives the user no other signal when schemas disagree, so a generic
    /// "protocol error" here costs hours.
    pub fn diff(&self, other: &Schema) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "schema mismatch: local {}, remote {}",
            self.schema_id(),
            other.schema_id()
        );

        let mut names: Vec<&str> = self
            .components
            .iter()
            .chain(other.components.iter())
            .map(|c| c.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();

        for name in names {
            match (self.component(name), other.component(name)) {
                (Some(_), None) => {
                    let _ = write!(out, "\n  + component {name}          (local only)");
                }
                (None, Some(_)) => {
                    let _ = write!(out, "\n  - component {name}          (remote only)");
                }
                (Some(a), Some(b)) => {
                    let mut lines = String::new();
                    let mut fields: Vec<&str> = a
                        .fields
                        .iter()
                        .chain(b.fields.iter())
                        .map(|f| f.name.as_str())
                        .collect();
                    fields.sort_unstable();
                    fields.dedup();
                    for fname in fields {
                        let fa = a.fields.iter().find(|f| f.name == fname);
                        let fb = b.fields.iter().find(|f| f.name == fname);
                        match (fa, fb) {
                            (Some(_), None) => {
                                let _ = write!(lines, "\n    + field {fname}   (local only)");
                            }
                            (None, Some(_)) => {
                                let _ = write!(lines, "\n    - field {fname}   (remote only)");
                            }
                            (Some(x), Some(y)) => {
                                let (mut lx, mut ly) = (String::new(), String::new());
                                x.write_canonical(&mut lx);
                                y.write_canonical(&mut ly);
                                if lx != ly {
                                    let _ = write!(
                                        lines,
                                        "\n    ~ field {fname}\n        local:  {}\n        remote: {}",
                                        lx.trim_start(),
                                        ly.trim_start()
                                    );
                                }
                            }
                            (None, None) => unreachable!("name came from one of the two lists"),
                        }
                    }
                    if !lines.is_empty() {
                        let _ = write!(out, "\n  component {name}{lines}");
                    }
                }
                (None, None) => unreachable!("name came from one of the two lists"),
            }
        }
        out
    }
}

/// A 128-bit schema identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchemaId(pub [u8; 16]);

impl core::fmt::Display for SchemaId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// Renders an `Fx` for the canonical form: raw `i64` in lowercase hex, sign outside the prefix.
///
/// Rendering the raw integer rather than a decimal string keeps float formatting out of
/// canonicalisation entirely. Two languages formatting `0.001` as `0.001` and `1e-3` would hash
/// differently for identical schemas; two languages formatting the same `i64` cannot.
pub fn render_fx(v: Fx) -> String {
    let raw = v.raw();
    if raw < 0 {
        format!("-0x{:x}", (raw as i128).unsigned_abs())
    } else {
        format!("0x{raw:x}")
    }
}

/// Number of bits needed to encode a value quantized over `[min, max]` with step `step`.
///
/// Computed on raw values in 128-bit arithmetic. Both operands carry the same `2^32` scale, so the
/// ratio is exact and unscaled — and unlike `Fx` division it cannot saturate for wide ranges.
pub fn quantized_bits(min: Fx, max: Fx, step: Fx) -> u32 {
    debug_assert!(step.raw() > 0 && max.raw() > min.raw());
    let span = max.raw() as i128 - min.raw() as i128;
    let steps = span / step.raw() as i128 + 1;
    let largest = (steps - 1).max(0) as u128;
    128 - largest.leading_zeros()
}

/// True if `s` matches `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> ComponentDesc {
        ComponentDesc::new(
            "Player",
            vec![
                FieldDesc::new("position", FieldType::Vec2)
                    .with_quantize(
                        Fx::from_raw(0x0041_8937),
                        Fx::from_int(-1000),
                        Fx::from_int(1000),
                    )
                    .with_priority(2.0),
                FieldDesc::new("health", FieldType::Fx),
                FieldDesc::new("score", FieldType::Uint).with_bits(10),
                FieldDesc::new("alive", FieldType::Bool),
            ],
        )
    }

    #[test]
    fn canonical_form_matches_the_spec_example() {
        let mut s = Schema::new();
        s.register(player()).unwrap();
        // Exactly the example in docs/spec/schema-and-hashing.md §3.1.
        let expected = "tempo-schema 1\n\
             component Player\n  \
             field alive bool\n  \
             field health fx\n  \
             field position vec2 max=0x3e800000000 min=-0x3e800000000 quantize=0x418937\n  \
             field score uint bits=10";
        assert_eq!(s.canonical_form(), expected);
    }

    #[test]
    fn declaration_order_does_not_affect_the_schema_id() {
        let mut a = Schema::new();
        a.register(player()).unwrap();

        let mut shuffled = player();
        shuffled.fields.reverse();
        let mut b = Schema::new();
        b.register(shuffled).unwrap();

        assert_eq!(a.canonical_form(), b.canonical_form());
        assert_eq!(a.schema_id(), b.schema_id());
    }

    #[test]
    fn component_order_does_not_affect_the_schema_id() {
        let other = ComponentDesc::new("Alpha", vec![FieldDesc::new("v", FieldType::Bool)]);
        let mut a = Schema::new();
        a.register(player()).unwrap();
        a.register(other.clone()).unwrap();
        let mut b = Schema::new();
        b.register(other).unwrap();
        b.register(player()).unwrap();
        assert_eq!(a.schema_id(), b.schema_id());
    }

    #[test]
    fn names_sort_byte_wise_not_by_locale() {
        // 'Z' (0x5A) must come before 'a' (0x61). A case-insensitive or locale-aware sort would
        // put them the other way round and produce a different schema ID in one language.
        let c = ComponentDesc::new(
            "C",
            vec![
                FieldDesc::new("apple", FieldType::Bool),
                FieldDesc::new("Zebra", FieldType::Bool),
            ],
        );
        let names: Vec<&str> = c
            .canonical_fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["Zebra", "apple"]);
    }

    #[test]
    fn priority_is_not_part_of_the_schema_id() {
        // Tuning replication priority must never break wire compatibility.
        let mut a = Schema::new();
        a.register(player()).unwrap();

        let mut tuned = player();
        tuned.fields[0].base_priority = 99.0;
        let mut b = Schema::new();
        b.register(tuned).unwrap();

        assert_eq!(a.schema_id(), b.schema_id());
    }

    #[test]
    fn unset_parameters_are_absent_not_defaulted() {
        let c = ComponentDesc::new("C", vec![FieldDesc::new("v", FieldType::Fx)]);
        let mut s = Schema::new();
        s.register(c).unwrap();
        assert_eq!(
            s.canonical_form(),
            "tempo-schema 1\ncomponent C\n  field v fx"
        );
    }

    #[test]
    fn changing_any_encoding_parameter_changes_the_id() {
        let mut base = Schema::new();
        base.register(player()).unwrap();
        let base_id = base.schema_id();

        let mut renamed = player();
        renamed.fields[1].name = "hp".into();
        let mut s = Schema::new();
        s.register(renamed).unwrap();
        assert_ne!(
            s.schema_id(),
            base_id,
            "renaming a field is a breaking change"
        );

        let mut requantized = player();
        requantized.fields[0].quantize = Some(Fx::from_raw(0x0041_8938));
        let mut s = Schema::new();
        s.register(requantized).unwrap();
        assert_ne!(
            s.schema_id(),
            base_id,
            "changing quantization is a breaking change"
        );
    }

    #[test]
    fn quantized_bit_widths_are_exact() {
        // 2000 units at 0.001 precision is 2,000,001 steps, needing 21 bits.
        let f = &player().fields[0];
        assert_eq!(f.quantized_bits(), Some(21));

        // A step coarser than the range leaves exactly one representable value, which needs zero
        // bits — ceil(log2(1)) == 0. The field then occupies no space at all and always decodes
        // to `min`, which is correct: there is nothing else it could be.
        assert_eq!(quantized_bits(Fx::ZERO, Fx::ONE, Fx::from_int(2)), 0);
        // Two steps -> 1 bit; three steps -> 2 bits.
        assert_eq!(quantized_bits(Fx::ZERO, Fx::ONE, Fx::ONE), 1);
        assert_eq!(quantized_bits(Fx::ZERO, Fx::from_int(2), Fx::ONE), 2);
    }

    #[test]
    fn quantized_bits_does_not_saturate_on_a_full_range() {
        // Fx division would saturate here; the raw 128-bit path must not.
        let bits = quantized_bits(
            Fx::from_int(-2_000_000_000),
            Fx::from_int(2_000_000_000),
            Fx::ONE,
        );
        assert_eq!(bits, 32);
    }

    #[test]
    fn validation_rejects_malformed_declarations() {
        let bad_name = ComponentDesc::new("9bad", vec![]);
        assert!(bad_name.validate().is_err());

        let dup = ComponentDesc::new(
            "C",
            vec![
                FieldDesc::new("v", FieldType::Bool),
                FieldDesc::new("v", FieldType::Bool),
            ],
        );
        assert!(dup.validate().is_err());

        let bad_step = ComponentDesc::new(
            "C",
            vec![FieldDesc::new("v", FieldType::Fx).with_quantize(Fx::ZERO, Fx::ZERO, Fx::ONE)],
        );
        assert!(bad_step.validate().is_err());

        let inverted = ComponentDesc::new(
            "C",
            vec![FieldDesc::new("v", FieldType::Fx).with_quantize(Fx::ONE, Fx::ONE, Fx::ZERO)],
        );
        assert!(inverted.validate().is_err());

        let partial = ComponentDesc::new(
            "C",
            vec![{
                let mut f = FieldDesc::new("v", FieldType::Fx);
                f.quantize = Some(Fx::ONE);
                f
            }],
        );
        assert!(
            partial.validate().is_err(),
            "quantize without min/max must be rejected"
        );

        let enum_without_variants =
            ComponentDesc::new("C", vec![FieldDesc::new("v", FieldType::Enum)]);
        assert!(enum_without_variants.validate().is_err());
    }

    #[test]
    fn duplicate_components_are_rejected() {
        let mut s = Schema::new();
        s.register(player()).unwrap();
        assert!(s.register(player()).is_err());
    }

    #[test]
    fn diff_names_the_specific_disagreement() {
        let mut local = Schema::new();
        local.register(player()).unwrap();

        let mut changed = player();
        changed.fields[0].quantize = Some(Fx::from_raw(0x0010_624D));
        changed
            .fields
            .push(FieldDesc::new("armour", FieldType::Uint));
        let mut remote = Schema::new();
        remote.register(changed).unwrap();

        let d = local.diff(&remote);
        assert!(d.contains("component Player"), "{d}");
        assert!(d.contains("armour"), "{d}");
        assert!(d.contains("position"), "{d}");
        assert!(
            d.contains("quantize=0x418937"),
            "must show the local value: {d}"
        );
        assert!(
            d.contains("quantize=0x10624d"),
            "must show the remote value: {d}"
        );
    }

    #[test]
    fn fx_rendering_is_raw_hex() {
        assert_eq!(render_fx(Fx::ONE), "0x100000000");
        assert_eq!(render_fx(Fx::from_int(-1000)), "-0x3e800000000");
        assert_eq!(render_fx(Fx::ZERO), "0x0");
        // MIN cannot be negated in i64; rendering must widen rather than overflow.
        assert_eq!(render_fx(Fx::MIN), "-0x8000000000000000");
    }
}
