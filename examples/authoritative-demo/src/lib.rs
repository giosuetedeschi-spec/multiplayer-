//! A complete authoritative-server session, wired end to end.
//!
//! This is the P1 vertical slice: every layer built so far, connected, running a real (if tiny)
//! game over a simulated link. It exists to prove the design works together before any of it is
//! ported to another language — proving it once beats porting a wrong design six times
//! ([ADR-0024](../../../docs/adr/0024-phasing-and-sequencing.md)).
//!
//! What it exercises:
//!
//! - `tempo-fixed` — all movement is deterministic fixed point
//! - `tempo-wire` — quantized, bit-packed encoding
//! - `tempo-core` — the world arena, snapshots, and baseline deltas
//! - `tempo-transport` — the simulated link, with latency, jitter, loss and reordering
//! - `tempo-reliability` — acknowledgements driving baseline selection
//! - `tempo-predict` — client prediction and reconciliation
//!
//! The game is deliberately trivial: players accelerate, drift, and are clamped to an arena. What
//! matters is that a client under 300 ms round trip and 10% loss stays in agreement with the server,
//! and that its own movement responds instantly.

use tempo_core::{apply_delta, encode_delta, ComponentId, Tick, World, WorldSnapshot};
use tempo_fixed::{Fx, Vec2};
use tempo_predict::{ClockSync, Predictor, Reconciliation};
use tempo_reliability::AckTracker;
use tempo_transport::{drain, LinkConditions, MemoryNetwork, PeerId, Timestamp, Transport, MTU};
use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

/// Simulation rate.
pub const TICK_HZ: u64 = 60;
/// Microseconds per tick.
pub const TICK_US: u64 = 1_000_000 / TICK_HZ;
/// How often the server sends state, in ticks. Snapshots are cheaper than simulation.
pub const SNAPSHOT_EVERY: u32 = 3;
/// How many recent inputs each packet repeats.
///
/// Inputs cannot usefully be retransmitted — by the time a retransmission arrived, its tick would
/// have passed — so redundancy is the standard answer instead.
pub const INPUT_REDUNDANCY: usize = 8;

/// Quantization step for positions: one millimetre.
pub const STEP: Fx = Fx::from_raw(0x0041_8937);
/// Arena half-extent.
pub const ARENA: i32 = 100;

const SERVER: PeerId = PeerId(0);

/// One player's input for a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Input {
    /// Horizontal thrust, -1, 0 or 1.
    pub thrust_x: i8,
    /// Vertical thrust, -1, 0 or 1.
    pub thrust_y: i8,
}

impl Input {
    /// Encodes to two bytes.
    pub fn encode(self) -> [u8; 2] {
        [self.thrust_x as u8, self.thrust_y as u8]
    }

    /// Decodes from two bytes.
    pub fn decode(b: [u8; 2]) -> Input {
        Input {
            thrust_x: b[0] as i8,
            thrust_y: b[1] as i8,
        }
    }
}

/// Registers the demo's components and returns the world plus its component id.
pub fn build_world() -> (World, ComponentId) {
    let mut w = World::new();
    let body = w
        .register(ComponentDesc::new(
            "Body",
            vec![
                FieldDesc::new("position", FieldType::Vec2).with_quantize(
                    STEP,
                    Fx::from_int(-ARENA),
                    Fx::from_int(ARENA),
                ),
                FieldDesc::new("velocity", FieldType::Vec2).with_quantize(
                    STEP,
                    Fx::from_int(-50),
                    Fx::from_int(50),
                ),
            ],
        ))
        .expect("the demo schema is valid");
    (w, body)
}

/// Advances one entity by one tick.
///
/// Deliberately the *same* function on client and server. Prediction compares the two, so any
/// difference between them — including a different order of operations — appears as divergence.
pub fn step_entity(world: &mut World, body: ComponentId, entity: tempo_core::Entity, input: Input) {
    let Ok(Value::Vec2(pos)) = world.get_named(entity, body, "position") else {
        return;
    };
    let Ok(Value::Vec2(vel)) = world.get_named(entity, body, "velocity") else {
        return;
    };

    let dt = Fx::from_ratio(1, TICK_HZ as i32);
    let accel = Fx::from_int(60);
    let drag = Fx::from_ratio(97, 100);

    let thrust = Vec2::new(
        Fx::from_int(input.thrust_x as i32).mul(accel),
        Fx::from_int(input.thrust_y as i32).mul(accel),
    );
    let new_vel = vel.add(thrust.scale(dt)).scale(drag);
    let new_pos = pos.add(new_vel.scale(dt));

    // Clamp to the arena. Doing this identically on both sides matters: a client that clamped
    // differently would diverge at the walls and nowhere else, which is a miserable bug to find.
    let limit = Fx::from_int(ARENA);
    let clamped = Vec2::new(
        new_pos.x.clamp(limit.neg(), limit),
        new_pos.y.clamp(limit.neg(), limit),
    );

    let _ = world.set_named(entity, body, "position", &Value::Vec2(clamped));
    let _ = world.set_named(entity, body, "velocity", &Value::Vec2(new_vel));
}

/// What a session run reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Ticks simulated.
    pub ticks: u32,
    /// Snapshots the server sent.
    pub snapshots_sent: u32,
    /// Snapshots the client received. The difference from `snapshots_sent` is loss.
    pub snapshots_received: u32,
    /// Total bytes of snapshot payload sent.
    pub bytes_sent: usize,
    /// Times the client's prediction had to be corrected.
    pub corrections: u64,
    /// Total ticks re-simulated during corrections.
    pub resimulated: u64,
    /// Times the client had to adopt server state wholesale.
    pub resyncs: u32,
    /// Largest snapshot sent, against the 1200-byte MTU.
    pub largest_snapshot: usize,
    /// Ticks the server simulated without the client's input having arrived.
    ///
    /// The number the client lead exists to drive to zero. A non-zero value on a good link means
    /// the lead is too short.
    pub input_starved: u32,
    /// How far ahead of the server the client ran, in ticks.
    pub lead_ticks: u32,
    /// Snapshots that described an already-confirmed tick and were ignored.
    pub stale: u32,
    /// A hash of *which* snapshots arrived, in order.
    ///
    /// Counts alone cannot distinguish two runs that lost the same number of different packets, so
    /// this captures the arrival pattern itself. It is what makes "the same seed reproduces the same
    /// run" a meaningful assertion rather than a coincidence.
    pub delivery_fingerprint: u64,
}

impl Report {
    /// Fraction of snapshots that arrived, as a percentage.
    pub fn delivery_percent(&self) -> f64 {
        if self.snapshots_sent == 0 {
            return 100.0;
        }
        self.snapshots_received as f64 * 100.0 / self.snapshots_sent as f64
    }

    /// Mean snapshot size in bytes.
    pub fn mean_snapshot_bytes(&self) -> f64 {
        if self.snapshots_sent == 0 {
            return 0.0;
        }
        self.bytes_sent as f64 / self.snapshots_sent as f64
    }
}

/// Runs a full session and returns what happened.
///
/// One server and one predicting client, over a simulated link, for `ticks` ticks.
pub fn run_session(conditions: LinkConditions, seed: u64, ticks: u32) -> Report {
    let net = MemoryNetwork::with_conditions(conditions, seed);
    let mut server_link = net.endpoint(SERVER);
    let mut client_link = net.endpoint(PeerId(1));

    let (mut server, body) = build_world();
    let (mut client, _) = build_world();

    // One player, spawned identically on both sides. A real session would replicate the spawn;
    // doing it directly here keeps the demo about prediction rather than about connection setup.
    let player = server.spawn();
    server
        .set_named(player, body, "position", &Value::Vec2(Vec2::ZERO))
        .expect("fresh entity");
    server
        .set_named(player, body, "velocity", &Value::Vec2(Vec2::ZERO))
        .expect("fresh entity");
    client.spawn_at(player);
    client
        .set_named(player, body, "position", &Value::Vec2(Vec2::ZERO))
        .expect("fresh entity");
    client
        .set_named(player, body, "velocity", &Value::Vec2(Vec2::ZERO))
        .expect("fresh entity");

    let mut predictor: Predictor<Input> = Predictor::new(240, STEP.mul(Fx::from_int(4)));

    // The client must run *ahead* of the server, or its input for tick N arrives after the server
    // has already simulated N. Without this the server substitutes a default input every tick, the
    // prediction disagrees every tick, and the client corrects continuously — which is what the
    // first version of this demo did, on a lossless LAN, and is exactly the failure ADR-0016
    // describes.
    let mut clock = ClockSync::new(TICK_US);
    clock.update_from_rtt(conditions.latency_us * 2, conditions.jitter_us);
    let lead = clock.target_lead_ticks();
    let mut acks = AckTracker::new(256);
    let mut baseline: Option<WorldSnapshot> = None;
    let mut pending_inputs: Vec<(Tick, Input)> = Vec::new();
    let mut recent_inputs: Vec<(u32, Input)> = Vec::new();
    let mut last_input = Input::default();
    let mut report = Report::default();

    for tick_index in 1..=ticks {
        let tick = Tick(tick_index);
        let now = Timestamp::from_micros(tick_index as u64 * TICK_US);

        // ---- client: sample input, predict immediately, tell the server ----
        // The client simulates `lead` ticks in the future, so its input reaches the server before
        // the server gets there.
        let client_tick_index = tick_index + lead;
        let input = scripted_input(client_tick_index);

        step_entity(&mut client, body, player, input);
        client.set_tick(Tick(client_tick_index));
        predictor.record(Tick(client_tick_index), input, client.snapshot());

        // Send a short window of recent inputs, not just this tick's. Inputs are tiny and cannot
        // be retransmitted usefully — by the time a retransmission arrived the tick would have
        // passed — so the standard answer is redundancy: one lost packet then costs nothing,
        // because the next packet already carries the input again.
        recent_inputs.push((client_tick_index, input));
        if recent_inputs.len() > INPUT_REDUNDANCY {
            recent_inputs.remove(0);
        }
        let mut msg = Vec::with_capacity(2 + recent_inputs.len() * 6);
        msg.extend_from_slice(&(recent_inputs.len() as u16).to_le_bytes());
        for (t, i) in &recent_inputs {
            msg.extend_from_slice(&t.to_le_bytes());
            msg.extend_from_slice(&i.encode());
        }
        client_link
            .send(SERVER, &msg, now)
            .expect("input is far below the MTU");

        // ---- server: consume inputs, simulate, occasionally send state ----
        for received in drain(&mut server_link, now) {
            let d = &received.data;
            if d.len() < 2 {
                continue;
            }
            let count = u16::from_le_bytes(d[0..2].try_into().expect("checked")) as usize;
            if d.len() != 2 + count * 6 {
                continue; // malformed; drop rather than parse partially
            }
            for k in 0..count {
                let o = 2 + k * 6;
                let t = u32::from_le_bytes(d[o..o + 4].try_into().expect("checked"));
                let i = Input::decode([d[o + 4], d[o + 5]]);
                if !pending_inputs.iter().any(|(pt, _)| pt.0 == t) {
                    pending_inputs.push((Tick(t), i));
                }
            }
        }

        // Apply the input for this tick if it arrived; otherwise repeat the last known one, which
        // is the same prediction rollback makes and is right far more often than it is wrong.
        let applied = pending_inputs
            .iter()
            .find(|(t, _)| *t == tick)
            .map(|(_, i)| *i)
            .unwrap_or(last_input);
        if let Some((_, i)) = pending_inputs.iter().find(|(t, _)| *t == tick) {
            last_input = *i;
        } else {
            report.input_starved += 1;
        }
        pending_inputs.retain(|(t, _)| t.is_newer_than(tick));

        step_entity(&mut server, body, player, applied);
        server.set_tick(tick);
        clock.observe_lead(client_tick_index, tick_index);

        if tick_index % SNAPSHOT_EVERY == 0 {
            let delta = encode_delta(&server, baseline.as_ref()).expect("schemas match");
            if delta.bytes.len() <= MTU {
                server_link
                    .send(PeerId(1), &delta.bytes, now)
                    .expect("checked against the MTU");
                report.snapshots_sent += 1;
                report.bytes_sent += delta.bytes.len();
                report.largest_snapshot = report.largest_snapshot.max(delta.bytes.len());
                let _ = acks.begin_send(now);
                baseline = Some(delta.as_sent);
            }
        }

        // ---- client: apply whatever arrived, then reconcile ----
        for received in drain(&mut client_link, now) {
            let mut authoritative = client.clone();
            if apply_delta(&mut authoritative, &received.data).is_err() {
                continue; // a corrupt or truncated snapshot is dropped, not trusted
            }
            report.snapshots_received += 1;
            report.delivery_fingerprint = report
                .delivery_fingerprint
                .rotate_left(7)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ tick_index as u64;

            let snap = authoritative.snapshot();
            match predictor.reconcile(&mut client, &snap, |w, i| step_entity(w, body, player, *i)) {
                Ok(Reconciliation::Resynced { .. }) => report.resyncs += 1,
                Ok(Reconciliation::Stale { .. }) => report.stale += 1,
                Ok(_) => {}
                Err(_) => report.resyncs += 1,
            }
        }

        report.ticks += 1;
    }

    report.lead_ticks = lead;
    report.corrections = predictor.corrections();
    report.resimulated = predictor.resimulated_total();
    report
}

/// A repeatable input pattern: circles around the arena, then holds still.
///
/// Scripted rather than random so a run is reproducible and comparable between changes.
pub fn scripted_input(tick: u32) -> Input {
    match (tick / 30) % 5 {
        0 => Input {
            thrust_x: 1,
            thrust_y: 0,
        },
        1 => Input {
            thrust_x: 0,
            thrust_y: 1,
        },
        2 => Input {
            thrust_x: -1,
            thrust_y: 0,
        },
        3 => Input {
            thrust_x: 0,
            thrust_y: -1,
        },
        _ => Input::default(),
    }
}
