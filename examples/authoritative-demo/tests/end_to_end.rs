//! End-to-end properties of the P1 vertical slice.
//!
//! These assert on *behaviour under adverse conditions*, which is the only kind of test that
//! catches the failures this stack is prone to. Every bug found while building this demo — the
//! missing client lead, the stale-snapshot cascade, the retransmission resonance — passed every
//! unit test in the workspace and was caught here.

use authoritative_demo::{run_session, Report, TICK_HZ};
use tempo_transport::LinkConditions;

const TICKS: u32 = TICK_HZ as u32 * 10;
const SEED: u64 = 0xC0FFEE;

fn all_conditions() -> Vec<(&'static str, LinkConditions)> {
    vec![
        ("perfect", LinkConditions::PERFECT),
        ("lan", LinkConditions::LAN),
        ("broadband", LinkConditions::BROADBAND),
        ("poor wifi", LinkConditions::POOR_WIFI),
        ("mobile", LinkConditions::MOBILE),
    ]
}

#[test]
fn prediction_holds_under_every_network_condition() {
    // The headline claim. A client whose prediction agreed with the server never has to correct,
    // so the player never sees their own character snap — even at 300 ms round trip with 10% loss.
    for (name, conditions) in all_conditions() {
        let r: Report = run_session(conditions, SEED, TICKS);
        assert_eq!(
            r.corrections, 0,
            "{name}: prediction diverged {} times; client and server simulations disagree",
            r.corrections
        );
        assert_eq!(
            r.resyncs, 0,
            "{name}: had to abandon prediction {} times",
            r.resyncs
        );
    }
}

#[test]
fn the_client_lead_grows_with_latency() {
    // The lead is derived from measured latency and jitter, not configured. A worse link must ask
    // for more lead, or inputs arrive after the server has passed the tick they belong to.
    let mut previous = 0;
    for (name, conditions) in all_conditions() {
        let r = run_session(conditions, SEED, TICKS);
        assert!(
            r.lead_ticks >= previous,
            "{name}: lead {} is less than the better link's {previous}",
            r.lead_ticks
        );
        previous = r.lead_ticks;
    }
    assert!(
        previous > 5,
        "a 300 ms link should need a substantial lead, got {previous}"
    );
}

#[test]
fn input_starvation_is_confined_to_startup() {
    // The server should never simulate a tick without the client's input once the session is
    // running. The only unavoidable gap is the first `lead` ticks, before the earliest input can
    // possibly have arrived. Anything beyond that means loss is reaching the server, which input
    // redundancy exists to prevent.
    for (name, conditions) in all_conditions() {
        let r = run_session(conditions, SEED, TICKS);
        assert!(
            r.input_starved <= r.lead_ticks,
            "{name}: starved on {} ticks with a lead of {}; redundancy is not covering loss",
            r.input_starved,
            r.lead_ticks
        );
    }
}

#[test]
fn snapshots_stay_far_below_the_mtu() {
    // A snapshot that does not fit cannot be sent at all, since snapshots are never fragmented.
    for (name, conditions) in all_conditions() {
        let r = run_session(conditions, SEED, TICKS);
        assert!(
            r.largest_snapshot < 200,
            "{name}: largest snapshot was {} bytes",
            r.largest_snapshot
        );
        assert!(
            r.mean_snapshot_bytes() < 64.0,
            "{name}: mean {}",
            r.mean_snapshot_bytes()
        );
    }
}

#[test]
fn delivery_tracks_the_configured_loss_rate() {
    // Confirms the simulated link is actually doing what it claims, rather than the session
    // quietly running on a perfect connection.
    let perfect = run_session(LinkConditions::PERFECT, SEED, TICKS);
    assert_eq!(perfect.delivery_percent(), 100.0);

    let mobile = run_session(LinkConditions::MOBILE, SEED, TICKS);
    assert!(
        (80.0..95.0).contains(&mobile.delivery_percent()),
        "10% configured loss should deliver roughly 90%, got {}",
        mobile.delivery_percent()
    );
}

#[test]
fn a_session_is_reproducible_from_its_seed() {
    // Determinism end to end: the same seed must produce an identical run, or a failure found in
    // CI could not be reproduced locally.
    let a = run_session(LinkConditions::MOBILE, 1234, TICKS);
    let b = run_session(LinkConditions::MOBILE, 1234, TICKS);
    assert_eq!(a, b);

    // Different seeds must lose a *different set* of packets. Comparing counts would not show
    // this — the server sends the same number of snapshots either way, and two runs can easily
    // drop the same number of different packets. The fingerprint captures which ones arrived.
    let c = run_session(LinkConditions::MOBILE, 5678, TICKS);
    assert_ne!(
        a.delivery_fingerprint, c.delivery_fingerprint,
        "different seeds explored the same loss pattern"
    );
}

#[test]
fn the_session_survives_a_long_run() {
    // Sustained play, not a burst. Catches state that grows without bound and errors that only
    // accumulate — the sequence counters, the ack buffers, the prediction history.
    let r = run_session(LinkConditions::POOR_WIFI, SEED, TICK_HZ as u32 * 120);
    assert_eq!(r.corrections, 0);
    assert_eq!(r.resyncs, 0);
    assert!(r.largest_snapshot < 200);
}
