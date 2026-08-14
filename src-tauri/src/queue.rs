use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// The playback queue: an ordered list of track ids plus a cursor.
///
/// Pure logic -- no audio, no database, no clock. That is deliberate: every
/// interesting rule in this phase (what plays next, how shuffle interacts with
/// repeat, what "previous" means at the start of a list) lives here and is
/// testable without a sound device.
#[derive(Debug, Default)]
pub struct Queue {
    /// The order the user chose. Shuffle never mutates this, which is what
    /// makes the original order recoverable.
    order: Vec<i64>,
    /// A permutation of indices into `order`. `None` when shuffle is off.
    shuffled: Option<Vec<usize>>,
    /// Cursor into the *active* sequence (shuffled if present, else `order`).
    position: usize,
    repeat: RepeatMode,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    pub fn is_shuffled(&self) -> bool {
        self.shuffled.is_some()
    }

    pub fn position(&self) -> usize {
        self.position
    }

    /// The track at the cursor.
    pub fn current(&self) -> Option<i64> {
        self.at(self.position)
    }

    fn at(&self, position: usize) -> Option<i64> {
        match &self.shuffled {
            Some(perm) => perm.get(position).and_then(|&i| self.order.get(i)).copied(),
            None => self.order.get(position).copied(),
        }
    }

    /// Where the current track sits in the *original* order.
    fn current_original_index(&self) -> Option<usize> {
        match &self.shuffled {
            Some(perm) => perm.get(self.position).copied(),
            None => (self.position < self.order.len()).then_some(self.position),
        }
    }

    /// Replaces the queue. `start_index` indexes into `tracks` as given.
    ///
    /// The caller passes a snapshot: the queue does not track later changes to
    /// the library, so a rescan mid-playback cannot reorder what is playing.
    pub fn set(&mut self, tracks: Vec<i64>, start_index: usize) {
        self.order = tracks;
        self.position = start_index.min(self.order.len().saturating_sub(1));

        // Re-derive the shuffle over the new contents, keeping the chosen
        // track first.
        if self.shuffled.is_some() {
            self.shuffled = None;
            self.set_shuffle(true);
        }
    }

    /// What to play when the current track ends by itself.
    ///
    /// `None` means "stop": the list ran out and repeat is off.
    pub fn advance_natural(&mut self) -> Option<i64> {
        if self.order.is_empty() {
            return None;
        }

        match self.repeat {
            RepeatMode::One => self.current(),
            RepeatMode::All => {
                self.position = (self.position + 1) % self.order.len();
                self.current()
            }
            RepeatMode::Off => {
                if self.position + 1 < self.order.len() {
                    self.position += 1;
                    self.current()
                } else {
                    None
                }
            }
        }
    }

    /// What to play when the user presses Next.
    ///
    /// Repeat-one deliberately does *not* apply here: it governs natural end
    /// only. Otherwise Next would appear broken in that mode.
    pub fn next_manual(&mut self) -> Option<i64> {
        if self.order.is_empty() {
            return None;
        }

        match self.repeat {
            RepeatMode::One | RepeatMode::All => {
                self.position = (self.position + 1) % self.order.len();
                self.current()
            }
            RepeatMode::Off => {
                if self.position + 1 < self.order.len() {
                    self.position += 1;
                    self.current()
                } else {
                    None
                }
            }
        }
    }

    /// What to play when the user presses Previous.
    ///
    /// At the start of the list with repeat off the cursor stays put; the
    /// caller restarts the track. (The "restart if more than a few seconds in"
    /// rule needs the playback clock, so it lives in the coordinator.)
    pub fn previous_manual(&mut self) -> Option<i64> {
        if self.order.is_empty() {
            return None;
        }

        if self.position > 0 {
            self.position -= 1;
        } else if matches!(self.repeat, RepeatMode::All | RepeatMode::One) {
            self.position = self.order.len() - 1;
        }

        self.current()
    }

    /// Turns shuffle on or off, without interrupting what is playing.
    ///
    /// Enabling puts the current track at the front of the new order, so the
    /// user does not get yanked to a random track. Disabling resumes the
    /// original order from wherever the current track sits in it.
    pub fn set_shuffle(&mut self, on: bool) {
        match (on, self.shuffled.is_some()) {
            (true, _) => {
                let current = self.current_original_index();

                let mut perm: Vec<usize> = (0..self.order.len()).collect();
                perm.shuffle(&mut rand::rng());

                if let Some(current) = current {
                    if let Some(at) = perm.iter().position(|&i| i == current) {
                        perm.swap(0, at);
                    }
                }

                self.shuffled = Some(perm);
                self.position = 0;
            }
            (false, true) => {
                let current = self.current_original_index();
                self.shuffled = None;
                self.position = current.unwrap_or(0);
            }
            (false, false) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_of(n: i64) -> Queue {
        let mut q = Queue::default();
        q.set((1..=n).collect(), 0);
        q
    }

    #[test]
    fn repeat_off_stops_at_the_end() {
        let mut q = queue_of(2);
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.advance_natural(), Some(2));
        assert_eq!(q.advance_natural(), None, "should stop, not wrap");
    }

    #[test]
    fn repeat_all_wraps_to_the_start() {
        let mut q = queue_of(2);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.advance_natural(), Some(2));
        assert_eq!(q.advance_natural(), Some(1));
    }

    #[test]
    fn repeat_one_replays_the_same_track() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.advance_natural(), Some(1));
        assert_eq!(q.advance_natural(), Some(1));
    }

    #[test]
    fn manual_next_ignores_repeat_one() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        assert_eq!(
            q.next_manual(),
            Some(2),
            "repeat-one governs natural end only, or Next looks broken"
        );
    }

    #[test]
    fn manual_next_stops_at_the_end_when_repeat_is_off() {
        let mut q = queue_of(2);
        q.next_manual();
        assert_eq!(q.next_manual(), None);
    }

    #[test]
    fn previous_walks_back_and_holds_at_the_start() {
        let mut q = queue_of(3);
        q.next_manual();
        assert_eq!(q.previous_manual(), Some(1));
        assert_eq!(q.previous_manual(), Some(1), "holds at the first track");
    }

    #[test]
    fn previous_wraps_to_the_end_when_repeating_all() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.previous_manual(), Some(3));
    }

    #[test]
    fn enabling_shuffle_keeps_the_current_track_playing() {
        let mut q = queue_of(50);
        q.next_manual();
        q.next_manual();
        let playing = q.current();

        q.set_shuffle(true);

        assert_eq!(
            q.current(),
            playing,
            "enabling shuffle must not yank the user to another track"
        );
    }

    #[test]
    fn disabling_shuffle_restores_the_original_order_from_the_current_track() {
        let mut q = queue_of(50);
        q.set_shuffle(true);
        q.next_manual();
        q.next_manual();
        let playing = q.current().expect("something is playing");

        q.set_shuffle(false);

        assert_eq!(q.current(), Some(playing), "same track keeps playing");
        assert!(!q.is_shuffled());
        // The original order is intact, so the next track is the numeric one.
        let expected_next = (playing + 1 <= 50).then_some(playing + 1);
        assert_eq!(q.next_manual(), expected_next);
    }

    #[test]
    fn shuffle_visits_every_track_exactly_once() {
        let mut q = queue_of(30);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::Off);

        let mut seen = vec![q.current().unwrap()];
        while let Some(id) = q.advance_natural() {
            seen.push(id);
        }

        seen.sort_unstable();
        assert_eq!(seen, (1..=30).collect::<Vec<_>>(), "no repeats, no gaps");
    }

    #[test]
    fn an_empty_queue_never_produces_a_track() {
        let mut q = Queue::default();
        assert_eq!(q.current(), None);
        assert_eq!(q.advance_natural(), None);
        assert_eq!(q.next_manual(), None);
        assert_eq!(q.previous_manual(), None);
        q.set_shuffle(true);
        assert_eq!(q.current(), None);
    }

    #[test]
    fn replacing_the_queue_starts_at_the_chosen_track() {
        let mut q = queue_of(3);
        q.set(vec![10, 20, 30], 2);
        assert_eq!(q.current(), Some(30));
    }

    #[test]
    fn an_out_of_range_start_index_is_clamped() {
        let mut q = Queue::default();
        q.set(vec![10, 20], 99);
        assert_eq!(q.current(), Some(20));
    }
}
