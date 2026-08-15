//! Lag compensation: resolving a hit against the world as the shooter saw it.
//!
//! Implements [ADR-0014](../../../docs/adr/0014-lag-compensation.md).
//!
//! # The problem, which is geometry rather than a bug
//!
//! A client renders remote entities in the past, by `interp_delay + RTT/2` — commonly 100 to
//! 150 ms. So when a player aims at an opponent and fires, they are aiming at where that opponent
//! was 130 ms ago. Resolve the shot against present-time positions and it misses: the player saw
//! their crosshair on the target, saw the shot land, and the server disagreed.
//!
//! Someone has to be wrong. Choosing *who* is a design decision, not a technical one, and this
//! module implements the usual answer: **the shooter is right**. The server reconstructs the world
//! as they saw it and resolves there.
//!
//! The cost is visible in gameplay: a victim can be hit after reaching cover on their own screen,
//! because on the shooter's screen they had not. That is inherent to choosing the shooter, and the
//! rewind cap is the knob that bounds how bad it gets.
//!
//! # Every bound here is an attack surface
//!
//! A client that could claim arbitrary view latency could shoot into the distant past. Claimed view
//! times are therefore clamped against server-measured round-trip time, and the history buffer is
//! bounded by the same cap.

use crate::entity::Tick;
use crate::snapshot::{SnapshotRing, WorldSnapshot};
use crate::world::World;
use crate::CoreError;

/// How far back the server is willing to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewindConfig {
    /// Simulation rate, used to convert latency into ticks.
    pub tick_hz: u32,
    /// Furthest the server will rewind, in milliseconds.
    ///
    /// The single most important number here. It bounds how long after reaching cover a player can
    /// still be hit, and how much memory the history costs. 250 ms is a common choice; generous
    /// values make the game feel unfair to whoever is being shot at.
    pub max_rewind_ms: u32,
    /// Largest interpolation delay a client may claim, in milliseconds.
    ///
    /// A server cannot *verify* this number. Round-trip time is measurable, but how far behind a
    /// client chooses to render is its own business and is not observable from the wire. So the
    /// protection is not "detect the lie" — it is "bound what a lie can buy". A client claiming a
    /// hundred seconds of interpolation gets exactly the same treatment as one claiming this value.
    ///
    /// Set it to slightly above the largest delay an honest client would use: two or three snapshot
    /// intervals is typical, so 150 ms is generous at a 20 Hz send rate.
    pub max_view_delay_ms: u32,
}

impl Default for RewindConfig {
    fn default() -> RewindConfig {
        RewindConfig {
            tick_hz: 60,
            max_rewind_ms: 250,
            max_view_delay_ms: 150,
        }
    }
}

impl RewindConfig {
    /// The rewind cap expressed in ticks.
    pub fn max_rewind_ticks(&self) -> u32 {
        (self.max_rewind_ms * self.tick_hz).div_ceil(1000).max(1)
    }

    /// The tick a client with this latency was rendering, given the current server tick.
    ///
    /// Combines the measured half round trip — which the server knows and the client cannot inflate
    /// — with the client's claimed interpolation delay, which it can. The claim is bounded by
    /// [`RewindConfig::max_view_delay_ms`] and the total by [`RewindConfig::max_rewind_ms`].
    ///
    /// Note what this does *not* do: distinguish an honest claim from a dishonest one. It cannot.
    /// The guarantee is that a lie buys at most `max_view_delay_ms`, not that lying fails.
    pub fn view_tick(
        &self,
        server_tick: Tick,
        measured_rtt_ms: u32,
        claimed_view_delay_ms: u32,
    ) -> Tick {
        let one_way = measured_rtt_ms / 2;
        let claimed = claimed_view_delay_ms.min(self.max_view_delay_ms);
        let capped = one_way.saturating_add(claimed).min(self.max_rewind_ms);
        let ticks = (capped * self.tick_hz) / 1000;
        server_tick.minus(ticks)
    }
}

/// Why a rewind could not reach the requested tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindError {
    /// The tick is older than the configured cap.
    BeyondCap {
        /// The tick asked for.
        requested: Tick,
        /// The oldest tick the cap allows.
        oldest: Tick,
    },
    /// No history was recorded for that tick.
    NotRecorded {
        /// The tick asked for.
        requested: Tick,
    },
}

/// A bounded history of past worlds, for resolving hits at a client's view time.
#[derive(Debug)]
pub struct RewindBuffer {
    config: RewindConfig,
    history: SnapshotRing,
    /// Reused buffer holding the present while a query runs. Never allocated in the hot path.
    scratch: WorldSnapshot,
    newest: Option<Tick>,
}

impl RewindBuffer {
    /// Creates a buffer covering the configured rewind cap.
    pub fn new(config: RewindConfig) -> RewindBuffer {
        let ticks = config.max_rewind_ticks() as usize + 2;
        RewindBuffer {
            config,
            history: SnapshotRing::new(ticks),
            scratch: WorldSnapshot::empty(),
            newest: None,
        }
    }

    /// The configuration.
    #[inline]
    pub fn config(&self) -> RewindConfig {
        self.config
    }

    /// The newest tick recorded.
    #[inline]
    pub fn newest(&self) -> Option<Tick> {
        self.newest
    }

    /// Approximate memory held, for capacity planning.
    ///
    /// At a 250 ms cap, 60 Hz and a 4 MB arena this is roughly 60 MB per session — which is why the
    /// cap is configuration rather than something generous by default.
    pub fn size_bytes(&self) -> usize {
        self.history.size_bytes()
    }

    /// Records the world's current state. Call once per tick, after simulating.
    pub fn record(&mut self, world: &World) {
        self.history.store(world.snapshot());
        self.newest = Some(world.tick());
    }

    /// Reconstructs the world at `view_tick`, runs `query`, and restores the present.
    ///
    /// The closure is the API on purpose: a shotgun firing twelve pellets rewinds **once** and
    /// resolves twelve rays against the same reconstructed state, rather than rewinding twelve
    /// times.
    ///
    /// Authoritative state is unchanged when this returns, including on the error paths.
    pub fn rewind<R>(
        &mut self,
        world: &mut World,
        view_tick: Tick,
        query: impl FnOnce(&World) -> R,
    ) -> Result<R, CoreError> {
        let present_tick = world.tick();
        let oldest = present_tick.minus(self.config.max_rewind_ticks());

        if oldest.is_newer_than(view_tick) {
            return Err(CoreError::Rewind(RewindError::BeyondCap {
                requested: view_tick,
                oldest,
            }));
        }
        if view_tick.is_newer_than(present_tick) {
            // Asking about the future: answer with the present rather than refusing, since a client
            // slightly ahead of the server is normal and its shot is still legitimate.
            return Ok(query(world));
        }

        let Some(past) = self.history.get(view_tick).cloned() else {
            return Err(CoreError::Rewind(RewindError::NotRecorded {
                requested: view_tick,
            }));
        };

        // Save the present into the reusable buffer, rewind, query, and put it back. Doing this
        // rather than cloning the world keeps the per-query cost free of allocation.
        world.snapshot_into(&mut self.scratch);
        world.restore(&past)?;
        let result = query(world);
        let scratch = core::mem::replace(&mut self.scratch, WorldSnapshot::empty());
        world.restore(&scratch)?;
        self.scratch = scratch;
        Ok(result)
    }

    /// Discards all history.
    pub fn clear(&mut self) {
        self.history.clear();
        self.newest = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::ComponentId;
    use tempo_fixed::{Fx, Vec2};
    use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

    fn build() -> (World, ComponentId, crate::Entity) {
        let mut w = World::new();
        let body = w
            .register(ComponentDesc::new(
                "Body",
                vec![FieldDesc::new("position", FieldType::Vec2)],
            ))
            .unwrap();
        let e = w.spawn();
        w.set_named(e, body, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();
        (w, body, e)
    }

    /// Advances the world, moving the entity one unit right per tick.
    fn advance(world: &mut World, body: ComponentId, e: crate::Entity, buffer: &mut RewindBuffer) {
        let Value::Vec2(p) = world.get_named(e, body, "position").unwrap() else {
            unreachable!()
        };
        world
            .set_named(
                e,
                body,
                "position",
                &Value::Vec2(Vec2::new(p.x.add(Fx::ONE), p.y)),
            )
            .unwrap();
        world.advance_tick();
        buffer.record(world);
    }

    #[test]
    fn a_query_sees_the_world_as_it_was() {
        let (mut world, body, e) = build();
        let mut buf = RewindBuffer::new(RewindConfig::default());
        for _ in 0..10 {
            advance(&mut world, body, e, &mut buf);
        }

        // At tick 3 the entity had moved three units.
        let seen = buf
            .rewind(&mut world, Tick(3), |past| {
                match past.get_named(e, body, "position").unwrap() {
                    Value::Vec2(p) => p.x,
                    _ => unreachable!(),
                }
            })
            .unwrap();
        assert_eq!(seen, Fx::from_int(3));
    }

    #[test]
    fn the_present_is_restored_exactly_after_a_query() {
        // Rewinding must not disturb authoritative state, or a hit query would corrupt the game.
        let (mut world, body, e) = build();
        let mut buf = RewindBuffer::new(RewindConfig::default());
        for _ in 0..10 {
            advance(&mut world, body, e, &mut buf);
        }
        let before = world.state_hash();
        let tick_before = world.tick();

        buf.rewind(&mut world, Tick(3), |_| ()).unwrap();

        assert_eq!(world.state_hash(), before);
        assert_eq!(world.tick(), tick_before);
    }

    #[test]
    fn one_rewind_can_resolve_many_rays() {
        // The reason the API takes a closure: a shotgun rewinds once, not once per pellet.
        let (mut world, body, e) = build();
        let mut buf = RewindBuffer::new(RewindConfig::default());
        for _ in 0..10 {
            advance(&mut world, body, e, &mut buf);
        }

        let hits = buf
            .rewind(&mut world, Tick(5), |past| {
                let Value::Vec2(p) = past.get_named(e, body, "position").unwrap() else {
                    unreachable!()
                };
                (0..12)
                    .filter(|i| p.x.raw() > Fx::from_int(*i).raw())
                    .count()
            })
            .unwrap();
        assert_eq!(hits, 5);
    }

    #[test]
    fn rewinding_beyond_the_cap_is_refused() {
        // Otherwise a client claiming huge latency could shoot into the distant past.
        let (mut world, body, e) = build();
        let mut buf = RewindBuffer::new(RewindConfig {
            tick_hz: 60,
            max_rewind_ms: 50, // three ticks
            ..Default::default()
        });
        for _ in 0..20 {
            advance(&mut world, body, e, &mut buf);
        }

        let err = buf.rewind(&mut world, Tick(2), |_| ());
        assert!(matches!(
            err,
            Err(CoreError::Rewind(RewindError::BeyondCap { .. }))
        ));
        assert_eq!(world.tick(), Tick(20), "a refused rewind changes nothing");
    }

    #[test]
    fn a_query_about_the_future_answers_with_the_present() {
        // A client slightly ahead of the server is normal, and its shot is still legitimate.
        let (mut world, body, e) = build();
        let mut buf = RewindBuffer::new(RewindConfig::default());
        for _ in 0..5 {
            advance(&mut world, body, e, &mut buf);
        }
        let seen = buf
            .rewind(&mut world, Tick(99), |w| {
                match w.get_named(e, body, "position").unwrap() {
                    Value::Vec2(p) => p.x,
                    _ => unreachable!(),
                }
            })
            .unwrap();
        assert_eq!(seen, Fx::from_int(5));
    }

    #[test]
    fn an_absurd_view_claim_buys_no_more_than_the_configured_maximum() {
        // What the design actually guarantees, which is narrower than it first appears. A server
        // cannot verify how far behind a client renders — that is not observable from the wire —
        // so the protection is not "detect the lie" but "bound what a lie can buy".
        let c = RewindConfig::default();
        let at_limit = c.view_tick(Tick(1000), 60, c.max_view_delay_ms);
        let absurd = c.view_tick(Tick(1000), 60, 100_000);
        assert_eq!(
            at_limit, absurd,
            "claiming a hundred seconds must be treated exactly as claiming the maximum"
        );

        // An honest client below the limit gets exactly what it asked for, and no more.
        let honest = c.view_tick(Tick(1000), 60, 50);
        assert!(
            honest.is_newer_than(at_limit),
            "an honest claim rewinds less than the cap"
        );
    }

    #[test]
    fn nothing_exceeds_the_hard_rewind_cap() {
        let c = RewindConfig::default();
        for rtt in [0u32, 60, 500, 10_000, u32::MAX / 2] {
            for claim in [0u32, 50, 150, 10_000] {
                let t = c.view_tick(Tick(1000), rtt, claim);
                assert!(
                    Tick(1000).diff(t) <= c.max_rewind_ticks() as i32,
                    "rtt {rtt}, claim {claim} exceeded the cap"
                );
            }
        }
    }

    #[test]
    fn a_higher_latency_client_sees_further_back() {
        let c = RewindConfig::default();
        let low = c.view_tick(Tick(1000), 20, 30);
        let high = c.view_tick(Tick(1000), 200, 30);
        assert!(
            low.is_newer_than(high),
            "a laggier client should rewind further"
        );
    }

    #[test]
    fn history_is_bounded_by_the_cap() {
        let (mut world, body, e) = build();
        let config = RewindConfig {
            tick_hz: 60,
            max_rewind_ms: 100,
            ..Default::default()
        };
        let mut buf = RewindBuffer::new(config);
        for _ in 0..1000 {
            advance(&mut world, body, e, &mut buf);
        }
        let per_snapshot = world.snapshot().size_bytes();
        assert!(
            buf.size_bytes() <= per_snapshot * (config.max_rewind_ticks() as usize + 2),
            "history grew past its bound"
        );
    }

    #[test]
    fn an_unrecorded_tick_within_the_cap_reports_clearly() {
        let (mut world, _, _) = build();
        let mut buf = RewindBuffer::new(RewindConfig::default());
        world.set_tick(Tick(10));
        assert!(matches!(
            buf.rewind(&mut world, Tick(9), |_| ()),
            Err(CoreError::Rewind(RewindError::NotRecorded { .. }))
        ));
    }
}
