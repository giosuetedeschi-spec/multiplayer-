//! Committed lookup tables for the transcendental functions.
//!
//! These are *data*, not computation. They are generated once by `tempo-tablegen`, committed to the
//! repository as raw little-endian `i64` arrays, and decoded here at compile time. Every language
//! implementation loads the same bytes, which is what makes `sin`, `atan2`, `exp2` and `log2`
//! bit-identical everywhere — see `docs/spec/fixed-point.md` §2 and ADR-0002.
//!
//! Because the tables are decoded in a `const` context, a truncated or corrupt file is a compile
//! error rather than a runtime surprise.

/// Decodes `N` little-endian `i64` values from a byte array at compile time.
///
/// Panics at compile time if `bytes` is not exactly `N * 8` long.
const fn decode<const N: usize>(bytes: &[u8]) -> [i64; N] {
    assert!(bytes.len() == N * 8, "table file has the wrong length");
    let mut out = [0i64; N];
    let mut i = 0;
    while i < N {
        let b = i * 8;
        out[i] = i64::from_le_bytes([
            bytes[b],
            bytes[b + 1],
            bytes[b + 2],
            bytes[b + 3],
            bytes[b + 4],
            bytes[b + 5],
            bytes[b + 6],
            bytes[b + 7],
        ]);
        i += 1;
    }
    out
}

/// Number of sine table entries covering one full turn. Entry `SIN_STEPS` duplicates entry 0.
pub(crate) const SIN_STEPS: usize = 4096;
/// Number of interpolation bits below a sine table index.
pub(crate) const SIN_FRAC_BITS: u32 = 20;

/// `sin(2π · i / 4096)` in Q32.32, with a duplicate wraparound sentinel at index 4096.
pub(crate) static SIN: [i64; SIN_STEPS + 1] =
    decode(include_bytes!("../../../conformance/tables/sin.bin"));

/// Number of `atan` table entries covering the ratio range `[0, 1]`.
pub(crate) const ATAN_STEPS: usize = 1024;
/// Number of interpolation bits below an `atan` table index.
pub(crate) const ATAN_FRAC_BITS: u32 = 22;

/// `atan(i / 1024)` in Q32.32.
///
/// Carries **two** sentinel slots beyond `ATAN_STEPS`: the ratio reaches exactly `1.0` when
/// `|y| == |x|` (45 degrees), producing index 1024 and an interpolation read of index 1025. See
/// `mistakes.md` — this was a genuine out-of-bounds read in the first draft of the spec.
pub(crate) static ATAN: [i64; ATAN_STEPS + 2] =
    decode(include_bytes!("../../../conformance/tables/atan.bin"));

/// Number of `exp2`/`log2` table entries.
pub(crate) const EXP_STEPS: usize = 1024;
/// Number of interpolation bits below an `exp2`/`log2` table index.
pub(crate) const EXP_FRAC_BITS: u32 = 22;

/// `2^(i / 1024)` in Q32.32, over `[1, 2]`.
pub(crate) static EXP2: [i64; EXP_STEPS + 1] =
    decode(include_bytes!("../../../conformance/tables/exp2.bin"));

/// `log2(1 + i / 1024)` in Q32.32, over `[0, 1]`.
pub(crate) static LOG2: [i64; EXP_STEPS + 1] =
    decode(include_bytes!("../../../conformance/tables/log2.bin"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_table_wraps() {
        assert_eq!(SIN[0], 0);
        assert_eq!(SIN[SIN_STEPS], SIN[0], "sentinel must duplicate entry 0");
        assert_eq!(
            SIN[SIN_STEPS / 4],
            1i64 << 32,
            "sin(pi/2) must be exactly 1"
        );
        assert_eq!(SIN[SIN_STEPS / 2], 0, "sin(pi) must be exactly 0");
    }

    #[test]
    fn atan_table_has_interpolation_sentinel() {
        assert_eq!(ATAN[0], 0);
        assert_eq!(
            ATAN[ATAN_STEPS + 1],
            ATAN[ATAN_STEPS],
            "the 45-degree case indexes ATAN_STEPS and interpolates against ATAN_STEPS + 1"
        );
    }

    #[test]
    fn exp_tables_have_exact_endpoints() {
        assert_eq!(EXP2[0], 1i64 << 32, "2^0 == 1");
        assert_eq!(EXP2[EXP_STEPS], 2i64 << 32, "2^1 == 2");
        assert_eq!(LOG2[0], 0, "log2(1) == 0");
        assert_eq!(LOG2[EXP_STEPS], 1i64 << 32, "log2(2) == 1");
    }
}
