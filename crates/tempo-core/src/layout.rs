//! In-memory layout of replicated components.
//!
//! Each component occupies a fixed-size slot whose fields sit at computed offsets, in **canonical
//! (name-sorted) order** — the same order the wire format uses, so encoding is a linear walk over
//! the slot with no reordering.
//!
//! # Everything is little-endian
//!
//! Slot bytes are little-endian regardless of the host. This matters because the per-tick state
//! hash is taken over these bytes: if the layout were native-endian, two peers on different
//! architectures would compute different hashes for identical state and report a desync that is
//! not one.
//!
//! # Fixed-size fields only
//!
//! `string` and `bytes` fields cannot live in the arena. Their variable length would break the
//! flat `memcpy` that makes snapshot save and restore cheap, which is the property rollback and
//! lag compensation are built on ([ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md)).
//! They remain available for commands and RPCs, which are length-prefixed and reliably delivered.
//! See ADR-0027.

use tempo_fixed::{Fx, Quat, Vec2, Vec3};
use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

use crate::CoreError;

/// Byte width of a field type's arena slot, or `None` if it cannot be stored in the arena.
pub const fn slot_size(ty: FieldType) -> Option<usize> {
    Some(match ty {
        FieldType::Bool => 1,
        FieldType::Enum => 4,
        FieldType::Uint | FieldType::Int | FieldType::Fx => 8,
        FieldType::Vec2 => 16,
        FieldType::Vec3 => 24,
        FieldType::Quat => 32,
        FieldType::Str | FieldType::Bytes => return None,
    })
}

/// Where a field sits inside a component's slot.
#[derive(Debug, Clone)]
pub struct FieldLayout {
    /// The field's declaration.
    pub desc: FieldDesc,
    /// Byte offset within the component slot.
    pub offset: usize,
    /// Byte width.
    pub size: usize,
}

/// The layout of one component's arena slot.
#[derive(Debug, Clone)]
pub struct ComponentLayout {
    /// The component's declaration.
    pub desc: ComponentDesc,
    /// Fields in canonical order, which is also wire order.
    pub fields: Vec<FieldLayout>,
    /// Total bytes per entity.
    pub stride: usize,
}

impl ComponentLayout {
    /// Computes the layout for a component.
    ///
    /// Fails if any field's type cannot be stored in the arena.
    pub fn new(desc: ComponentDesc) -> Result<ComponentLayout, CoreError> {
        let mut fields = Vec::with_capacity(desc.fields.len());
        let mut offset = 0usize;
        for f in desc.canonical_fields() {
            let size = slot_size(f.ty).ok_or_else(|| CoreError::UnsupportedFieldType {
                component: desc.name.clone(),
                field: f.name.clone(),
                ty: f.ty,
            })?;
            fields.push(FieldLayout {
                desc: f.clone(),
                offset,
                size,
            });
            offset += size;
        }
        Ok(ComponentLayout {
            desc,
            fields,
            stride: offset,
        })
    }

    /// Index of a field by name, in canonical order.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.desc.name == name)
    }

    /// Number of fields, which is also the width of the dirty mask.
    #[inline]
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// Reads a field from a slot.
    pub fn read(&self, slot: &[u8], field: usize) -> Value {
        let f = &self.fields[field];
        let b = &slot[f.offset..f.offset + f.size];
        match f.desc.ty {
            FieldType::Bool => Value::Bool(b[0] != 0),
            FieldType::Enum => Value::Enum(u32::from_le_bytes(b[..4].try_into().unwrap())),
            FieldType::Uint => Value::Uint(u64::from_le_bytes(b[..8].try_into().unwrap())),
            FieldType::Int => Value::Int(i64::from_le_bytes(b[..8].try_into().unwrap())),
            FieldType::Fx => Value::Fx(read_fx(b)),
            FieldType::Vec2 => Value::Vec2(Vec2::new(read_fx(&b[0..8]), read_fx(&b[8..16]))),
            FieldType::Vec3 => Value::Vec3(Vec3::new(
                read_fx(&b[0..8]),
                read_fx(&b[8..16]),
                read_fx(&b[16..24]),
            )),
            FieldType::Quat => Value::Quat(Quat::new(
                read_fx(&b[0..8]),
                read_fx(&b[8..16]),
                read_fx(&b[16..24]),
                read_fx(&b[24..32]),
            )),
            FieldType::Str | FieldType::Bytes => {
                unreachable!("layout construction rejects variable-length fields")
            }
        }
    }

    /// Writes a field into a slot.
    pub fn write(&self, slot: &mut [u8], field: usize, value: &Value) -> Result<(), CoreError> {
        let f = &self.fields[field];
        if value.field_type() != f.desc.ty {
            return Err(CoreError::TypeMismatch {
                component: self.desc.name.clone(),
                field: f.desc.name.clone(),
                expected: f.desc.ty,
                found: value.field_type(),
            });
        }
        let b = &mut slot[f.offset..f.offset + f.size];
        match value {
            Value::Bool(v) => b[0] = *v as u8,
            Value::Enum(v) => b[..4].copy_from_slice(&v.to_le_bytes()),
            Value::Uint(v) => b[..8].copy_from_slice(&v.to_le_bytes()),
            Value::Int(v) => b[..8].copy_from_slice(&v.to_le_bytes()),
            Value::Fx(v) => write_fx(b, *v),
            Value::Vec2(v) => {
                write_fx(&mut b[0..8], v.x);
                write_fx(&mut b[8..16], v.y);
            }
            Value::Vec3(v) => {
                write_fx(&mut b[0..8], v.x);
                write_fx(&mut b[8..16], v.y);
                write_fx(&mut b[16..24], v.z);
            }
            Value::Quat(v) => {
                write_fx(&mut b[0..8], v.x);
                write_fx(&mut b[8..16], v.y);
                write_fx(&mut b[16..24], v.z);
                write_fx(&mut b[24..32], v.w);
            }
            Value::Str(_) | Value::Bytes(_) => {
                return Err(CoreError::UnsupportedFieldType {
                    component: self.desc.name.clone(),
                    field: f.desc.name.clone(),
                    ty: value.field_type(),
                })
            }
        }
        Ok(())
    }

    /// Reads every field of a slot, in canonical order.
    pub fn read_all(&self, slot: &[u8]) -> Vec<Value> {
        (0..self.fields.len()).map(|i| self.read(slot, i)).collect()
    }
}

#[inline]
fn read_fx(b: &[u8]) -> Fx {
    Fx::from_raw(i64::from_le_bytes(b[..8].try_into().unwrap()))
}

#[inline]
fn write_fx(b: &mut [u8], v: Fx) {
    b[..8].copy_from_slice(&v.raw().to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> ComponentLayout {
        ComponentLayout::new(ComponentDesc::new(
            "Player",
            vec![
                FieldDesc::new("position", FieldType::Vec2),
                FieldDesc::new("alive", FieldType::Bool),
                FieldDesc::new("score", FieldType::Uint),
                FieldDesc::new("rotation", FieldType::Quat),
            ],
        ))
        .unwrap()
    }

    #[test]
    fn fields_are_laid_out_in_canonical_order() {
        let l = layout();
        let names: Vec<&str> = l.fields.iter().map(|f| f.desc.name.as_str()).collect();
        // Name-sorted, not declaration order — the same order the wire format walks.
        assert_eq!(names, vec!["alive", "position", "rotation", "score"]);
        assert_eq!(l.stride, 1 + 16 + 32 + 8);
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 1);
    }

    #[test]
    fn values_round_trip_through_a_slot() {
        let l = layout();
        let mut slot = vec![0u8; l.stride];
        let values = [
            Value::Bool(true),
            Value::Vec2(Vec2::from_ints(3, -4)),
            Value::Quat(Quat::IDENTITY),
            Value::Uint(9_999_999),
        ];
        for (i, v) in values.iter().enumerate() {
            l.write(&mut slot, i, v).unwrap();
        }
        assert_eq!(l.read_all(&slot), values.to_vec());
    }

    #[test]
    fn slots_are_little_endian_regardless_of_host() {
        // The state hash is taken over these bytes, so native-endian layout would make two peers
        // on different architectures disagree about identical state.
        let l = ComponentLayout::new(ComponentDesc::new(
            "C",
            vec![FieldDesc::new("v", FieldType::Uint)],
        ))
        .unwrap();
        let mut slot = vec![0u8; l.stride];
        l.write(&mut slot, 0, &Value::Uint(1)).unwrap();
        assert_eq!(slot[0], 1, "least significant byte first");
        assert_eq!(slot[7], 0);
    }

    #[test]
    fn variable_length_fields_are_rejected_at_layout_time() {
        // Rejecting here rather than at write time means the error names the schema, which is
        // where the mistake actually is.
        let err = ComponentLayout::new(ComponentDesc::new(
            "C",
            vec![FieldDesc::new("name", FieldType::Str)],
        ));
        assert!(matches!(err, Err(CoreError::UnsupportedFieldType { .. })));
    }

    #[test]
    fn writing_the_wrong_type_is_rejected() {
        let l = layout();
        let mut slot = vec![0u8; l.stride];
        assert!(matches!(
            l.write(&mut slot, 0, &Value::Uint(1)),
            Err(CoreError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn field_lookup_uses_canonical_indices() {
        let l = layout();
        assert_eq!(l.field_index("alive"), Some(0));
        assert_eq!(l.field_index("score"), Some(3));
        assert_eq!(l.field_index("missing"), None);
    }
}
