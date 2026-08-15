//! Clock synchronisation and time dilation.
//!
//! Implements [ADR-0016](../../../docs/adr/0016-clock-sync-and-time-dilation.md).
//!
//! An authoritative server simulating tick *N* needs every client's input for *N* to have arrived
//! before it gets there. If an input is late the server must either stall — unacceptable, one bad
//! connection would freeze everyone — or drop it, which the player experiences as their input being
//! ignored.
//!
//! So clients run *ahead* of the server. How far ahead depends on latency and jitter, both of which
//! vary continuously, so the lead is tracked rather than configured.
//!
//! # Dilation, not jumping
//!
//! When a client's lead is wrong it does not jump. Jumping means replaying or skipping ticks, and
//! both are visible. Instead the tick *rate* is nudged by a small percentage until the lead is
//! right: a client running 3 ms tight speeds up imperceptibly for a second and arrives correct
//! having skipped nothing.

/// How the server wants a client's clock adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DilationHint {
    /// Rate adjustment in parts per thousand. Positive means run faster.
    pub parts_per_thousand: i16,
}

impl DilationHint {
    /// No adjustment.
    pub const NONE: DilationHint = DilationHint {
        parts_per_thousand: 0,
    };

    /// Largest adjustment permitted, in parts per thousand.
    ///
    /// Bounded deliberately. Unbounded dilation would visibly speed the game up or slow it down,
    /// which is worse than losing a frame of input — so a sudden latency spike still drops inputs
    /// rather than distorting time.
    pub const MAX_PPT: i16 = 50;

    /// Clamps an adjustment into the permitted range.
    pub const fn clamped(ppt: i32) -> DilationHint {
        let v = if ppt > Self::MAX_PPT as i32 {
            Self::MAX_PPT
        } else if ppt < -(Self::MAX_PPT as i32) {
            -Self::MAX_PPT
        } else {
            ppt as i16
        };
        DilationHint {
            parts_per_thousand: v,
        }
    }

    /// The tick duration to use, given the nominal one in microseconds.
    pub const fn adjusted_tick_us(&self, nominal_us: u64) -> u64 {
        // Running faster means a shorter tick, so the sign is inverted here.
        let delta = (nominal_us as i64 * self.parts_per_thousand as i64) / 1000;
        let out = nominal_us as i64 - delta;
        if out < 1 {
            1
        } else {
            out as u64
        }
    }
}

/// Tracks how far ahead of the server a client should run.
#[derive(Debug, Clone)]
pub struct ClockSync {
    tick_us: u64,
    jitter_sigmas: u32,
    safety_ticks: u32,
    target_lead_ticks: u32,
    observed_lead_ticks: Option<i32>,
}

impl ClockSync {
    /// Creates a synchroniser for a given tick duration.
    pub fn new(tick_us: u64) -> ClockSync {
        ClockSync {
            tick_us: tick_us.max(1),
            jitter_sigmas: 2,
            safety_ticks: 1,
            target_lead_ticks: 2,
            observed_lead_ticks: None,
        }
    }

    /// Recomputes the target lead from the current round-trip estimate.
    ///
    /// `lead = RTT/2 + jitter margin + safety buffer`, expressed in ticks.
    pub fn update_from_rtt(&mut self, srtt_us: u64, rttvar_us: u64) {
        let one_way = srtt_us / 2;
        let margin = rttvar_us.saturating_mul(self.jitter_sigmas as u64);
        let lead_us = one_way + margin;
        let ticks = lead_us.div_ceil(self.tick_us) as u32;
        self.target_lead_ticks = ticks + self.safety_ticks;
    }

    /// Records the lead the server actually observed.
    ///
    /// Measuring the lead as the *server* experienced it is more accurate than inferring it from
    /// round-trip time, because it includes queueing and processing rather than just the path.
    pub fn observe_lead(&mut self, client_tick: u32, server_processed_tick: u32) {
        self.observed_lead_ticks = Some(client_tick.wrapping_sub(server_processed_tick) as i32);
    }

    /// The lead the client should be running.
    #[inline]
    pub fn target_lead_ticks(&self) -> u32 {
        self.target_lead_ticks
    }

    /// The lead most recently observed, if any.
    #[inline]
    pub fn observed_lead_ticks(&self) -> Option<i32> {
        self.observed_lead_ticks
    }

    /// The dilation to send to the client, given how full its input buffer is.
    ///
    /// Buffer occupancy is the control signal rather than round-trip time: consistently empty means
    /// the client is cutting it too fine, consistently full means it is running further ahead than
    /// it needs to and paying latency for nothing.
    pub fn dilation_for(&self, buffer_occupancy: u32) -> DilationHint {
        let target = self.target_lead_ticks;
        let error = buffer_occupancy as i32 - target as i32;
        if error == 0 {
            return DilationHint::NONE;
        }
        // Twenty parts per thousand per tick of error converges within a second or so without
        // being perceptible.
        DilationHint::clamped(-error * 20)
    }

    /// Whether the client should adjust, given what the server observed.
    pub fn dilation_from_observation(&self) -> DilationHint {
        match self.observed_lead_ticks {
            None => DilationHint::NONE,
            Some(observed) => {
                let error = observed - self.target_lead_ticks as i32;
                DilationHint::clamped(-error * 20)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK_US: u64 = 16_667; // 60 Hz

    #[test]
    fn the_lead_grows_with_latency() {
        let mut c = ClockSync::new(TICK_US);
        c.update_from_rtt(20_000, 1_000);
        let low = c.target_lead_ticks();

        c.update_from_rtt(200_000, 1_000);
        let high = c.target_lead_ticks();
        assert!(
            high > low,
            "200ms RTT needs more lead than 20ms: {high} vs {low}"
        );
    }

    #[test]
    fn the_lead_grows_with_jitter_at_the_same_latency() {
        // Two players on the same latency but different stability should not run the same lead.
        let mut steady = ClockSync::new(TICK_US);
        steady.update_from_rtt(60_000, 500);
        let mut jittery = ClockSync::new(TICK_US);
        jittery.update_from_rtt(60_000, 20_000);
        assert!(jittery.target_lead_ticks() > steady.target_lead_ticks());
    }

    #[test]
    fn dilation_pushes_the_buffer_toward_its_target() {
        let mut c = ClockSync::new(TICK_US);
        c.update_from_rtt(40_000, 2_000);
        let target = c.target_lead_ticks();

        // Buffer empty: the client is cutting it fine and must speed up.
        assert!(c.dilation_for(0).parts_per_thousand > 0);
        // Buffer overfull: the client is paying latency for nothing and should ease off.
        assert!(c.dilation_for(target + 5).parts_per_thousand < 0);
        // On target: leave it alone.
        assert_eq!(c.dilation_for(target), DilationHint::NONE);
    }

    #[test]
    fn dilation_is_bounded_so_time_never_visibly_distorts() {
        // A sudden spike must drop an input rather than warp the game's speed.
        let c = ClockSync::new(TICK_US);
        assert_eq!(
            c.dilation_for(10_000).parts_per_thousand,
            -DilationHint::MAX_PPT
        );
        assert_eq!(
            DilationHint::clamped(i32::MAX).parts_per_thousand,
            DilationHint::MAX_PPT
        );
        assert_eq!(
            DilationHint::clamped(i32::MIN).parts_per_thousand,
            -DilationHint::MAX_PPT
        );
    }

    #[test]
    fn running_faster_shortens_the_tick() {
        let faster = DilationHint {
            parts_per_thousand: 50,
        };
        let slower = DilationHint {
            parts_per_thousand: -50,
        };
        assert!(faster.adjusted_tick_us(TICK_US) < TICK_US);
        assert!(slower.adjusted_tick_us(TICK_US) > TICK_US);
        assert_eq!(DilationHint::NONE.adjusted_tick_us(TICK_US), TICK_US);
        // Five percent of 16.667 ms is about 833 microseconds.
        assert_eq!(faster.adjusted_tick_us(TICK_US), TICK_US - 833);
    }

    #[test]
    fn the_adjusted_tick_never_reaches_zero() {
        // A degenerate tick duration would make the loop spin without advancing.
        assert!(
            DilationHint {
                parts_per_thousand: 50
            }
            .adjusted_tick_us(1)
                >= 1
        );
    }

    #[test]
    fn the_observed_lead_drives_correction() {
        let mut c = ClockSync::new(TICK_US);
        c.update_from_rtt(40_000, 1_000);
        assert_eq!(
            c.dilation_from_observation(),
            DilationHint::NONE,
            "nothing observed yet"
        );

        let target = c.target_lead_ticks();
        // Running behind the target: speed up.
        c.observe_lead(100, 100 - (target - 1));
        assert!(c.dilation_from_observation().parts_per_thousand > 0);
        // Running ahead: ease off.
        c.observe_lead(100, 100 - (target + 3));
        assert!(c.dilation_from_observation().parts_per_thousand < 0);
    }

    #[test]
    fn lead_observation_survives_tick_wraparound() {
        let mut c = ClockSync::new(TICK_US);
        c.observe_lead(2, u32::MAX - 1);
        assert_eq!(c.observed_lead_ticks(), Some(4));
    }
}
