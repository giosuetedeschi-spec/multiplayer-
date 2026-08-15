//! Deterministic vector and quaternion types built on [`Fx`].
//!
//! Determinism follows from the scalar operations, but **evaluation order is normative**: because
//! `Fx` addition saturates, it is not associative near the range limits, so `(a + b) + c` and
//! `a + (b + c)` can differ. Every composite operation here evaluates strictly left to right, in
//! the order written in `docs/spec/fixed-point.md` §3, and reordering them for readability would
//! be a wire-visible change.

use crate::Fx;
use core::fmt;
use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

/// A two-dimensional vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct Vec2 {
    /// X component.
    pub x: Fx,
    /// Y component.
    pub y: Fx,
}

/// A three-dimensional vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct Vec3 {
    /// X component.
    pub x: Fx,
    /// Y component.
    pub y: Fx,
    /// Z component.
    pub z: Fx,
}

/// A quaternion, in Hamilton convention.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Quat {
    /// X component (vector part).
    pub x: Fx,
    /// Y component (vector part).
    pub y: Fx,
    /// Z component (vector part).
    pub z: Fx,
    /// W component (scalar part).
    pub w: Fx,
}

// ---------------------------------------------------------------------------------------------
// Vec2
// ---------------------------------------------------------------------------------------------

impl Vec2 {
    /// The zero vector.
    pub const ZERO: Vec2 = Vec2 {
        x: Fx::ZERO,
        y: Fx::ZERO,
    };
    /// The unit vector along X.
    pub const X: Vec2 = Vec2 {
        x: Fx::ONE,
        y: Fx::ZERO,
    };
    /// The unit vector along Y.
    pub const Y: Vec2 = Vec2 {
        x: Fx::ZERO,
        y: Fx::ONE,
    };

    /// Constructs from components.
    #[inline]
    pub const fn new(x: Fx, y: Fx) -> Vec2 {
        Vec2 { x, y }
    }

    /// Constructs from integer components.
    #[inline]
    pub const fn from_ints(x: i32, y: i32) -> Vec2 {
        Vec2 {
            x: Fx::from_int(x),
            y: Fx::from_int(y),
        }
    }

    /// Scales every component by a scalar.
    #[inline]
    pub const fn scale(self, s: Fx) -> Vec2 {
        Vec2 {
            x: self.x.mul(s),
            y: self.y.mul(s),
        }
    }

    /// Dot product, evaluated left to right.
    #[inline]
    pub const fn dot(self, rhs: Vec2) -> Fx {
        self.x.mul(rhs.x).add(self.y.mul(rhs.y))
    }

    /// Squared length. Prefer this to [`Vec2::length`] for comparisons — it avoids a square root.
    #[inline]
    pub const fn length_sq(self) -> Fx {
        self.dot(self)
    }

    /// Length.
    #[inline]
    pub const fn length(self) -> Fx {
        self.length_sq().sqrt()
    }

    /// Distance between two points.
    #[inline]
    pub const fn distance(self, rhs: Vec2) -> Fx {
        self.sub(rhs).length()
    }

    /// Unit vector in the same direction. The zero vector normalises to itself, making this total.
    #[inline]
    pub const fn normalize(self) -> Vec2 {
        let len = self.length();
        if len.is_zero() {
            Vec2::ZERO
        } else {
            self.scale(Fx::ONE.div(len))
        }
    }

    /// Perpendicular vector, rotated a quarter turn counter-clockwise.
    #[inline]
    pub const fn perp(self) -> Vec2 {
        Vec2 {
            x: self.y.neg(),
            y: self.x,
        }
    }

    /// Linear interpolation. `t` is not clamped.
    #[inline]
    pub const fn lerp(self, to: Vec2, t: Fx) -> Vec2 {
        self.add(to.sub(self).scale(t))
    }

    /// Componentwise addition.
    #[inline]
    pub const fn add(self, rhs: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.add(rhs.x),
            y: self.y.add(rhs.y),
        }
    }

    /// Componentwise subtraction.
    #[inline]
    pub const fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2 {
            x: self.x.sub(rhs.x),
            y: self.y.sub(rhs.y),
        }
    }

    /// Componentwise negation.
    #[inline]
    pub const fn neg(self) -> Vec2 {
        Vec2 {
            x: self.x.neg(),
            y: self.y.neg(),
        }
    }

    /// The angle of this vector from the positive X axis, in radians.
    #[inline]
    pub fn angle(self) -> Fx {
        Fx::atan2(self.y, self.x)
    }

    /// Rotates by an angle in radians.
    #[inline]
    pub fn rotate(self, radians: Fx) -> Vec2 {
        let (s, c) = (radians.sin(), radians.cos());
        Vec2 {
            x: self.x.mul(c).sub(self.y.mul(s)),
            y: self.x.mul(s).add(self.y.mul(c)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Vec3
// ---------------------------------------------------------------------------------------------

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Vec3 = Vec3 {
        x: Fx::ZERO,
        y: Fx::ZERO,
        z: Fx::ZERO,
    };
    /// The unit vector along X.
    pub const X: Vec3 = Vec3 {
        x: Fx::ONE,
        y: Fx::ZERO,
        z: Fx::ZERO,
    };
    /// The unit vector along Y.
    pub const Y: Vec3 = Vec3 {
        x: Fx::ZERO,
        y: Fx::ONE,
        z: Fx::ZERO,
    };
    /// The unit vector along Z.
    pub const Z: Vec3 = Vec3 {
        x: Fx::ZERO,
        y: Fx::ZERO,
        z: Fx::ONE,
    };

    /// Constructs from components.
    #[inline]
    pub const fn new(x: Fx, y: Fx, z: Fx) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// Constructs from integer components.
    #[inline]
    pub const fn from_ints(x: i32, y: i32, z: i32) -> Vec3 {
        Vec3 {
            x: Fx::from_int(x),
            y: Fx::from_int(y),
            z: Fx::from_int(z),
        }
    }

    /// Scales every component by a scalar.
    #[inline]
    pub const fn scale(self, s: Fx) -> Vec3 {
        Vec3 {
            x: self.x.mul(s),
            y: self.y.mul(s),
            z: self.z.mul(s),
        }
    }

    /// Dot product, evaluated strictly left to right.
    #[inline]
    pub const fn dot(self, rhs: Vec3) -> Fx {
        self.x
            .mul(rhs.x)
            .add(self.y.mul(rhs.y))
            .add(self.z.mul(rhs.z))
    }

    /// Cross product.
    #[inline]
    pub const fn cross(self, rhs: Vec3) -> Vec3 {
        Vec3 {
            x: self.y.mul(rhs.z).sub(self.z.mul(rhs.y)),
            y: self.z.mul(rhs.x).sub(self.x.mul(rhs.z)),
            z: self.x.mul(rhs.y).sub(self.y.mul(rhs.x)),
        }
    }

    /// Squared length.
    #[inline]
    pub const fn length_sq(self) -> Fx {
        self.dot(self)
    }

    /// Length.
    #[inline]
    pub const fn length(self) -> Fx {
        self.length_sq().sqrt()
    }

    /// Distance between two points.
    #[inline]
    pub const fn distance(self, rhs: Vec3) -> Fx {
        self.sub(rhs).length()
    }

    /// Unit vector in the same direction. The zero vector normalises to itself.
    #[inline]
    pub const fn normalize(self) -> Vec3 {
        let len = self.length();
        if len.is_zero() {
            Vec3::ZERO
        } else {
            self.scale(Fx::ONE.div(len))
        }
    }

    /// Linear interpolation. `t` is not clamped.
    #[inline]
    pub const fn lerp(self, to: Vec3, t: Fx) -> Vec3 {
        self.add(to.sub(self).scale(t))
    }

    /// Componentwise addition.
    #[inline]
    pub const fn add(self, rhs: Vec3) -> Vec3 {
        Vec3 {
            x: self.x.add(rhs.x),
            y: self.y.add(rhs.y),
            z: self.z.add(rhs.z),
        }
    }

    /// Componentwise subtraction.
    #[inline]
    pub const fn sub(self, rhs: Vec3) -> Vec3 {
        Vec3 {
            x: self.x.sub(rhs.x),
            y: self.y.sub(rhs.y),
            z: self.z.sub(rhs.z),
        }
    }

    /// Componentwise negation.
    #[inline]
    pub const fn neg(self) -> Vec3 {
        Vec3 {
            x: self.x.neg(),
            y: self.y.neg(),
            z: self.z.neg(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Quat
// ---------------------------------------------------------------------------------------------

impl Quat {
    /// The identity rotation.
    pub const IDENTITY: Quat = Quat {
        x: Fx::ZERO,
        y: Fx::ZERO,
        z: Fx::ZERO,
        w: Fx::ONE,
    };

    /// Constructs from components.
    #[inline]
    pub const fn new(x: Fx, y: Fx, z: Fx, w: Fx) -> Quat {
        Quat { x, y, z, w }
    }

    /// Constructs a rotation of `radians` about a (not necessarily unit) `axis`.
    #[inline]
    pub fn from_axis_angle(axis: Vec3, radians: Fx) -> Quat {
        let half = radians.mul(Fx::HALF);
        let s = half.sin();
        let n = axis.normalize();
        Quat {
            x: n.x.mul(s),
            y: n.y.mul(s),
            z: n.z.mul(s),
            w: half.cos(),
        }
    }

    /// Hamilton product. Evaluation order is normative — see `docs/spec/fixed-point.md` §3.
    #[inline]
    pub const fn mul(self, rhs: Quat) -> Quat {
        Quat {
            x: self
                .w
                .mul(rhs.x)
                .add(self.x.mul(rhs.w))
                .add(self.y.mul(rhs.z))
                .add(self.z.mul(rhs.y).neg()),
            y: self
                .w
                .mul(rhs.y)
                .add(self.x.mul(rhs.z).neg())
                .add(self.y.mul(rhs.w))
                .add(self.z.mul(rhs.x)),
            z: self
                .w
                .mul(rhs.z)
                .add(self.x.mul(rhs.y))
                .add(self.y.mul(rhs.x).neg())
                .add(self.z.mul(rhs.w)),
            w: self
                .w
                .mul(rhs.w)
                .sub(self.x.mul(rhs.x))
                .sub(self.y.mul(rhs.y))
                .sub(self.z.mul(rhs.z)),
        }
    }

    /// Conjugate, which is the inverse for unit quaternions.
    #[inline]
    pub const fn conjugate(self) -> Quat {
        Quat {
            x: self.x.neg(),
            y: self.y.neg(),
            z: self.z.neg(),
            w: self.w,
        }
    }

    /// Squared norm.
    #[inline]
    pub const fn length_sq(self) -> Fx {
        self.x
            .mul(self.x)
            .add(self.y.mul(self.y))
            .add(self.z.mul(self.z))
            .add(self.w.mul(self.w))
    }

    /// Norm.
    #[inline]
    pub const fn length(self) -> Fx {
        self.length_sq().sqrt()
    }

    /// Unit quaternion. A zero quaternion normalises to [`Quat::IDENTITY`], keeping this total.
    #[inline]
    pub const fn normalize(self) -> Quat {
        let len = self.length();
        if len.is_zero() {
            Quat::IDENTITY
        } else {
            let inv = Fx::ONE.div(len);
            Quat {
                x: self.x.mul(inv),
                y: self.y.mul(inv),
                z: self.z.mul(inv),
                w: self.w.mul(inv),
            }
        }
    }

    /// Rotates a vector by this quaternion, assuming it is unit length.
    #[inline]
    pub const fn rotate(self, v: Vec3) -> Vec3 {
        // t = 2 * (q_vec × v); result = v + w*t + q_vec × t
        let qv = Vec3 {
            x: self.x,
            y: self.y,
            z: self.z,
        };
        let t = qv.cross(v).scale(Fx::from_int(2));
        v.add(t.scale(self.w)).add(qv.cross(t))
    }
}

impl Default for Quat {
    #[inline]
    fn default() -> Quat {
        Quat::IDENTITY
    }
}

// ---------------------------------------------------------------------------------------------
// Operator sugar
// ---------------------------------------------------------------------------------------------

macro_rules! vec_ops {
    ($t:ty, $($field:ident),+) => {
        impl Add for $t {
            type Output = $t;
            #[inline]
            fn add(self, rhs: $t) -> $t {
                <$t>::add(self, rhs)
            }
        }
        impl Sub for $t {
            type Output = $t;
            #[inline]
            fn sub(self, rhs: $t) -> $t {
                <$t>::sub(self, rhs)
            }
        }
        impl Neg for $t {
            type Output = $t;
            #[inline]
            fn neg(self) -> $t {
                <$t>::neg(self)
            }
        }
        impl Mul<Fx> for $t {
            type Output = $t;
            #[inline]
            fn mul(self, s: Fx) -> $t {
                self.scale(s)
            }
        }
        impl AddAssign for $t {
            #[inline]
            fn add_assign(&mut self, rhs: $t) {
                $(self.$field = self.$field.add(rhs.$field);)+
            }
        }
        impl SubAssign for $t {
            #[inline]
            fn sub_assign(&mut self, rhs: $t) {
                $(self.$field = self.$field.sub(rhs.$field);)+
            }
        }
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($t), "("))?;
                let mut first = true;
                $(
                    if !first { write!(f, ", ")?; }
                    first = false;
                    write!(f, "{}", self.$field)?;
                )+
                let _ = first;
                write!(f, ")")
            }
        }
    };
}

vec_ops!(Vec2, x, y);
vec_ops!(Vec3, x, y, z);

impl Mul for Quat {
    type Output = Quat;
    #[inline]
    fn mul(self, rhs: Quat) -> Quat {
        Quat::mul(self, rhs)
    }
}

impl fmt::Debug for Quat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Quat({}, {}, {}, {})", self.x, self.y, self.z, self.w)
    }
}
