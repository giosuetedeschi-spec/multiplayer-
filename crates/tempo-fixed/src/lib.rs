//! Deterministic Q32.32 fixed-point arithmetic.
//!
//! Every operation here produces bit-identical results on every platform and in every language
//! binding. That property is the foundation the rest of `tempo` is built on: rollback re-simulates
//! past ticks and expects the same answer, peers compare per-tick state hashes to detect desync,
//! and deterministic replay reproduces a recorded session exactly.
//!
//! Floating point cannot deliver this across six languages and two architectures — libm differs,
//! compilers reassociate, JITs change precision between tiers. See
//! [ADR-0002](../../../docs/adr/0002-fixed-point-determinism.md) for the full argument, and
//! `docs/spec/fixed-point.md` for the normative semantics this module implements.
//!
//! # The two rules that matter
//!
//! - **`mul` floors, `div` truncates toward zero.** The asymmetry is deliberate: `mul` reduces via
//!   an arithmetic shift (which floors), while integer division in every target language truncates
//!   toward zero. Specifying anything else would force every implementation to add a correction
//!   step.
//! - **Everything saturates.** Overflow clamps to [`Fx::MIN`] or [`Fx::MAX`]; it never wraps.
//!   Wrapping turns a small numerical error into a catastrophic one.
//!
//! # Example
//!
//! ```
//! use tempo_fixed::Fx;
//!
//! let dt = Fx::from_ratio(1, 60);          // one tick at 60 Hz
//! let velocity = Fx::from_int(10);         // units per second
//! let step = velocity * dt;
//! assert_eq!(step.to_f64_lossy().round() as i64, 0); // 0.1666… units
//! ```

#![forbid(unsafe_code)]

mod tables;
mod vector;

pub use vector::{Quat, Vec2, Vec3};

use core::fmt;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Rem, Sub, SubAssign};

/// Number of fractional bits. Q32.32.
pub const FRAC_BITS: u32 = 32;

/// Raw value of [`Fx::ONE`].
const ONE_RAW: i64 = 1 << FRAC_BITS;
/// Mask selecting the fractional bits of a raw value.
const FRAC_MASK: i64 = ONE_RAW - 1;

/// A signed Q32.32 fixed-point number.
///
/// The stored value is `raw`, interpreted as `raw / 2^32`. Resolution is `2^-32` (about 2.3e-10)
/// and the range is roughly `±2.1e9`.
///
/// See the module documentation for the rounding and overflow rules, which are normative.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Fx(i64);

// ---------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------

impl Fx {
    /// Zero.
    pub const ZERO: Fx = Fx(0);
    /// One.
    pub const ONE: Fx = Fx(ONE_RAW);
    /// Negative one.
    pub const NEG_ONE: Fx = Fx(-ONE_RAW);
    /// One half.
    pub const HALF: Fx = Fx(ONE_RAW / 2);
    /// The smallest representable positive value, `2^-32`.
    pub const EPSILON: Fx = Fx(1);
    /// The most negative representable value.
    pub const MIN: Fx = Fx(i64::MIN);
    /// The largest representable value.
    pub const MAX: Fx = Fx(i64::MAX);

    /// π. See `docs/spec/fixed-point.md` §1.1 — these are committed exact values, never computed.
    pub const PI: Fx = Fx(0x0000_0003_243F_6A89);
    /// τ, or 2π.
    pub const TAU: Fx = Fx(0x0000_0006_487E_D511);
    /// π/2.
    pub const FRAC_PI_2: Fx = Fx(0x0000_0001_921F_B544);
    /// 1/τ, used to convert radians to turns.
    pub const INV_TAU: Fx = Fx(0x0000_0000_28BE_60DC);
    /// Euler's number.
    pub const E: Fx = Fx(0x0000_0002_B7E1_5163);
    /// log₂(e), used to derive `exp` from `exp2`.
    pub const LOG2_E: Fx = Fx(0x0000_0001_7154_7653);
    /// ln(2), used to derive `ln` from `log2`.
    pub const LN_2: Fx = Fx(0x0000_0000_B172_17F8);
    /// √2.
    pub const SQRT_2: Fx = Fx(0x0000_0001_6A09_E668);
    /// 1/√2, the bound on smallest-three quaternion components.
    pub const FRAC_1_SQRT_2: Fx = Fx(0x0000_0000_B504_F334);
}

// ---------------------------------------------------------------------------------------------
// Construction and conversion
// ---------------------------------------------------------------------------------------------

/// Clamps a 128-bit intermediate into `i64`, saturating rather than wrapping.
#[inline]
const fn sat(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

impl Fx {
    /// Constructs from a raw Q32.32 value.
    #[inline]
    pub const fn from_raw(raw: i64) -> Fx {
        Fx(raw)
    }

    /// Returns the raw Q32.32 value.
    ///
    /// This is the serialised representation: writing an `Fx` to the wire writes this `i64`.
    #[inline]
    pub const fn raw(self) -> i64 {
        self.0
    }

    /// Constructs from an integer, saturating if out of range.
    #[inline]
    pub const fn from_int(i: i32) -> Fx {
        Fx((i as i64) << FRAC_BITS)
    }

    /// Constructs the exact ratio `num / den`, saturating on overflow.
    ///
    /// Prefer this to converting a float literal: `Fx::from_ratio(1, 3)` is exact to the last bit,
    /// while parsing `0.3333` is not.
    #[inline]
    pub const fn from_ratio(num: i32, den: i32) -> Fx {
        if den == 0 {
            return if num > 0 {
                Fx::MAX
            } else if num < 0 {
                Fx::MIN
            } else {
                Fx::ZERO
            };
        }
        Fx(sat(((num as i128) << FRAC_BITS) / (den as i128)))
    }

    /// Converts from `f64`, rounding half away from zero.
    ///
    /// **Construction only.** Using this anywhere in a simulation path reintroduces exactly the
    /// platform-dependent behaviour this type exists to eliminate. It is intended for parsing
    /// configuration, writing test fixtures, and building constants offline.
    pub fn from_f64_lossy(v: f64) -> Fx {
        let scaled = v * (ONE_RAW as f64);
        if scaled.is_nan() {
            return Fx::ZERO;
        }
        let rounded = if scaled >= 0.0 {
            (scaled + 0.5).floor()
        } else {
            (scaled - 0.5).ceil()
        };
        if rounded >= i64::MAX as f64 {
            Fx::MAX
        } else if rounded <= i64::MIN as f64 {
            Fx::MIN
        } else {
            Fx(rounded as i64)
        }
    }

    /// Converts to `f64` for display and diagnostics.
    ///
    /// **Never** feed the result back into a simulation.
    #[inline]
    pub fn to_f64_lossy(self) -> f64 {
        self.0 as f64 / ONE_RAW as f64
    }
}

// ---------------------------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------------------------

impl Fx {
    /// Saturating addition.
    #[inline]
    pub const fn add(self, rhs: Fx) -> Fx {
        Fx(self.0.saturating_add(rhs.0))
    }

    /// Saturating subtraction.
    #[inline]
    pub const fn sub(self, rhs: Fx) -> Fx {
        Fx(self.0.saturating_sub(rhs.0))
    }

    /// Saturating negation. `(-Fx::MIN)` is `Fx::MAX`.
    #[inline]
    pub const fn neg(self) -> Fx {
        Fx(self.0.saturating_neg())
    }

    /// Saturating multiplication.
    ///
    /// The 128-bit product is reduced by an **arithmetic** shift, so results round toward negative
    /// infinity. This differs from [`Fx::div`] and the difference is normative.
    #[inline]
    pub const fn mul(self, rhs: Fx) -> Fx {
        Fx(sat((self.0 as i128 * rhs.0 as i128) >> FRAC_BITS))
    }

    /// Saturating division, truncating toward zero.
    ///
    /// Division by zero does not panic: it yields [`Fx::MAX`], [`Fx::MIN`] or [`Fx::ZERO`]
    /// according to the sign of the numerator. A mid-match divide-by-zero must not take down a
    /// session.
    #[inline]
    pub const fn div(self, rhs: Fx) -> Fx {
        if rhs.0 == 0 {
            return if self.0 > 0 {
                Fx::MAX
            } else if self.0 < 0 {
                Fx::MIN
            } else {
                Fx::ZERO
            };
        }
        Fx(sat(((self.0 as i128) << FRAC_BITS) / rhs.0 as i128))
    }

    /// Remainder, following [`Fx::div`]'s truncation toward zero.
    ///
    /// The raw integer remainder is already correct: for `a = A/2^32` and `b = B/2^32`,
    /// `a - b·trunc(a/b)` equals `(A % B)/2^32`, so no rescaling is needed.
    #[inline]
    pub const fn rem(self, rhs: Fx) -> Fx {
        if rhs.0 == 0 {
            Fx::ZERO
        } else {
            Fx(self.0 % rhs.0)
        }
    }

    /// Checked addition, returning `None` on overflow.
    #[inline]
    pub const fn checked_add(self, rhs: Fx) -> Option<Fx> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Fx(v)),
            None => None,
        }
    }

    /// Checked subtraction, returning `None` on overflow.
    #[inline]
    pub const fn checked_sub(self, rhs: Fx) -> Option<Fx> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Some(Fx(v)),
            None => None,
        }
    }

    /// Checked multiplication, returning `None` on overflow.
    #[inline]
    pub const fn checked_mul(self, rhs: Fx) -> Option<Fx> {
        let p = (self.0 as i128 * rhs.0 as i128) >> FRAC_BITS;
        if p > i64::MAX as i128 || p < i64::MIN as i128 {
            None
        } else {
            Some(Fx(p as i64))
        }
    }

    /// Checked division, returning `None` on overflow or division by zero.
    #[inline]
    pub const fn checked_div(self, rhs: Fx) -> Option<Fx> {
        if rhs.0 == 0 {
            return None;
        }
        let q = ((self.0 as i128) << FRAC_BITS) / rhs.0 as i128;
        if q > i64::MAX as i128 || q < i64::MIN as i128 {
            None
        } else {
            Some(Fx(q as i64))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rounding, sign, comparison
// ---------------------------------------------------------------------------------------------

impl Fx {
    /// Largest integer value not greater than `self`.
    #[inline]
    pub const fn floor(self) -> Fx {
        Fx(self.0 & !FRAC_MASK)
    }

    /// Smallest integer value not less than `self`.
    #[inline]
    pub const fn ceil(self) -> Fx {
        Fx(self.0.saturating_add(FRAC_MASK) & !FRAC_MASK)
    }

    /// Nearest integer value, with ties resolved toward positive infinity.
    #[inline]
    pub const fn round(self) -> Fx {
        Fx(self.0.saturating_add(ONE_RAW / 2) & !FRAC_MASK)
    }

    /// Fractional part, always in `[0, 1)` — including for negative values.
    #[inline]
    pub const fn frac(self) -> Fx {
        Fx(self.0 & FRAC_MASK)
    }

    /// Truncates toward negative infinity and returns the integer part.
    #[inline]
    pub const fn to_int_floor(self) -> i64 {
        self.0 >> FRAC_BITS
    }

    /// Truncates toward zero and returns the integer part.
    ///
    /// Negation happens in 128 bits so that [`Fx::MIN`] does not overflow.
    #[inline]
    pub const fn to_int_trunc(self) -> i64 {
        if self.0 >= 0 {
            self.0 >> FRAC_BITS
        } else {
            -(((-(self.0 as i128)) >> FRAC_BITS) as i64)
        }
    }

    /// Saturating absolute value.
    #[inline]
    pub const fn abs(self) -> Fx {
        if self.0 < 0 {
            Fx(self.0.saturating_neg())
        } else {
            self
        }
    }

    /// Returns `-1`, `0` or `1` as an `Fx`.
    #[inline]
    pub const fn signum(self) -> Fx {
        if self.0 > 0 {
            Fx::ONE
        } else if self.0 < 0 {
            Fx::NEG_ONE
        } else {
            Fx::ZERO
        }
    }

    /// Returns the smaller of two values.
    #[inline]
    pub const fn min(self, rhs: Fx) -> Fx {
        if self.0 < rhs.0 {
            self
        } else {
            rhs
        }
    }

    /// Returns the larger of two values.
    #[inline]
    pub const fn max(self, rhs: Fx) -> Fx {
        if self.0 > rhs.0 {
            self
        } else {
            rhs
        }
    }

    /// Clamps into `[lo, hi]`. If `lo > hi`, returns `lo`.
    #[inline]
    pub const fn clamp(self, lo: Fx, hi: Fx) -> Fx {
        if self.0 < lo.0 {
            lo
        } else if self.0 > hi.0 {
            hi
        } else {
            self
        }
    }

    /// True if the value is exactly zero.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Linear interpolation. `t` is not clamped, so values outside `[0, 1]` extrapolate.
    #[inline]
    pub const fn lerp(self, to: Fx, t: Fx) -> Fx {
        self.add(to.sub(self).mul(t))
    }
}

// ---------------------------------------------------------------------------------------------
// Transcendentals
// ---------------------------------------------------------------------------------------------

/// Exact floor of the integer square root, by the bit-by-bit restoring method.
///
/// Exactness matters: an uncorrected Newton iteration is off by one for some inputs, and a
/// one-bit difference between two peers is a desync. See `docs/spec/fixed-point.md` Appendix A.
const fn isqrt_u128(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    // Largest power of four not exceeding `n`.
    let mut bit: u128 = 1u128 << (126 - (n.leading_zeros() & !1));
    let mut x: u128 = 0;
    let mut rem: u128 = n;
    while bit != 0 {
        if rem >= x + bit {
            rem -= x + bit;
            x = (x >> 1) + bit;
        } else {
            x >>= 1;
        }
        bit >>= 2;
    }
    x
}

impl Fx {
    /// Square root. Negative inputs return zero rather than trapping.
    #[inline]
    pub const fn sqrt(self) -> Fx {
        if self.0 <= 0 {
            return Fx::ZERO;
        }
        Fx(isqrt_u128((self.0 as u128) << FRAC_BITS) as i64)
    }

    /// Sine of an angle in radians.
    ///
    /// Maximum absolute error is below 3e-7. Accurate over the full input range: the angle is
    /// reduced to a fractional turn by masking, which handles negative and large-magnitude inputs
    /// without a branch.
    pub fn sin(self) -> Fx {
        let turns = self.mul(Fx::INV_TAU);
        // Masking a two's-complement value with 2^32 - 1 yields `turns mod 1`, which is the
        // positive fractional turn even when `turns` is negative. Adding a sign branch here would
        // be a divergence risk for no benefit.
        let f = (turns.0 as u64) & 0xFFFF_FFFF;
        let idx = (f >> tables::SIN_FRAC_BITS) as usize;
        let rem = (f & ((1 << tables::SIN_FRAC_BITS) - 1)) as i128;
        let lo = tables::SIN[idx] as i128;
        let hi = tables::SIN[idx + 1] as i128;
        Fx(sat(lo + (((hi - lo) * rem) >> tables::SIN_FRAC_BITS)))
    }

    /// Cosine of an angle in radians.
    #[inline]
    pub fn cos(self) -> Fx {
        self.add(Fx::FRAC_PI_2).sin()
    }

    /// Tangent of an angle in radians. Saturates near the poles rather than trapping.
    #[inline]
    pub fn tan(self) -> Fx {
        self.sin().div(self.cos())
    }

    /// Four-quadrant arctangent, in radians over `(-π, π]`.
    ///
    /// `atan2(0, 0)` is defined as zero rather than being an error, which makes normalising a
    /// degenerate direction total.
    pub fn atan2(y: Fx, x: Fx) -> Fx {
        if y.0 == 0 && x.0 == 0 {
            return Fx::ZERO;
        }
        let ay = y.abs();
        let ax = x.abs();
        // Reduce to a ratio in [0, 1] so a single octant of table covers every input.
        let (ratio, swapped) = if ay.0 <= ax.0 {
            (ay.div(ax), false)
        } else {
            (ax.div(ay), true)
        };

        let r = ratio.0 as u64;
        let idx = (r >> tables::ATAN_FRAC_BITS) as usize;
        let rem = (r & ((1 << tables::ATAN_FRAC_BITS) - 1)) as i128;
        let lo = tables::ATAN[idx] as i128;
        // `idx` reaches ATAN_STEPS when |y| == |x|; the table carries a sentinel so this read is
        // in bounds. See mistakes.md.
        let hi = tables::ATAN[idx + 1] as i128;
        let mut a = Fx(sat(lo + (((hi - lo) * rem) >> tables::ATAN_FRAC_BITS)));

        if swapped {
            a = Fx::FRAC_PI_2.sub(a);
        }
        if x.0 < 0 {
            a = Fx::PI.sub(a);
        }
        if y.0 < 0 {
            a = a.neg();
        }
        a
    }

    /// Base-2 exponential.
    pub fn exp2(self) -> Fx {
        let n = self.to_int_floor();
        if n >= 31 {
            return Fx::MAX;
        }
        if n < -63 {
            return Fx::ZERO;
        }
        let f = self.frac().0 as u64;
        let idx = (f >> tables::EXP_FRAC_BITS) as usize;
        let rem = (f & ((1 << tables::EXP_FRAC_BITS) - 1)) as i128;
        let lo = tables::EXP2[idx] as i128;
        let hi = tables::EXP2[idx + 1] as i128;
        let mantissa = lo + (((hi - lo) * rem) >> tables::EXP_FRAC_BITS);
        Fx(sat(if n >= 0 {
            mantissa << n
        } else {
            mantissa >> (-n)
        }))
    }

    /// Base-2 logarithm. Non-positive inputs return [`Fx::MIN`].
    pub fn log2(self) -> Fx {
        if self.0 <= 0 {
            return Fx::MIN;
        }
        let raw = self.0 as u64;
        let msb = 63 - raw.leading_zeros();
        let int_part = Fx::from_int(msb as i32 - FRAC_BITS as i32);

        // Normalise so the leading one sits at bit 63; the next ten bits index the table and the
        // twenty-two below that are the interpolation weight.
        let m = raw << (63 - msb);
        let idx = ((m >> 53) & 0x3FF) as usize;
        let rem = ((m >> 31) & ((1 << tables::EXP_FRAC_BITS) - 1)) as i128;
        let lo = tables::LOG2[idx] as i128;
        let hi = tables::LOG2[idx + 1] as i128;
        let frac_part = Fx(sat(lo + (((hi - lo) * rem) >> tables::EXP_FRAC_BITS)));

        int_part.add(frac_part)
    }

    /// Natural exponential.
    #[inline]
    pub fn exp(self) -> Fx {
        self.mul(Fx::LOG2_E).exp2()
    }

    /// Natural logarithm. Non-positive inputs return [`Fx::MIN`].
    #[inline]
    pub fn ln(self) -> Fx {
        if self.0 <= 0 {
            return Fx::MIN;
        }
        self.log2().mul(Fx::LN_2)
    }

    /// `self` raised to the power `e`. Defined for positive `self` only; otherwise returns zero.
    #[inline]
    pub fn powf(self, e: Fx) -> Fx {
        if self.0 <= 0 {
            return Fx::ZERO;
        }
        self.log2().mul(e).exp2()
    }
}

// ---------------------------------------------------------------------------------------------
// Operator sugar
// ---------------------------------------------------------------------------------------------

// The inherent methods above share names with the operator traits below. Rust resolves inherent
// items first, so `Fx::add(a, b)` inside a trait impl would in fact call the inherent method — but
// relying on that is fragile and reads as accidental recursion. The macro therefore expands to the
// operation itself, keeping both definitions obviously independent.
macro_rules! binop {
    ($trait:ident, $method:ident, $assign_trait:ident, $assign_method:ident, $body:expr) => {
        impl $trait for Fx {
            type Output = Fx;
            #[inline]
            fn $method(self, rhs: Fx) -> Fx {
                #[allow(clippy::redundant_closure_call)]
                ($body)(self, rhs)
            }
        }
        impl $assign_trait for Fx {
            #[inline]
            fn $assign_method(&mut self, rhs: Fx) {
                #[allow(clippy::redundant_closure_call)]
                {
                    *self = ($body)(*self, rhs);
                }
            }
        }
    };
}

binop!(Add, add, AddAssign, add_assign, |a: Fx, b: Fx| Fx(a
    .0
    .saturating_add(b.0)));
binop!(Sub, sub, SubAssign, sub_assign, |a: Fx, b: Fx| Fx(a
    .0
    .saturating_sub(b.0)));
binop!(Mul, mul, MulAssign, mul_assign, |a: Fx, b: Fx| Fx(sat(
    (a.0 as i128 * b.0 as i128) >> FRAC_BITS
)));
binop!(Div, div, DivAssign, div_assign, |a: Fx, b: Fx| {
    if b.0 == 0 {
        if a.0 > 0 {
            Fx::MAX
        } else if a.0 < 0 {
            Fx::MIN
        } else {
            Fx::ZERO
        }
    } else {
        Fx(sat(((a.0 as i128) << FRAC_BITS) / b.0 as i128))
    }
});

impl Rem for Fx {
    type Output = Fx;
    #[inline]
    fn rem(self, rhs: Fx) -> Fx {
        if rhs.0 == 0 {
            Fx::ZERO
        } else {
            Fx(self.0 % rhs.0)
        }
    }
}

impl Neg for Fx {
    type Output = Fx;
    #[inline]
    fn neg(self) -> Fx {
        Fx(self.0.saturating_neg())
    }
}

impl fmt::Debug for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fx({} = 0x{:016X})", self.to_f64_lossy(), self.0)
    }
}

impl fmt::Display for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_f64_lossy())
    }
}

impl From<i32> for Fx {
    #[inline]
    fn from(v: i32) -> Fx {
        Fx::from_int(v)
    }
}
