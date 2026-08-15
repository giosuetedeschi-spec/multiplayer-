//! Per-peer input queues, with prediction for inputs that have not arrived.
//!
//! Rollback works by predicting *everyone's* inputs, not just your own, and correcting afterwards.
//! The prediction is deliberately unsophisticated: repeat the peer's last known input. For
//! human-controlled characters that is right the large majority of the time, because inputs are
//! held across many frames — a player holding "forward" holds it for dozens of ticks, not one.
//!
//! A cleverer predictor would be wrong more interestingly and no more often.

use std::collections::HashMap;

use tempo_core::Tick;

/// Identifies a participant in a rollback session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlayerId(pub u8);

/// Where an input for a tick came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    /// The peer actually sent this.
    Confirmed,
    /// Predicted by repeating the peer's last known input.
    Predicted,
}

/// One peer's inputs over the rollback window.
#[derive(Debug, Clone)]
struct PlayerInputs<I> {
    /// Ring of `(tick, input, source)`, indexed by tick modulo capacity.
    slots: Vec<Option<(Tick, I, InputSource)>>,
    /// Newest tick with a confirmed input.
    last_confirmed: Option<Tick>,
    /// The input to repeat when predicting.
    last_known: Option<I>,
}

impl<I: Clone + PartialEq> PlayerInputs<I> {
    fn new(capacity: usize) -> PlayerInputs<I> {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        PlayerInputs {
            slots,
            last_confirmed: None,
            last_known: None,
        }
    }

    fn index(&self, tick: Tick) -> usize {
        tick.0 as usize % self.slots.len()
    }

    fn get(&self, tick: Tick) -> Option<&(Tick, I, InputSource)> {
        let i = self.index(tick);
        match &self.slots[i] {
            Some(entry) if entry.0 == tick => Some(entry),
            _ => None,
        }
    }

    fn set(&mut self, tick: Tick, input: I, source: InputSource) {
        let i = self.index(tick);
        self.slots[i] = Some((tick, input, source));
    }

    /// Discards predicted entries after `tick`, leaving confirmed ones alone.
    ///
    /// Required whenever a correction lands. Predictions are derived from the last known input, so
    /// correcting tick *T* invalidates every prediction after it — they were extrapolated from a
    /// value now known to be wrong. Leaving them in place means the correction applies to exactly
    /// one tick and the re-simulation reproduces the old, wrong future.
    fn invalidate_predictions_after(&mut self, tick: Tick) {
        for slot in &mut self.slots {
            if let Some((t, _, source)) = slot {
                if *source == InputSource::Predicted && t.is_newer_than(tick) {
                    *slot = None;
                }
            }
        }
    }
}

/// Holds every participant's inputs and predicts the ones that have not arrived.
#[derive(Debug, Clone)]
pub struct InputQueue<I> {
    players: HashMap<PlayerId, PlayerInputs<I>>,
    capacity: usize,
    default_input: I,
}

impl<I: Clone + PartialEq + Default> Default for InputQueue<I> {
    fn default() -> InputQueue<I> {
        InputQueue::new(128)
    }
}

impl<I: Clone + PartialEq + Default> InputQueue<I> {
    /// Creates a queue covering `capacity` ticks of history.
    pub fn new(capacity: usize) -> InputQueue<I> {
        assert!(capacity > 0, "an input queue needs at least one slot");
        InputQueue {
            players: HashMap::new(),
            capacity,
            default_input: I::default(),
        }
    }

    /// Registers a participant. Idempotent.
    pub fn add_player(&mut self, player: PlayerId) {
        self.players
            .entry(player)
            .or_insert_with(|| PlayerInputs::new(self.capacity));
    }

    /// Removes a participant.
    pub fn remove_player(&mut self, player: PlayerId) {
        self.players.remove(&player);
    }

    /// Participants, in ascending order.
    ///
    /// Ordered because the simulation consumes inputs in this order, and an unordered iteration
    /// would make the result depend on hash seeding — the classic way a "deterministic" simulation
    /// turns out not to be.
    pub fn players(&self) -> Vec<PlayerId> {
        let mut out: Vec<PlayerId> = self.players.keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// Records a confirmed input.
    ///
    /// Returns `Some(tick)` if this contradicts a prediction already used, meaning the simulation
    /// must roll back to that tick.
    pub fn confirm(&mut self, player: PlayerId, tick: Tick, input: I) -> Option<Tick> {
        self.add_player(player);
        let entry = self.players.get_mut(&player).expect("just added");

        let contradicts = match entry.get(tick) {
            Some((_, existing, InputSource::Predicted)) => *existing != input,
            // Already confirmed, or never predicted: nothing to correct.
            _ => false,
        };

        entry.set(tick, input.clone(), InputSource::Confirmed);

        // `last_known` is what predictions extrapolate from, so it must track the *newest*
        // confirmed input. A late-arriving older input must not drag it backwards.
        let is_newest = match entry.last_confirmed {
            None => true,
            Some(t) => tick.is_newer_than(t),
        };
        if is_newest {
            entry.last_confirmed = Some(tick);
            entry.last_known = Some(input);
        }

        if contradicts {
            entry.invalidate_predictions_after(tick);
        }
        contradicts.then_some(tick)
    }

    /// Returns the input to use for a tick, predicting if it has not arrived.
    ///
    /// Recording the prediction matters: without it, a later confirmation could not tell whether
    /// the simulation had already acted on a guess.
    pub fn get_or_predict(&mut self, player: PlayerId, tick: Tick) -> (I, InputSource) {
        self.add_player(player);
        let default = self.default_input.clone();
        let entry = self.players.get_mut(&player).expect("just added");

        if let Some((_, input, source)) = entry.get(tick) {
            return (input.clone(), *source);
        }
        let predicted = entry.last_known.clone().unwrap_or(default);
        entry.set(tick, predicted.clone(), InputSource::Predicted);
        (predicted, InputSource::Predicted)
    }

    /// Inputs for every participant at a tick, in ascending player order.
    pub fn frame(&mut self, tick: Tick) -> Vec<(PlayerId, I, InputSource)> {
        self.players()
            .into_iter()
            .map(|p| {
                let (input, source) = self.get_or_predict(p, tick);
                (p, input, source)
            })
            .collect()
    }

    /// The newest tick for which *every* participant's input is confirmed.
    ///
    /// State at or before this is final; everything after it is speculative. Returns `None` if any
    /// participant has confirmed nothing at all.
    pub fn confirmed_frame(&self) -> Option<Tick> {
        let mut oldest: Option<Tick> = None;
        for entry in self.players.values() {
            let t = entry.last_confirmed?;
            oldest = Some(match oldest {
                None => t,
                Some(o) if t.is_newer_than(o) => o,
                Some(_) => t,
            });
        }
        oldest
    }

    /// Whether a tick's input for a player is confirmed rather than predicted.
    pub fn is_confirmed(&self, player: PlayerId, tick: Tick) -> bool {
        self.players
            .get(&player)
            .and_then(|e| e.get(tick))
            .is_some_and(|(_, _, s)| *s == InputSource::Confirmed)
    }

    /// Number of participants.
    #[inline]
    pub fn player_count(&self) -> usize {
        self.players.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct Buttons(u8);

    const A: PlayerId = PlayerId(0);
    const B: PlayerId = PlayerId(1);

    #[test]
    fn a_missing_input_is_predicted_by_repeating_the_last() {
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b01));

        let (input, source) = q.get_or_predict(A, Tick(2));
        assert_eq!(input, Buttons(0b01), "a held button stays held");
        assert_eq!(source, InputSource::Predicted);
    }

    #[test]
    fn the_first_prediction_falls_back_to_the_default() {
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        let (input, source) = q.get_or_predict(A, Tick(1));
        assert_eq!(input, Buttons::default());
        assert_eq!(source, InputSource::Predicted);
    }

    #[test]
    fn a_confirmation_matching_the_prediction_needs_no_rollback() {
        // The common case, and the reason repeat-last is a good predictor.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b01));
        q.get_or_predict(A, Tick(2));
        assert_eq!(q.confirm(A, Tick(2), Buttons(0b01)), None);
    }

    #[test]
    fn a_confirmation_contradicting_a_prediction_demands_a_rollback() {
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b01));
        q.get_or_predict(A, Tick(2));
        assert_eq!(q.confirm(A, Tick(2), Buttons(0b10)), Some(Tick(2)));
    }

    #[test]
    fn an_input_arriving_before_it_was_needed_costs_nothing() {
        // No prediction was made, so there is nothing to contradict.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        assert_eq!(q.confirm(A, Tick(5), Buttons(0b11)), None);
        let (input, source) = q.get_or_predict(A, Tick(5));
        assert_eq!(input, Buttons(0b11));
        assert_eq!(source, InputSource::Confirmed);
    }

    #[test]
    fn a_duplicate_confirmation_does_not_demand_a_rollback() {
        // Networks duplicate. Rolling back on a duplicate would be pure waste.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b01));
        assert_eq!(q.confirm(A, Tick(1), Buttons(0b01)), None);
    }

    #[test]
    fn a_correction_invalidates_the_predictions_derived_from_it() {
        // Predictions extrapolate from the last known input, so correcting tick 2 makes every
        // prediction after it wrong. Leaving them would apply the correction to exactly one tick
        // and reproduce the old future.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b00));
        for t in 2..=5 {
            q.get_or_predict(A, Tick(t));
        }

        assert_eq!(q.confirm(A, Tick(2), Buttons(0b11)), Some(Tick(2)));

        // Ticks 3 to 5 must now predict from the corrected value, not the stale one.
        for t in 3..=5 {
            let (input, source) = q.get_or_predict(A, Tick(t));
            assert_eq!(input, Buttons(0b11), "tick {t} kept a stale prediction");
            assert_eq!(source, InputSource::Predicted);
        }
    }

    #[test]
    fn a_correction_does_not_disturb_confirmed_inputs_after_it() {
        // Only predictions are invalidated. A confirmed input is ground truth regardless of what
        // arrives for an earlier tick.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b00));
        q.get_or_predict(A, Tick(2));
        q.confirm(A, Tick(4), Buttons(0b10));

        q.confirm(A, Tick(2), Buttons(0b11));
        assert!(q.is_confirmed(A, Tick(4)));
        assert_eq!(q.get_or_predict(A, Tick(4)).0, Buttons(0b10));
    }

    #[test]
    fn a_late_older_input_does_not_drag_the_prediction_backwards() {
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(10), Buttons(0b11));
        q.confirm(A, Tick(3), Buttons(0b00));
        assert_eq!(
            q.get_or_predict(A, Tick(11)).0,
            Buttons(0b11),
            "predictions extrapolate from the newest confirmed input, not the latest to arrive"
        );
    }

    #[test]
    fn the_confirmed_frame_is_the_slowest_participant() {
        // Everything after it is speculative, so it is gated by whoever is furthest behind.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.add_player(A);
        q.add_player(B);
        assert_eq!(q.confirmed_frame(), None, "nobody has confirmed anything");

        q.confirm(A, Tick(10), Buttons(1));
        assert_eq!(q.confirmed_frame(), None, "B still has nothing");

        q.confirm(B, Tick(4), Buttons(1));
        assert_eq!(q.confirmed_frame(), Some(Tick(4)));

        q.confirm(B, Tick(12), Buttons(1));
        assert_eq!(q.confirmed_frame(), Some(Tick(10)));
    }

    #[test]
    fn players_iterate_in_a_deterministic_order() {
        // Hash-map order would make the simulation depend on hash seeding — the classic way a
        // "deterministic" simulation turns out not to be.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        for id in [7u8, 2, 9, 0, 4] {
            q.add_player(PlayerId(id));
        }
        assert_eq!(
            q.players(),
            vec![
                PlayerId(0),
                PlayerId(2),
                PlayerId(4),
                PlayerId(7),
                PlayerId(9)
            ]
        );
    }

    #[test]
    fn a_frame_covers_every_participant() {
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(1), Buttons(0b01));
        q.add_player(B);

        let frame = q.frame(Tick(1));
        assert_eq!(frame.len(), 2);
        assert_eq!(frame[0], (A, Buttons(0b01), InputSource::Confirmed));
        assert_eq!(frame[1], (B, Buttons::default(), InputSource::Predicted));
    }

    #[test]
    fn the_ring_verifies_the_tick_before_returning() {
        // A recycled slot must report absent rather than serve a different tick's input, which
        // would be a silent wrong answer during re-simulation.
        let mut q: InputQueue<Buttons> = InputQueue::new(4);
        q.confirm(A, Tick(0), Buttons(0b01));
        assert!(q.is_confirmed(A, Tick(0)));
        q.confirm(A, Tick(4), Buttons(0b10));
        assert!(
            !q.is_confirmed(A, Tick(0)),
            "tick 0's slot now holds tick 4"
        );
        assert!(q.is_confirmed(A, Tick(4)));
    }

    #[test]
    fn removing_a_participant_stops_gating_the_confirmed_frame() {
        // A peer that leaves must not hold the confirmed frame back forever.
        let mut q: InputQueue<Buttons> = InputQueue::new(64);
        q.confirm(A, Tick(10), Buttons(1));
        q.add_player(B);
        assert_eq!(q.confirmed_frame(), None);

        q.remove_player(B);
        assert_eq!(q.confirmed_frame(), Some(Tick(10)));
        assert_eq!(q.player_count(), 1);
    }
}
