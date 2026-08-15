//! The rollback loop.
//!
//! Implements [ADR-0013](../../../docs/adr/0013-rollback-model.md). The shape:
//!
//! 1. Advance a frame using whatever inputs are known, predicting the rest.
//! 2. When a real input contradicts a prediction, restore the saved state from that tick and
//!    re-simulate forward with the corrected input.
//! 3. Compare per-tick state hashes with peers to detect divergence within one tick.
//!
//! This is available at all because of two earlier decisions. Fixed-point arithmetic
//! ([ADR-0002](../../../docs/adr/0002-fixed-point-determinism.md)) makes re-simulation bit-identical,
//! and the core-owned arena ([ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md)) makes
//! saving and restoring state a copy of contiguous buffers rather than a traversal of user objects.
//! Rollback is the feature those two were made for.
//!
//! # What user code must not do
//!
//! The simulation step must be a pure function of state and input. No wall clock, no unseeded
//! randomness, no unordered iteration, no I/O. Side effects — spawning particles, playing audio —
//! must be deferred to confirmed frames, or they fire again on every re-simulation.
//! [`RollbackSession::sync_test`] exists to catch violations on a developer's machine rather than
//! in a player's ranked match.

use tempo_core::{CoreError, SnapshotRing, Tick, World};

use crate::input::{InputQueue, InputSource, PlayerId};

/// How a rollback session is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackConfig {
    /// Furthest back the session will roll, in ticks.
    ///
    /// Bounded because worst-case frame time is `window × tick cost`. An unbounded window means a
    /// peer that vanished for two seconds triggers a 120-frame re-simulation and a visible freeze;
    /// stalling briefly is more predictable than that.
    pub max_rollback: u32,
    /// Ticks of delay applied to local input.
    ///
    /// The classic trade: a little uniform latency in exchange for fewer visible corrections. The
    /// right value is genre-specific, which is why it is a knob and not a constant.
    pub input_delay: u32,
    /// Ticks of input history retained.
    pub input_history: usize,
}

impl Default for RollbackConfig {
    fn default() -> RollbackConfig {
        RollbackConfig {
            max_rollback: 8,
            input_delay: 2,
            input_history: 128,
        }
    }
}

/// What advancing a frame did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameResult {
    /// The tick now simulated.
    pub tick: Tick,
    /// Ticks re-simulated before reaching it, if a rollback occurred.
    pub rolled_back: u32,
    /// True if every input used was confirmed rather than predicted.
    pub fully_confirmed: bool,
}

/// Why a rollback could not go as far back as requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackLimit {
    /// The tick is older than the configured window.
    BeyondWindow {
        /// The tick asked for.
        requested: Tick,
        /// The oldest tick the window covers.
        oldest: Tick,
    },
    /// No saved state exists for the tick.
    NoSavedState {
        /// The tick asked for.
        requested: Tick,
    },
}

/// A rollback simulation.
pub struct RollbackSession<I> {
    config: RollbackConfig,
    inputs: InputQueue<I>,
    saved: SnapshotRing,
    current: Tick,
    stats: RollbackStats,
}

/// Counters describing what a session has done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RollbackStats {
    /// Frames advanced.
    pub frames: u64,
    /// Rollbacks performed.
    pub rollbacks: u64,
    /// Total ticks re-simulated.
    pub resimulated: u64,
    /// Deepest single rollback, in ticks.
    pub deepest_rollback: u32,
    /// Rollbacks refused for falling outside the window.
    pub refused: u64,
}

impl<I: Clone + PartialEq + Default> RollbackSession<I> {
    /// Creates a session.
    pub fn new(config: RollbackConfig) -> RollbackSession<I> {
        // The ring must hold more than the rollback window: rolling back to the oldest tick in the
        // window still needs that tick's saved state to be present.
        let ring = (config.max_rollback as usize + 2).max(4);
        RollbackSession {
            inputs: InputQueue::new(config.input_history),
            saved: SnapshotRing::new(ring),
            current: Tick::ZERO,
            stats: RollbackStats::default(),
            config,
        }
    }

    /// The configuration.
    #[inline]
    pub fn config(&self) -> RollbackConfig {
        self.config
    }

    /// The tick most recently simulated.
    #[inline]
    pub fn current_tick(&self) -> Tick {
        self.current
    }

    /// Counters describing what the session has done.
    #[inline]
    pub fn stats(&self) -> RollbackStats {
        self.stats
    }

    /// The input queue.
    #[inline]
    pub fn inputs(&mut self) -> &mut InputQueue<I> {
        &mut self.inputs
    }

    /// Registers a participant.
    pub fn add_player(&mut self, player: PlayerId) {
        self.inputs.add_player(player);
    }

    /// The newest tick every participant has confirmed. State at or before it is final.
    pub fn confirmed_frame(&self) -> Option<Tick> {
        self.inputs.confirmed_frame()
    }

    /// Records the local player's input, applying the configured delay.
    ///
    /// The delay is applied here rather than at the call site so a game cannot forget it and
    /// silently lose the latency-for-stability trade it configured.
    pub fn add_local_input(&mut self, player: PlayerId, input: I) -> Tick {
        let target = self.current.plus(self.config.input_delay + 1);
        self.inputs.confirm(player, target, input);
        target
    }

    /// Records a remote input, rolling back if it contradicts a prediction already acted on.
    ///
    /// Returns the tick re-simulated from, if any.
    pub fn add_remote_input<F>(
        &mut self,
        world: &mut World,
        player: PlayerId,
        tick: Tick,
        input: I,
        simulate: F,
    ) -> Result<Option<Tick>, RollbackError>
    where
        F: FnMut(&mut World, Tick, &[(PlayerId, I, InputSource)]),
    {
        let Some(contradicted) = self.inputs.confirm(player, tick, input) else {
            return Ok(None);
        };
        self.rollback_to(world, contradicted, simulate)?;
        Ok(Some(contradicted))
    }

    /// Advances one frame, predicting any inputs that have not arrived.
    pub fn advance<F>(
        &mut self,
        world: &mut World,
        mut simulate: F,
    ) -> Result<FrameResult, RollbackError>
    where
        F: FnMut(&mut World, Tick, &[(PlayerId, I, InputSource)]),
    {
        let tick = self.current.next();

        // Save *before* simulating, so the ring holds the state a rollback to this tick restores.
        world.set_tick(self.current);
        self.saved.store(world.snapshot());

        let frame = self.inputs.frame(tick);
        let fully_confirmed = frame.iter().all(|(_, _, s)| *s == InputSource::Confirmed);

        simulate(world, tick, &frame);
        world.set_tick(tick);
        self.current = tick;
        self.stats.frames += 1;

        Ok(FrameResult {
            tick,
            rolled_back: 0,
            fully_confirmed,
        })
    }

    /// Restores state to just before `tick` and re-simulates to the present.
    pub fn rollback_to<F>(
        &mut self,
        world: &mut World,
        tick: Tick,
        mut simulate: F,
    ) -> Result<u32, RollbackError>
    where
        F: FnMut(&mut World, Tick, &[(PlayerId, I, InputSource)]),
    {
        if tick.is_newer_than(self.current) {
            return Ok(0); // nothing simulated yet at that tick
        }
        let depth = self.current.diff(tick.minus(1)).max(0) as u32;
        if depth > self.config.max_rollback {
            self.stats.refused += 1;
            return Err(RollbackError::Limit(RollbackLimit::BeyondWindow {
                requested: tick,
                oldest: self.current.minus(self.config.max_rollback),
            }));
        }

        let restore_from = tick.minus(1);
        let Some(snapshot) = self.saved.get(restore_from) else {
            self.stats.refused += 1;
            return Err(RollbackError::Limit(RollbackLimit::NoSavedState {
                requested: tick,
            }));
        };
        world.restore(snapshot)?;

        let target = self.current;
        let mut replayed = 0u32;
        let mut t = tick;
        while !t.is_newer_than(target) {
            world.set_tick(t.minus(1));
            self.saved.store(world.snapshot());
            let frame = self.inputs.frame(t);
            simulate(world, t, &frame);
            world.set_tick(t);
            replayed += 1;
            t = t.next();
        }

        self.current = target;
        self.stats.rollbacks += 1;
        self.stats.resimulated += replayed as u64;
        self.stats.deepest_rollback = self.stats.deepest_rollback.max(depth);
        Ok(replayed)
    }

    /// Re-simulates the last frame and checks the result is identical.
    ///
    /// A development mode, and the single most valuable debugging tool in a rollback system. Any
    /// nondeterminism in user code — a hash-map iteration, a wall-clock read, an uninitialised
    /// field, a stray float — shows up here immediately, on the developer's machine, instead of as
    /// a desync in a player's ranked match.
    ///
    /// Returns `Ok(())` if the re-simulation matched.
    pub fn sync_test<F>(&mut self, world: &mut World, mut simulate: F) -> Result<(), RollbackError>
    where
        F: FnMut(&mut World, Tick, &[(PlayerId, I, InputSource)]),
    {
        let tick = self.current;
        if tick == Tick::ZERO {
            return Ok(());
        }
        let expected = world.state_hash();

        let Some(before) = self.saved.get(tick.minus(1)).cloned() else {
            return Err(RollbackError::Limit(RollbackLimit::NoSavedState {
                requested: tick,
            }));
        };
        world.restore(&before)?;
        let frame = self.inputs.frame(tick);
        simulate(world, tick, &frame);
        world.set_tick(tick);

        let actual = world.state_hash();
        if actual == expected {
            Ok(())
        } else {
            Err(RollbackError::Nondeterministic { tick })
        }
    }

    /// A hash of the world's replicated state, for exchanging with peers.
    pub fn state_hash(&self, world: &World) -> [u8; 32] {
        world.state_hash()
    }

    /// Discards saved state and input history, for a hard resync.
    pub fn reset(&mut self, world: &World) {
        self.saved.clear();
        self.current = world.tick();
    }
}

impl<I> core::fmt::Debug for RollbackSession<I> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RollbackSession")
            .field("current", &self.current)
            .field("config", &self.config)
            .field("stats", &self.stats)
            .finish()
    }
}

/// Errors from a rollback session.
#[derive(Debug, Clone, PartialEq)]
pub enum RollbackError {
    /// The rollback fell outside what the session retains.
    Limit(RollbackLimit),
    /// A sync test found the simulation is not deterministic.
    Nondeterministic {
        /// The tick that failed to reproduce.
        tick: Tick,
    },
    /// An error from the core.
    Core(CoreError),
}

impl From<CoreError> for RollbackError {
    fn from(e: CoreError) -> RollbackError {
        RollbackError::Core(e)
    }
}

impl core::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RollbackError::Limit(RollbackLimit::BeyondWindow { requested, oldest }) => write!(
                f,
                "cannot roll back to {requested:?}; the window reaches only to {oldest:?}"
            ),
            RollbackError::Limit(RollbackLimit::NoSavedState { requested }) => {
                write!(f, "no saved state for {requested:?}")
            }
            RollbackError::Nondeterministic { tick } => write!(
                f,
                "re-simulating {tick:?} produced different state; the simulation is not \
                 deterministic. Check for wall-clock reads, unseeded randomness, unordered \
                 iteration, or floating point in simulated state."
            ),
            RollbackError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for RollbackError {}
