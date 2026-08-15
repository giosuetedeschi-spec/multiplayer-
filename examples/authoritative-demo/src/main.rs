//! Runs the authoritative-server demo across a range of network conditions and reports.
//!
//! `cargo run -p authoritative-demo`

use authoritative_demo::{run_session, Report, TICK_HZ};
use tempo_transport::LinkConditions;

fn main() {
    let ticks = TICK_HZ as u32 * 10; // ten seconds of play

    println!("tempo — authoritative server demo");
    println!("{ticks} ticks at {TICK_HZ} Hz, one predicting client\n");
    println!(
        "{:<12} {:>8} {:>9} {:>8} {:>9} {:>7} {:>8}",
        "link", "lead", "arrived", "mean B", "corrected", "starved", "resync"
    );
    println!("{}", "-".repeat(68));

    for (name, conditions) in [
        ("perfect", LinkConditions::PERFECT),
        ("lan", LinkConditions::LAN),
        ("broadband", LinkConditions::BROADBAND),
        ("poor wifi", LinkConditions::POOR_WIFI),
        ("mobile", LinkConditions::MOBILE),
    ] {
        let r: Report = run_session(conditions, 0xC0FFEE, ticks);
        println!(
            "{:<12} {:>8} {:>8.1}% {:>8.1} {:>9} {:>7} {:>8}",
            name,
            r.lead_ticks,
            r.delivery_percent(),
            r.mean_snapshot_bytes(),
            r.corrections,
            r.input_starved,
            r.resyncs
        );
    }

    println!("\nReading this table:");
    println!("  lead      — ticks the client ran ahead, derived from measured latency and jitter");
    println!("  arrived   — snapshot delivery; the gap from 100% is simulated packet loss");
    println!("  mean B    — bytes per snapshot, against a 1200 byte MTU");
    println!("  corrected — times the client's prediction disagreed with the server");
    println!(
        "  starved   — ticks the server ran before the client input arrived; lead drives this to 0"
    );
    println!("  resync    — times state was adopted wholesale, having lost the predicted tick");
}
