//! Generates the committed lookup tables and constants used by `tempo-fixed`.
//!
//! This binary is the *only* place floating point is permitted to touch fixed-point values. It
//! runs once, offline, and its output is committed to the repository as raw little-endian `i64`
//! arrays. From then on the tables are data, identical in every language and on every platform,
//! which is what makes the transcendental functions deterministic — see
//! `docs/spec/fixed-point.md` §2 and ADR-0002.
//!
//! Run with: `cargo run -p tempo-tablegen`

use std::fs;
use std::path::Path;

/// Scale factor for Q32.32: `value = raw / 2^32`.
const SCALE: f64 = 4_294_967_296.0;

/// Rounds to the nearest integer with ties going away from zero, then saturates into `i64`.
///
/// Ties-away-from-zero is specified rather than Rust's default so that the rule is stated once and
/// is trivially reproducible by a generator written in any language.
fn to_raw(v: f64) -> i64 {
    let scaled = v * SCALE;
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    if rounded >= i64::MAX as f64 {
        i64::MAX
    } else if rounded <= i64::MIN as f64 {
        i64::MIN
    } else {
        rounded as i64
    }
}

fn write_table(dir: &Path, name: &str, values: &[i64]) {
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let path = dir.join(name);
    fs::write(&path, &bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    println!("{:>12}  {:>6} entries  {:>7} bytes", name, values.len(), bytes.len());
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/tables")
        .canonicalize()
        .unwrap_or_else(|_| {
            let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/tables");
            fs::create_dir_all(&p).expect("creating table directory");
            p.canonicalize().expect("canonicalising table directory")
        });

    // sin over one full turn. 4097 entries so that entry 4096 duplicates entry 0, which removes
    // the wraparound branch from the interpolation path.
    let sin: Vec<i64> = (0..=4096)
        .map(|i| to_raw((std::f64::consts::TAU * i as f64 / 4096.0).sin()))
        .collect();
    write_table(&dir, "sin.bin", &sin);

    // atan over the ratio range [0, 1]; octant reduction handles the rest.
    //
    // 1026 entries, not 1025: the ratio reaches exactly 1.0 whenever |y| == |x| (a 45-degree
    // angle, which is extremely common), giving table index 1024. The interpolation then reads
    // index 1025. Entry 1025 duplicates 1024 so the read is in bounds and contributes zero,
    // exactly as entry 4096 does for the sine table.
    let mut atan: Vec<i64> = (0..=1024)
        .map(|i| to_raw((i as f64 / 1024.0).atan()))
        .collect();
    atan.push(atan[1024]);
    write_table(&dir, "atan.bin", &atan);

    // exp2 over [0, 1); entry 1024 is exactly 2.0 and is used as the interpolation upper bound.
    let exp2: Vec<i64> = (0..=1024)
        .map(|i| to_raw((i as f64 / 1024.0).exp2()))
        .collect();
    write_table(&dir, "exp2.bin", &exp2);

    // log2 over mantissa range [1, 2).
    let log2: Vec<i64> = (0..=1024)
        .map(|i| to_raw((1.0 + i as f64 / 1024.0).log2()))
        .collect();
    write_table(&dir, "log2.bin", &log2);

    // The constants in docs/spec/fixed-point.md §1.1 are committed as exact raw values so that no
    // implementation ever computes them. Printing them here is how that table is kept honest.
    println!("\nconstants (docs/spec/fixed-point.md §1.1):");
    for (name, value) in [
        ("ONE", 1.0),
        ("HALF", 0.5),
        ("PI", std::f64::consts::PI),
        ("TAU", std::f64::consts::TAU),
        ("FRAC_PI_2", std::f64::consts::FRAC_PI_2),
        ("INV_TAU", 1.0 / std::f64::consts::TAU),
        ("E", std::f64::consts::E),
        ("LOG2_E", std::f64::consts::LOG2_E),
        ("LN_2", std::f64::consts::LN_2),
        ("SQRT_2", std::f64::consts::SQRT_2),
        ("FRAC_1_SQRT_2", std::f64::consts::FRAC_1_SQRT_2),
    ] {
        println!("  {name:<14} 0x{:016X}   {value}", to_raw(value));
    }
}
