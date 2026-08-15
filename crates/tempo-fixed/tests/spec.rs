//! Tests that check `tempo-fixed` against `docs/spec/fixed-point.md`.
//!
//! These deliberately assert on *specified behaviour*, not on whatever the implementation happens
//! to do. Where the spec pins an exact raw value, the test names that raw value rather than
//! recomputing it with the code under test — a test that calls the implementation to derive its own
//! expectation proves only self-consistency.

use tempo_fixed::{Fx, Quat, Vec2, Vec3};

/// Largest tolerated deviation when comparing a transcendental against `f64`.
const TRANSCENDENTAL_TOLERANCE: f64 = 3e-7;

// ---------------------------------------------------------------------------------------------
// §1.1 Constants
// ---------------------------------------------------------------------------------------------

#[test]
fn constants_match_the_specified_raw_values() {
    // Spec §1.1. Written as literals, not computed, because these are the values every language
    // must agree on. Five of these were wrong in the first draft of the spec — see mistakes.md.
    assert_eq!(Fx::ONE.raw(), 0x0000_0001_0000_0000);
    assert_eq!(Fx::HALF.raw(), 0x0000_0000_8000_0000);
    assert_eq!(Fx::PI.raw(), 0x0000_0003_243F_6A89);
    assert_eq!(Fx::TAU.raw(), 0x0000_0006_487E_D511);
    assert_eq!(Fx::FRAC_PI_2.raw(), 0x0000_0001_921F_B544);
    assert_eq!(Fx::INV_TAU.raw(), 0x0000_0000_28BE_60DC);
    assert_eq!(Fx::E.raw(), 0x0000_0002_B7E1_5163);
    assert_eq!(Fx::LOG2_E.raw(), 0x0000_0001_7154_7653);
    assert_eq!(Fx::LN_2.raw(), 0x0000_0000_B172_17F8);
}

#[test]
fn constants_are_the_nearest_representable_value() {
    // Independent cross-check: each constant must be within half an ULP of the true value.
    for (raw, truth) in [
        (Fx::PI, std::f64::consts::PI),
        (Fx::TAU, std::f64::consts::TAU),
        (Fx::FRAC_PI_2, std::f64::consts::FRAC_PI_2),
        (Fx::INV_TAU, 1.0 / std::f64::consts::TAU),
        (Fx::E, std::f64::consts::E),
        (Fx::LOG2_E, std::f64::consts::LOG2_E),
        (Fx::LN_2, std::f64::consts::LN_2),
        (Fx::SQRT_2, std::f64::consts::SQRT_2),
        (Fx::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2),
    ] {
        let ulp = 1.0 / 4_294_967_296.0;
        let err = (raw.to_f64_lossy() - truth).abs();
        assert!(err <= ulp / 2.0, "{raw:?} differs from {truth} by {err}");
    }
}

// ---------------------------------------------------------------------------------------------
// §1.2–1.3 Rounding, overflow, arithmetic
// ---------------------------------------------------------------------------------------------

#[test]
fn mul_rounds_toward_negative_infinity() {
    // The spec's central asymmetry: mul reduces via an arithmetic shift, so it floors.
    // 1 ULP times one half is half a ULP, which floors to zero going up and to -1 going down.
    assert_eq!(Fx::EPSILON.mul(Fx::HALF).raw(), 0);
    assert_eq!(Fx::EPSILON.neg().mul(Fx::HALF).raw(), -1);
}

#[test]
fn div_truncates_toward_zero() {
    // ...while div truncates toward zero, because that is what integer division does in every
    // target language. Both directions must round to zero, not to -1.
    assert_eq!(Fx::EPSILON.div(Fx::from_int(2)).raw(), 0);
    assert_eq!(Fx::EPSILON.neg().div(Fx::from_int(2)).raw(), 0);
}

#[test]
fn division_by_zero_is_defined_not_trapping() {
    assert_eq!(Fx::ONE.div(Fx::ZERO), Fx::MAX);
    assert_eq!(Fx::NEG_ONE.div(Fx::ZERO), Fx::MIN);
    assert_eq!(Fx::ZERO.div(Fx::ZERO), Fx::ZERO);
    assert_eq!(Fx::ONE.rem(Fx::ZERO), Fx::ZERO);
}

#[test]
fn arithmetic_saturates_rather_than_wrapping() {
    assert_eq!(Fx::MAX.add(Fx::ONE), Fx::MAX);
    assert_eq!(Fx::MIN.sub(Fx::ONE), Fx::MIN);
    assert_eq!(Fx::MAX.mul(Fx::from_int(2)), Fx::MAX);
    assert_eq!(Fx::MIN.mul(Fx::from_int(2)), Fx::MIN);
    // Negating MIN cannot be represented, so it saturates to MAX.
    assert_eq!(Fx::MIN.neg(), Fx::MAX);
    assert_eq!(Fx::MIN.abs(), Fx::MAX);
}

#[test]
fn checked_variants_report_overflow() {
    assert_eq!(Fx::MAX.checked_add(Fx::ONE), None);
    assert_eq!(Fx::ONE.checked_add(Fx::ONE), Some(Fx::from_int(2)));
    assert_eq!(Fx::ONE.checked_div(Fx::ZERO), None);
}

#[test]
fn rounding_helpers_handle_negatives() {
    let neg = Fx::from_ratio(-3, 2); // -1.5
    assert_eq!(neg.floor(), Fx::from_int(-2));
    assert_eq!(neg.ceil(), Fx::NEG_ONE);
    assert_eq!(neg.to_int_floor(), -2);
    assert_eq!(neg.to_int_trunc(), -1);
    // frac is always in [0, 1), including for negatives: -1.5 - floor(-1.5) = 0.5
    assert_eq!(neg.frac(), Fx::HALF);

    // Ties round toward positive infinity.
    assert_eq!(Fx::HALF.round(), Fx::ONE);
    assert_eq!(Fx::HALF.neg().round(), Fx::ZERO);
}

#[test]
fn to_int_trunc_does_not_overflow_at_min() {
    // Negating MIN in 64 bits would overflow; the implementation must widen first.
    assert_eq!(Fx::MIN.to_int_trunc(), -2_147_483_648);
    assert_eq!(Fx::MIN.to_int_floor(), -2_147_483_648);
}

#[test]
fn from_ratio_is_exact() {
    assert_eq!(Fx::from_ratio(1, 2), Fx::HALF);
    assert_eq!(Fx::from_ratio(-1, 2), Fx::HALF.neg());
    assert_eq!(Fx::from_ratio(1, 0), Fx::MAX);
    assert_eq!(Fx::from_ratio(0, 0), Fx::ZERO);
    // 1/3 is not representable; the result must be the truncation, and must be stable.
    assert_eq!(Fx::from_ratio(1, 3).raw(), 0x5555_5555);
}

// ---------------------------------------------------------------------------------------------
// §2 Transcendentals
// ---------------------------------------------------------------------------------------------

#[test]
fn sqrt_is_the_exact_floor() {
    assert_eq!(Fx::ZERO.sqrt(), Fx::ZERO);
    assert_eq!(Fx::ONE.sqrt(), Fx::ONE);
    assert_eq!(Fx::from_int(4).sqrt(), Fx::from_int(2));
    assert_eq!(Fx::from_int(144).sqrt(), Fx::from_int(12));
    // Negative inputs return zero rather than trapping.
    assert_eq!(Fx::NEG_ONE.sqrt(), Fx::ZERO);
}

#[test]
fn sqrt_floors_where_constants_round() {
    // sqrt is specified as the exact *floor* (§2.1), while the committed constants are the
    // *nearest* representable value (§1.1). For an irrational result the two differ by up to one
    // ULP, so sqrt(2) is deliberately one ULP below Fx::SQRT_2. Asserting equality here would be
    // asserting against the spec.
    let computed = Fx::from_int(2).sqrt();
    assert!(
        computed.raw() == Fx::SQRT_2.raw() || computed.raw() == Fx::SQRT_2.raw() - 1,
        "sqrt(2) = {computed:?} should be at or one ULP below SQRT_2 = {:?}",
        Fx::SQRT_2
    );
    assert!(
        computed.raw() < Fx::SQRT_2.raw(),
        "sqrt truncates, so this case floors"
    );
}

#[test]
fn sqrt_satisfies_its_defining_inequality() {
    // The exact property from §2.1, stated on raw integers: sqrt returns the largest r with
    // r² <= raw · 2^32. Expressing it in Fx space instead would reintroduce the very truncation
    // being tested and make the check vacuous.
    for raw in [1i64, 7, 1 << 16, 1 << 32, (1 << 32) + 1, 1 << 40, i64::MAX] {
        let r = Fx::from_raw(raw).sqrt().raw() as i128;
        let n = (raw as i128) << 32;
        assert!(r * r <= n, "sqrt({raw}) = {r} is too large");
        assert!((r + 1) * (r + 1) > n, "sqrt({raw}) = {r} is too small");
    }
}

#[test]
fn sin_and_cos_are_accurate_across_the_full_range() {
    // Includes negative and large-magnitude angles, which exercise the masking-based range
    // reduction. A sign branch here would be a divergence risk, so its absence is load-bearing.
    let mut angle = -40.0f64;
    while angle < 40.0 {
        let fx = Fx::from_f64_lossy(angle);
        assert!(
            (fx.sin().to_f64_lossy() - angle.sin()).abs() < TRANSCENDENTAL_TOLERANCE,
            "sin({angle}) was {}",
            fx.sin()
        );
        assert!(
            (fx.cos().to_f64_lossy() - angle.cos()).abs() < TRANSCENDENTAL_TOLERANCE,
            "cos({angle}) was {}",
            fx.cos()
        );
        angle += 0.017;
    }
}

#[test]
fn sin_is_exact_at_table_entries() {
    assert_eq!(Fx::ZERO.sin(), Fx::ZERO);
    assert_eq!(Fx::FRAC_PI_2.sin().raw(), Fx::ONE.raw());
    assert_eq!(Fx::ZERO.cos().raw(), Fx::ONE.raw());
}

#[test]
fn atan2_handles_the_forty_five_degree_case() {
    // |y| == |x| drives the ratio to exactly 1.0, which indexes the last real table entry and
    // interpolates against the sentinel. This read was out of bounds in the first draft of the
    // spec, and 45 degrees is one of the most common angles in any game. See mistakes.md.
    let a = Fx::atan2(Fx::ONE, Fx::ONE);
    let expected = std::f64::consts::FRAC_PI_4;
    assert!(
        (a.to_f64_lossy() - expected).abs() < TRANSCENDENTAL_TOLERANCE,
        "atan2(1,1) was {a}"
    );

    for (y, x) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
        let got = Fx::atan2(Fx::from_int(y), Fx::from_int(x)).to_f64_lossy();
        let want = (y as f64).atan2(x as f64);
        assert!(
            (got - want).abs() < TRANSCENDENTAL_TOLERANCE,
            "atan2({y},{x}) was {got}"
        );
    }
}

#[test]
fn atan2_covers_all_quadrants_and_axes() {
    assert_eq!(
        Fx::atan2(Fx::ZERO, Fx::ZERO),
        Fx::ZERO,
        "defined, not an error"
    );
    assert_eq!(Fx::atan2(Fx::ZERO, Fx::ONE), Fx::ZERO);
    assert_eq!(Fx::atan2(Fx::ONE, Fx::ZERO), Fx::FRAC_PI_2);
    assert_eq!(Fx::atan2(Fx::NEG_ONE, Fx::ZERO), Fx::FRAC_PI_2.neg());
    assert_eq!(Fx::atan2(Fx::ZERO, Fx::NEG_ONE), Fx::PI);

    let mut angle = -3.10f64;
    while angle < 3.10 {
        let (y, x) = (
            Fx::from_f64_lossy(angle.sin()),
            Fx::from_f64_lossy(angle.cos()),
        );
        let got = Fx::atan2(y, x).to_f64_lossy();
        assert!(
            (got - angle).abs() < 1e-5,
            "atan2 round trip at {angle} gave {got}"
        );
        angle += 0.031;
    }
}

#[test]
fn exp2_and_log2_are_accurate() {
    assert_eq!(Fx::ZERO.exp2(), Fx::ONE);
    assert_eq!(Fx::ONE.exp2(), Fx::from_int(2));
    assert_eq!(Fx::ONE.log2(), Fx::ZERO);
    assert_eq!(Fx::from_int(2).log2(), Fx::ONE);
    assert_eq!(Fx::from_int(1024).log2(), Fx::from_int(10));

    // Non-positive inputs return MIN rather than trapping.
    assert_eq!(Fx::ZERO.log2(), Fx::MIN);
    assert_eq!(Fx::NEG_ONE.ln(), Fx::MIN);

    let mut v = 0.02f64;
    while v < 1000.0 {
        let got = Fx::from_f64_lossy(v).log2().to_f64_lossy();
        assert!((got - v.log2()).abs() < 1e-5, "log2({v}) was {got}");
        v *= 1.37;
    }

    let mut e = -20.0f64;
    while e < 20.0 {
        let got = Fx::from_f64_lossy(e).exp2().to_f64_lossy();
        let want = e.exp2();
        assert!(
            (got - want).abs() <= want.abs() * 1e-5 + 1e-9,
            "exp2({e}) was {got}"
        );
        e += 0.31;
    }
}

#[test]
fn exp2_saturates_instead_of_shifting_out_of_range() {
    assert_eq!(Fx::from_int(100).exp2(), Fx::MAX);
    assert_eq!(Fx::from_int(-100).exp2(), Fx::ZERO);
    assert_eq!(Fx::from_int(-100).exp(), Fx::ZERO);
}

#[test]
fn ln_and_exp_round_trip() {
    let mut v = 0.05f64;
    while v < 100.0 {
        let got = Fx::from_f64_lossy(v).ln().exp().to_f64_lossy();
        assert!((got - v).abs() <= v * 1e-4, "exp(ln({v})) was {got}");
        v *= 1.53;
    }
}

// ---------------------------------------------------------------------------------------------
// §3 Vectors and quaternions
// ---------------------------------------------------------------------------------------------

#[test]
fn vector_basics() {
    let a = Vec2::from_ints(3, 4);
    assert_eq!(a.length(), Fx::from_int(5));
    assert_eq!(a.length_sq(), Fx::from_int(25));
    assert_eq!(a.dot(Vec2::X), Fx::from_int(3));
    assert_eq!(Vec2::X.perp(), Vec2::Y);

    let b = Vec3::from_ints(1, 2, 2);
    assert_eq!(b.length(), Fx::from_int(3));
    assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
}

#[test]
fn normalising_zero_is_total() {
    // Making these total rather than undefined removes an entire class of divergence: an
    // implementation that trapped here would desync against one that returned a sentinel.
    assert_eq!(Vec2::ZERO.normalize(), Vec2::ZERO);
    assert_eq!(Vec3::ZERO.normalize(), Vec3::ZERO);
    assert_eq!(
        Quat::new(Fx::ZERO, Fx::ZERO, Fx::ZERO, Fx::ZERO).normalize(),
        Quat::IDENTITY
    );
}

#[test]
fn normalised_vectors_are_unit_length() {
    for (x, y) in [(3, 4), (-7, 24), (1, 1), (100, -1)] {
        let n = Vec2::from_ints(x, y).normalize();
        let err = (n.length().to_f64_lossy() - 1.0).abs();
        assert!(err < 1e-6, "normalize({x},{y}) had length {}", n.length());
    }
}

#[test]
fn quaternion_identity_and_conjugate() {
    let q = Quat::from_axis_angle(Vec3::Z, Fx::FRAC_PI_2);
    assert_eq!(Quat::IDENTITY.mul(q), q);

    // q * conj(q) is the identity for a unit quaternion, within quantisation error.
    let r = q.mul(q.conjugate());
    assert!((r.w.to_f64_lossy() - 1.0).abs() < 1e-6, "{r:?}");
}

#[test]
fn quaternion_rotation_matches_expectation() {
    // A quarter turn about Z takes +X to +Y.
    let q = Quat::from_axis_angle(Vec3::Z, Fx::FRAC_PI_2);
    let v = q.rotate(Vec3::X);
    assert!(v.x.to_f64_lossy().abs() < 1e-5, "{v:?}");
    assert!((v.y.to_f64_lossy() - 1.0).abs() < 1e-5, "{v:?}");
    assert!(v.z.to_f64_lossy().abs() < 1e-5, "{v:?}");
}

#[test]
fn rotate_by_angle_matches_trig() {
    let v = Vec2::from_ints(1, 0).rotate(Fx::FRAC_PI_2);
    assert!(v.x.to_f64_lossy().abs() < 1e-6, "{v:?}");
    assert!((v.y.to_f64_lossy() - 1.0).abs() < 1e-6, "{v:?}");
}

// ---------------------------------------------------------------------------------------------
// Determinism properties
// ---------------------------------------------------------------------------------------------

#[test]
fn repeated_evaluation_is_identical() {
    // Determinism within a process is necessary but not sufficient; the cross-language guarantee
    // comes from the conformance vectors. This catches accidental use of uninitialised state or
    // ambient inputs such as a clock.
    let inputs: Vec<Fx> = (0..500).map(|i| Fx::from_ratio(i - 250, 17)).collect();
    let run = || -> Vec<i64> {
        inputs
            .iter()
            .map(|&x| {
                x.sin()
                    .mul(x.cos())
                    .add(x.abs().sqrt())
                    .add(x.abs().add(Fx::ONE).log2())
                    .raw()
            })
            .collect()
    };
    assert_eq!(run(), run());
}

#[test]
fn simulation_step_is_reproducible_from_a_saved_state() {
    // A miniature of the property the whole engine depends on: simulating forward from a restored
    // state must produce exactly what simulating straight through produced. This is rollback,
    // reduced to two integers.
    fn step(pos: Vec2, vel: Vec2, dt: Fx) -> (Vec2, Vec2) {
        let gravity = Vec2::new(Fx::ZERO, Fx::from_ratio(-98, 10));
        let vel = vel.add(gravity.scale(dt));
        (pos.add(vel.scale(dt)), vel)
    }

    let dt = Fx::from_ratio(1, 60);
    let mut pos = Vec2::from_ints(0, 100);
    let mut vel = Vec2::from_ints(5, 0);

    let mut saved = None;
    let mut straight_through = Vec::new();
    for tick in 0..240 {
        if tick == 100 {
            saved = Some((pos, vel));
        }
        let (p, v) = step(pos, vel, dt);
        pos = p;
        vel = v;
        if tick >= 100 {
            straight_through.push((pos.x.raw(), pos.y.raw()));
        }
    }

    let (mut rpos, mut rvel) = saved.expect("state saved at tick 100");
    let mut replayed = Vec::new();
    for _ in 100..240 {
        let (p, v) = step(rpos, rvel, dt);
        rpos = p;
        rvel = v;
        replayed.push((rpos.x.raw(), rpos.y.raw()));
    }

    assert_eq!(
        straight_through, replayed,
        "re-simulation diverged from the original run"
    );
}
