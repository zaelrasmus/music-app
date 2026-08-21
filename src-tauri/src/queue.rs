use std::collections::VecDeque;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

/// How many played tracks "previous" can walk back through.
///
/// Bounded because a long listening session would otherwise grow this forever.
const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// Which tier a playing track came from.
///
/// Needed for history: going back to a track the user had queued must not put
/// it back in the queue, so "where did this come from" has to survive the play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Manual,
    Context,
}

/// One entry in the manual queue.
///
/// `entry_id` exists because queueing the same track twice is legitimate, so
/// `track_id` is not a key. Removal and reorder address entries by this id
/// rather than by list index: the front can pop at any moment when a track
/// ends, and an index captured when the panel rendered would then point at the
/// wrong row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualEntry {
    pub entry_id: u64,
    pub track_id: i64,
}

#[derive(Debug, Clone, Copy)]
struct Playing {
    track_id: i64,
    origin: Origin,
}

/// A track that was played, and where the context cursor sat while it played.
///
/// The cursor is the part that matters. Going back to a manual item has to
/// restore the context to where it was, or playing forward again would resume
/// from the wrong place.
#[derive(Debug, Clone, Copy)]
struct HistoryEntry {
    track_id: i64,
    origin: Origin,
    context_position: usize,
}

/// The automatic continuation of whatever the user started playing: an album,
/// a playlist, a filtered library view.
///
/// Shuffle and repeat act on this and only this.
#[derive(Debug, Default)]
struct Context {
    /// The order the user chose. Shuffle never mutates this, which is what
    /// makes the original order recoverable.
    order: Vec<i64>,
    /// A permutation of indices into `order`. `None` when shuffle is off.
    shuffled: Option<Vec<usize>>,
    /// Cursor into the *active* sequence (shuffled if present, else `order`).
    position: usize,
    repeat: RepeatMode,
}

impl Context {
    fn len(&self) -> usize {
        self.order.len()
    }

    fn current(&self) -> Option<i64> {
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

    /// Replaces the contents. `start_index` indexes into `tracks` as given.
    ///
    /// The caller passes a snapshot: the context does not track later changes
    /// to the library, so a rescan mid-playback cannot reorder what is playing.
    fn set(&mut self, tracks: Vec<i64>, start_index: usize) {
        self.order = tracks;
        self.position = start_index.min(self.order.len().saturating_sub(1));

        // Re-derive the shuffle over the new contents, keeping the chosen
        // track first.
        if self.shuffled.is_some() {
            self.shuffled = None;
            self.set_shuffle(true);
        }
    }

    fn set_position(&mut self, position: usize) {
        self.position = position.min(self.order.len().saturating_sub(1));
    }

    /// What to play when the current track ends by itself.
    ///
    /// `None` means "the context ran out". Repeat-one is deliberately not
    /// handled here -- it repeats whatever is *audible*, which may be a manual
    /// item, so that decision belongs one level up.
    fn advance_natural(&mut self) -> Option<i64> {
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
    fn next(&mut self) -> Option<i64> {
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

    /// Moves the cursor to the last track. Only used by Previous under
    /// repeat-all, where the end of the list is what sits "behind" the start.
    fn wrap_to_end(&mut self) -> Option<i64> {
        if self.order.is_empty() {
            return None;
        }
        self.position = self.order.len() - 1;
        self.current()
    }

    /// The next `limit` tracks, without moving the cursor.
    ///
    /// Read-only on purpose: this feeds the "Next from …" preview, which is
    /// display only.
    fn upcoming(&self, limit: usize) -> Vec<i64> {
        let len = self.order.len();
        if len == 0 {
            return Vec::new();
        }

        // Repeat-all is the only mode where the list continues past its end.
        // Repeat-one is treated as Off here: it replays the current track, so
        // there is nothing "next" to show.
        let wraps = self.repeat == RepeatMode::All;
        // Never show the current track again as part of its own upcoming list.
        let steps = limit.min(len.saturating_sub(1));

        let mut upcoming = Vec::with_capacity(steps);
        let mut position = self.position;

        for _ in 0..steps {
            if position + 1 < len {
                position += 1;
            } else if wraps {
                position = 0;
            } else {
                break;
            }

            if let Some(id) = self.at(position) {
                upcoming.push(id);
            }
        }

        upcoming
    }

    /// Turns shuffle on or off, without interrupting what is playing.
    ///
    /// Enabling puts the track at the cursor at the front of the new order, so
    /// the context resumes from around where the user is rather than jumping.
    /// Disabling resumes the original order from wherever that track sits.
    fn set_shuffle(&mut self, on: bool) {
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

/// Everything that decides what plays next, in two tiers.
///
/// - The **manual queue** holds tracks the user explicitly queued. It has
///   priority and it is *consumed*: once an entry plays it is gone.
/// - The **context** is the automatic continuation of whatever is playing.
///   Shuffle and repeat act on it alone.
///
/// Pure logic -- no audio, no database, no clock. That is deliberate: the
/// priority rule, the shuffle/repeat interaction and what "previous" means are
/// the whole feature, and all of it is testable without a sound device. The
/// coordinator only asks "what now?" and plays the answer.
#[derive(Debug, Default)]
pub struct PlayerQueue {
    /// What is playing, and which tier it came from. This is *not* always the
    /// track at the context cursor -- that is the entire point of the split.
    current: Option<Playing>,
    manual: VecDeque<ManualEntry>,
    context: Context,
    /// Where the context came from, for the "Next from …" heading.
    context_name: Option<String>,
    history: VecDeque<HistoryEntry>,
    /// Monotonic, never reused, so a stale id from the UI matches nothing.
    next_entry_id: u64,
    /// Whether the manual queue recycles instead of draining.
    ///
    /// The queue is normally *consumed* -- an entry plays once and is gone,
    /// which is what makes "play this next" mean next and not forever. Turning
    /// this on says something different: these few tracks, round and round.
    ///
    /// Separate from repeat, which acts on the context. Repeat-all replays a
    /// playlist; this replays the handful you picked out of one, and the two
    /// are different intentions that happen to share a word.
    loop_manual: bool,
}

impl PlayerQueue {
    // --- reads ---------------------------------------------------------

    pub fn current(&self) -> Option<i64> {
        self.current.map(|playing| playing.track_id)
    }

    pub fn manual(&self) -> impl Iterator<Item = &ManualEntry> {
        self.manual.iter()
    }

    pub fn manual_len(&self) -> usize {
        self.manual.len()
    }

    pub fn loops_manual(&self) -> bool {
        self.loop_manual
    }

    /// Turns recycling on or off.
    ///
    /// Whatever is playing keeps playing -- a button that interrupts the music
    /// to prove it was pressed is a bad button -- but it *joins the loop*.
    ///
    /// "Loop these" said while a song is audible plainly includes that song:
    /// it is the one you are listening to. Leaving it out means the loop skips
    /// it forever after, and the only way back in is to queue it again by hand,
    /// which is a chore invented by an implementation detail -- that it came
    /// from a playlist rather than from the queue.
    ///
    /// It joins at the *end*, so the tracks already waiting keep their turn and
    /// the current one comes round again where it belongs. And it joins as an
    /// ordinary entry, visible in the panel and removable there, so a user who
    /// meant only the next four is one click from saying so.
    pub fn set_loop_manual(&mut self, on: bool) {
        self.loop_manual = on;

        if !on {
            return;
        }

        let Some(playing) = self.current else {
            return;
        };

        // Already round again: it came from the queue and the loop kept it, or
        // the user queued it twice. Either way, adding another would make it
        // play twice a lap.
        if self.manual.iter().any(|e| e.track_id == playing.track_id) {
            return;
        }

        self.enqueue_last(playing.track_id);
    }

    pub fn context_upcoming(&self, limit: usize) -> Vec<i64> {
        self.context.upcoming(limit)
    }

    /// What [`Self::on_next`] would return, without consuming anything.
    ///
    /// Same priority rule, read-only: the manual queue first, then the
    /// context. Exists so the coordinator can resolve the next track's stream
    /// ahead of time, which is only worth doing if it agrees with what will
    /// actually play.
    pub fn peek_next(&self) -> Option<i64> {
        if let Some(entry) = self.manual.front() {
            return Some(entry.track_id);
        }
        self.context.upcoming(1).first().copied()
    }

    /// How many context tracks follow the current one in total, ignoring any
    /// preview limit. Lets the UI say "and 300 more" without shipping them.
    pub fn context_upcoming_total(&self) -> usize {
        let len = self.context.len();
        if len == 0 {
            return 0;
        }
        match self.context.repeat {
            // Everything else in the list comes round again.
            RepeatMode::All => len - 1,
            _ => len - 1 - self.context.position.min(len - 1),
        }
    }

    pub fn context_name(&self) -> Option<&str> {
        self.context_name.as_deref()
    }

    pub fn context_len(&self) -> usize {
        self.context.len()
    }

    pub fn context_position(&self) -> usize {
        self.context.position
    }

    pub fn repeat(&self) -> RepeatMode {
        self.context.repeat
    }

    pub fn is_shuffled(&self) -> bool {
        self.context.shuffled.is_some()
    }

    // --- context -------------------------------------------------------

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.context.repeat = mode;
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.context.set_shuffle(on);
    }

    /// Starts a new context, returning the track to play.
    ///
    /// The manual queue deliberately survives: tracks the user interposed are
    /// a separate intention from "play this album", and losing them because
    /// they clicked a playlist is the behaviour users notice and complain
    /// about.
    pub fn set_context(
        &mut self,
        tracks: Vec<i64>,
        start_index: usize,
        name: Option<String>,
    ) -> Option<i64> {
        self.push_history();
        self.context.set(tracks, start_index);
        self.context_name = name;

        let track_id = self.context.current();
        self.current = track_id.map(|track_id| Playing {
            track_id,
            origin: Origin::Context,
        });
        track_id
    }

    // --- manual queue --------------------------------------------------

    /// Queues `track_id` to play immediately after the current track.
    pub fn enqueue_next(&mut self, track_id: i64) -> u64 {
        let entry_id = self.take_entry_id();
        self.manual.push_front(ManualEntry { entry_id, track_id });
        entry_id
    }

    /// Queues `track_id` behind everything already queued.
    pub fn enqueue_last(&mut self, track_id: i64) -> u64 {
        let entry_id = self.take_entry_id();
        self.manual.push_back(ManualEntry { entry_id, track_id });
        entry_id
    }

    pub fn remove_manual(&mut self, entry_id: u64) -> bool {
        let Some(index) = self.index_of(entry_id) else {
            return false;
        };
        self.manual.remove(index);
        true
    }

    pub fn reorder_manual(&mut self, entry_id: u64, to: usize) -> bool {
        let Some(from) = self.index_of(entry_id) else {
            return false;
        };

        let to = to.min(self.manual.len().saturating_sub(1));
        if from == to {
            return false;
        }

        let entry = self
            .manual
            .remove(from)
            .expect("index came from a search over this deque");
        self.manual.insert(to, entry);
        true
    }

    pub fn clear_manual(&mut self) {
        self.manual.clear();
    }

    fn index_of(&self, entry_id: u64) -> Option<usize> {
        self.manual.iter().position(|e| e.entry_id == entry_id)
    }

    fn take_entry_id(&mut self) -> u64 {
        self.next_entry_id += 1;
        self.next_entry_id
    }

    // --- advancing -----------------------------------------------------

    /// What to play when the current track ends by itself.
    ///
    /// This and [`Self::on_next`] are the whole feature: the manual queue is
    /// checked first, and only when it is empty does the context advance.
    pub fn on_finished(&mut self) -> Option<i64> {
        // Repeat-one repeats whatever is *audible*, including a queued track.
        // It therefore does not drain the manual queue -- which is correct:
        // the user asked for this one track over and over, and the queue is
        // still waiting behind it.
        if self.context.repeat == RepeatMode::One {
            if let Some(playing) = self.current {
                return Some(playing.track_id);
            }
        }

        self.push_history();

        if let Some(track_id) = self.take_from_manual() {
            return Some(track_id);
        }

        let track_id = self.context.advance_natural()?;
        self.set_context_current(track_id);
        Some(track_id)
    }

    /// What to play when the user presses Next.
    ///
    /// Same priority rule as [`Self::on_finished`], minus repeat-one: that
    /// governs natural end only, or Next would look broken in that mode.
    pub fn on_next(&mut self) -> Option<i64> {
        self.push_history();

        if let Some(track_id) = self.take_from_manual() {
            return Some(track_id);
        }

        let track_id = self.context.next()?;
        self.set_context_current(track_id);
        Some(track_id)
    }

    /// What to play when the user clicks a track in the manual queue.
    ///
    /// Everything queued ahead of it is dropped. That is what clicking the
    /// fourth item in a queue means: the three above it were going to play
    /// first, and choosing the fourth is choosing not to hear them. Leaving
    /// them in place would put them *after* the track just picked, which is
    /// the one arrangement nobody asked for.
    ///
    /// They are dropped rather than pushed to history: they never played.
    pub fn jump_to_manual(&mut self, entry_id: u64) -> Option<i64> {
        let index = self
            .manual
            .iter()
            .position(|entry| entry.entry_id == entry_id)?;

        self.push_history();

        if self.loop_manual {
            // Rotate rather than drop. Skipping ahead inside a loop is
            // choosing where to start, not choosing to lose the ones before
            // it -- they come round again, which is the entire point.
            self.manual.rotate_left(index);
        } else {
            self.manual.drain(..index);
        }

        self.take_from_manual()
    }

    /// What to play when the user clicks a track in "up next".
    ///
    /// `offset` counts from the first row shown, so zero is the same track
    /// pressing Next would reach.
    ///
    /// Stepping the cursor rather than indexing it: with shuffle on, the row
    /// at position three of the display is at some other index of the context,
    /// and only the cursor knows which. Taking the same step Next takes,
    /// repeatedly, cannot disagree with it.
    pub fn jump_to_upcoming(&mut self, offset: usize) -> Option<i64> {
        self.push_history();

        let mut reached = None;
        for _ in 0..=offset {
            match self.context.next() {
                Some(track_id) => reached = Some(track_id),
                // Ran out early. The cursor stays where it got to, exactly as
                // it would after pressing Next that many times.
                None => break,
            }
        }

        let track_id = reached?;
        self.set_context_current(track_id);
        Some(track_id)
    }

    /// What to play when the user presses Previous.
    ///
    /// Driven by history rather than by stepping the cursor backwards. With
    /// shuffle on, or with queued tracks interposed, "the track I just heard"
    /// and "the previous cursor position" are different tracks, and only the
    /// first one is unsurprising.
    ///
    /// Returning the track that is already playing means "restart it" -- the
    /// caller decides whether that is a rewind or a reload.
    pub fn on_previous(&mut self) -> Option<i64> {
        if let Some(entry) = self.history.pop_back() {
            // Restoring the cursor is what makes going back to a queued track
            // work: playing forward again resumes the context where it was,
            // not wherever it happened to be left.
            self.context.set_position(entry.context_position);
            self.current = Some(Playing {
                track_id: entry.track_id,
                origin: entry.origin,
            });
            // A consumed manual entry deliberately does *not* return to the
            // queue. It already had its turn.
            return Some(entry.track_id);
        }

        // Nothing behind us. Under repeat-all the end of the list is what sits
        // behind the start, so Previous wraps there rather than doing nothing.
        if self.current.is_some() && self.context.repeat == RepeatMode::All {
            if let Some(track_id) = self.context.wrap_to_end() {
                self.set_context_current(track_id);
                return Some(track_id);
            }
        }

        self.current()
    }

    fn take_from_manual(&mut self) -> Option<i64> {
        let entry = self.manual.pop_front()?;

        // Back to the end rather than gone. The entry keeps its id, so a row
        // the user is looking at stays the same row after it has played --
        // which is what lets them remove or reorder one mid-loop.
        if self.loop_manual {
            self.manual.push_back(entry);
        }

        self.current = Some(Playing {
            track_id: entry.track_id,
            origin: Origin::Manual,
        });
        Some(entry.track_id)
    }

    fn set_context_current(&mut self, track_id: i64) {
        self.current = Some(Playing {
            track_id,
            origin: Origin::Context,
        });
    }

    fn push_history(&mut self) {
        let Some(playing) = self.current else {
            return;
        };

        self.history.push_back(HistoryEntry {
            track_id: playing.track_id,
            origin: playing.origin,
            context_position: self.context.position,
        });

        if self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_of(n: i64) -> PlayerQueue {
        let mut q = PlayerQueue::default();
        q.set_context((1..=n).collect(), 0, Some("Test".into()));
        q
    }

    // --- context behaviour (unchanged by the split) ---------------------

    /// Four tracks picked by hand, round and round.
    #[test]
    fn a_looping_queue_never_runs_out() {
        let mut q = PlayerQueue::default();
        q.set_loop_manual(true);
        for id in [101, 102, 103, 104] {
            q.enqueue_last(id);
        }

        let heard: Vec<Option<i64>> = (0..9).map(|_| q.on_next()).collect();

        assert_eq!(
            heard,
            vec![
                Some(101), Some(102), Some(103), Some(104),
                Some(101), Some(102), Some(103), Some(104),
                Some(101),
            ],
            "the four should come round in order, forever",
        );
        assert_eq!(q.manual_len(), 4, "and none of them consumed");
    }

    /// Off by default: the queue is normally consumed, and "play this next"
    /// has to keep meaning next rather than forever.
    #[test]
    fn a_queue_is_consumed_unless_asked_otherwise() {
        let mut q = PlayerQueue::default();
        q.enqueue_last(101);
        q.enqueue_last(102);

        assert_eq!(q.on_next(), Some(101));
        assert_eq!(q.on_next(), Some(102));
        assert_eq!(q.on_next(), None);
        assert_eq!(q.manual_len(), 0);
    }

    /// Skipping ahead inside a loop chooses where to start, not what to lose.
    #[test]
    fn jumping_inside_a_loop_keeps_the_ones_it_passed() {
        let mut q = PlayerQueue::default();
        q.set_loop_manual(true);

        let ids: Vec<u64> = [101, 102, 103, 104]
            .into_iter()
            .map(|id| q.enqueue_last(id))
            .collect();

        assert_eq!(q.jump_to_manual(ids[2]), Some(103));
        assert_eq!(q.manual_len(), 4, "nothing should have been dropped");

        // And it carries on round from there.
        assert_eq!(q.on_next(), Some(104));
        assert_eq!(q.on_next(), Some(101));
    }

    /// Without the loop, the same gesture means "not those".
    #[test]
    fn jumping_without_a_loop_still_drops_what_it_passed() {
        let mut q = PlayerQueue::default();
        let ids: Vec<u64> = [101, 102, 103, 104]
            .into_iter()
            .map(|id| q.enqueue_last(id))
            .collect();

        assert_eq!(q.jump_to_manual(ids[2]), Some(103));
        assert_eq!(q.manual_len(), 1, "101 and 102 were skipped, not queued");
        assert_eq!(q.on_next(), Some(104));
    }

    /// Turning it on mid-listen does not disturb what is playing -- and does
    /// not leave it out of the loop either.
    #[test]
    fn the_playing_track_joins_the_loop_without_being_interrupted() {
        let mut q = PlayerQueue::default();
        q.enqueue_last(101);
        q.enqueue_last(102);

        assert_eq!(q.on_next(), Some(101));
        q.set_loop_manual(true);

        assert_eq!(q.current(), Some(101), "the button must not skip a track");
        assert_eq!(q.on_next(), Some(102));
        assert_eq!(
            q.on_next(),
            Some(101),
            "the song that was playing has to come round again",
        );
    }

    /// The reported case: something from a playlist is playing, four tracks are
    /// queued behind it, and "loop the queue" is expected to mean all five.
    #[test]
    fn a_context_track_joins_the_loop_it_was_playing_over() {
        let mut q = PlayerQueue::default();
        q.set_context(vec![1, 2, 3], 0, None);
        assert_eq!(q.current(), Some(1));

        for id in [101, 102, 103, 104] {
            q.enqueue_last(id);
        }
        q.set_loop_manual(true);

        let heard: Vec<Option<i64>> = (0..6).map(|_| q.on_next()).collect();
        assert_eq!(
            heard,
            vec![Some(101), Some(102), Some(103), Some(104), Some(1), Some(101)],
            "the queued four then the one that was playing, round again",
        );
    }

    /// It joins once, not once per press.
    #[test]
    fn toggling_the_loop_does_not_stack_copies_of_the_playing_track() {
        let mut q = PlayerQueue::default();
        q.set_context(vec![1], 0, None);
        q.enqueue_last(101);

        q.set_loop_manual(true);
        q.set_loop_manual(false);
        q.set_loop_manual(true);

        let queued: Vec<i64> = q.manual().map(|e| e.track_id).collect();
        assert_eq!(queued, vec![101, 1], "got {queued:?}");
    }

    #[test]
    fn repeat_off_stops_at_the_end() {
        let mut q = queue_of(2);
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.on_finished(), Some(2));
        assert_eq!(q.on_finished(), None, "should stop, not wrap");
    }

    #[test]
    fn repeat_all_wraps_to_the_start() {
        let mut q = queue_of(2);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.on_finished(), Some(2));
        assert_eq!(q.on_finished(), Some(1));
    }

    #[test]
    fn repeat_one_replays_the_same_track() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.on_finished(), Some(1));
        assert_eq!(q.on_finished(), Some(1));
    }

    #[test]
    fn manual_next_ignores_repeat_one() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        assert_eq!(
            q.on_next(),
            Some(2),
            "repeat-one governs natural end only, or Next looks broken"
        );
    }

    #[test]
    fn next_stops_at_the_end_when_repeat_is_off() {
        let mut q = queue_of(2);
        q.on_next();
        assert_eq!(q.on_next(), None);
    }

    #[test]
    fn enabling_shuffle_keeps_the_current_track_playing() {
        let mut q = queue_of(50);
        q.on_next();
        q.on_next();
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
        q.on_next();
        q.on_next();
        let playing = q.current().expect("something is playing");

        q.set_shuffle(false);

        assert_eq!(q.current(), Some(playing), "same track keeps playing");
        assert!(!q.is_shuffled());
        let expected_next = (playing + 1 <= 50).then_some(playing + 1);
        assert_eq!(q.on_next(), expected_next);
    }

    #[test]
    fn shuffle_visits_every_track_exactly_once() {
        let mut q = queue_of(30);
        q.set_shuffle(true);

        let mut seen = vec![q.current().unwrap()];
        while let Some(id) = q.on_finished() {
            seen.push(id);
        }

        seen.sort_unstable();
        assert_eq!(seen, (1..=30).collect::<Vec<_>>(), "no repeats, no gaps");
    }

    #[test]
    fn an_empty_queue_never_produces_a_track() {
        let mut q = PlayerQueue::default();
        assert_eq!(q.current(), None);
        assert_eq!(q.on_finished(), None);
        assert_eq!(q.on_next(), None);
        assert_eq!(q.on_previous(), None);
        q.set_shuffle(true);
        assert_eq!(q.current(), None);
    }

    #[test]
    fn an_out_of_range_start_index_is_clamped() {
        let mut q = PlayerQueue::default();
        assert_eq!(q.set_context(vec![10, 20], 99, None), Some(20));
    }

    // --- the priority rule ----------------------------------------------

    #[test]
    fn a_queued_track_plays_before_the_rest_of_the_context() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        assert_eq!(q.on_finished(), Some(99), "the manual queue wins");
        assert_eq!(q.on_finished(), Some(2), "then the context resumes");
    }

    #[test]
    fn a_queued_track_is_consumed_and_never_replays() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        assert_eq!(q.on_finished(), Some(99));
        assert_eq!(q.manual_len(), 0, "playing it removed it");
        assert_eq!(q.on_finished(), Some(2));
        assert_eq!(q.on_finished(), Some(3));
        assert_eq!(q.on_finished(), None, "99 does not come round again");
    }

    #[test]
    fn repeat_all_loops_the_context_but_not_consumed_queue_items() {
        let mut q = queue_of(2);
        q.set_repeat(RepeatMode::All);
        q.enqueue_last(99);

        assert_eq!(q.on_finished(), Some(99));
        // Two full laps of the context; 99 must never reappear.
        let laps: Vec<_> = (0..4).map(|_| q.on_finished()).collect();
        assert_eq!(laps, vec![Some(2), Some(1), Some(2), Some(1)]);
    }

    #[test]
    fn play_next_jumps_ahead_of_already_queued_tracks() {
        let mut q = queue_of(2);
        q.enqueue_last(50);
        q.enqueue_last(51);
        q.enqueue_next(60);

        assert_eq!(q.on_next(), Some(60), "play-next goes to the front");
        assert_eq!(q.on_next(), Some(50));
        assert_eq!(q.on_next(), Some(51));
        assert_eq!(q.on_next(), Some(2), "then the context");
    }

    #[test]
    fn the_manual_queue_plays_in_insertion_order_even_when_shuffled() {
        let mut q = queue_of(30);
        q.set_shuffle(true);
        for id in [101, 102, 103] {
            q.enqueue_last(id);
        }

        assert_eq!(q.on_next(), Some(101));
        assert_eq!(q.on_next(), Some(102));
        assert_eq!(q.on_next(), Some(103));
    }

    #[test]
    fn next_on_a_queued_track_consumes_it_like_a_natural_end() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        assert_eq!(q.on_next(), Some(99));
        assert_eq!(q.manual_len(), 0);
        assert_eq!(q.on_next(), Some(2), "the context did not skip a track");
    }

    #[test]
    fn repeat_one_holds_a_queued_track_without_draining_the_queue() {
        let mut q = queue_of(2);
        q.enqueue_last(99);
        assert_eq!(q.on_finished(), Some(99));

        q.set_repeat(RepeatMode::One);
        q.enqueue_last(98);

        assert_eq!(q.on_finished(), Some(99), "repeat-one repeats what is audible");
        assert_eq!(q.manual_len(), 1, "and leaves the queue waiting behind it");
    }

    // --- context changes -------------------------------------------------

    #[test]
    fn starting_a_new_context_keeps_the_manual_queue() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        q.set_context(vec![7, 8, 9], 0, Some("Another playlist".into()));

        assert_eq!(q.manual_len(), 1, "interposed tracks survive a context change");
        assert_eq!(q.current(), Some(7));
        assert_eq!(q.on_finished(), Some(99), "and still play first");
        assert_eq!(q.on_finished(), Some(8));
    }

    #[test]
    fn queueing_with_no_context_still_gives_something_to_play() {
        let mut q = PlayerQueue::default();
        q.enqueue_last(42);
        assert_eq!(q.on_next(), Some(42));
    }

    // --- previous ---------------------------------------------------------

    #[test]
    fn previous_walks_back_through_what_was_actually_heard() {
        let mut q = queue_of(3);
        q.on_next();
        assert_eq!(q.on_previous(), Some(1));
        assert_eq!(
            q.on_previous(),
            Some(1),
            "nothing further back, so it restarts"
        );
    }

    #[test]
    fn previous_wraps_to_the_end_when_repeating_all() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.on_previous(), Some(3));
    }

    #[test]
    fn previous_goes_back_to_a_queued_track_without_requeueing_it() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        assert_eq!(q.on_finished(), Some(99));
        assert_eq!(q.on_finished(), Some(2));

        assert_eq!(q.on_previous(), Some(99), "back to the track just heard");
        assert_eq!(q.manual_len(), 0, "but it is not queued again");

        assert_eq!(
            q.on_next(),
            Some(2),
            "and the context resumes where it was, not from the start"
        );
    }

    #[test]
    fn previous_under_shuffle_returns_the_track_that_was_heard() {
        let mut q = queue_of(30);
        q.set_shuffle(true);

        let first = q.current().expect("something is playing");
        let second = q.on_next().expect("a second track");
        assert_ne!(first, second);

        assert_eq!(q.on_previous(), Some(first));
    }

    #[test]
    fn history_is_bounded() {
        let mut q = PlayerQueue::default();
        q.set_context((1..=1000).collect(), 0, None);
        while q.on_next().is_some() {}
        assert!(
            q.history.len() <= MAX_HISTORY,
            "history grew to {}",
            q.history.len()
        );
    }

    // --- manual queue editing --------------------------------------------

    #[test]
    fn entries_are_addressed_by_id_so_the_same_track_can_be_queued_twice() {
        let mut q = queue_of(1);
        let first = q.enqueue_last(99);
        let second = q.enqueue_last(99);
        assert_ne!(first, second);

        assert!(q.remove_manual(second));
        assert_eq!(q.manual_len(), 1);
        assert_eq!(q.on_next(), Some(99), "the other copy is still queued");
    }

    #[test]
    fn removing_an_entry_that_already_played_is_a_no_op() {
        let mut q = queue_of(2);
        let entry = q.enqueue_last(99);
        q.on_next();

        assert!(
            !q.remove_manual(entry),
            "a stale id from the UI must not remove someone else's row"
        );
    }

    #[test]
    fn reordering_moves_the_addressed_entry() {
        let mut q = queue_of(1);
        q.enqueue_last(10);
        q.enqueue_last(20);
        let third = q.enqueue_last(30);

        assert!(q.reorder_manual(third, 0));
        let ids: Vec<_> = q.manual().map(|e| e.track_id).collect();
        assert_eq!(ids, vec![30, 10, 20]);
    }

    #[test]
    fn clearing_the_queue_leaves_the_context_alone() {
        let mut q = queue_of(3);
        q.enqueue_last(99);
        q.clear_manual();

        assert_eq!(q.manual_len(), 0);
        assert_eq!(q.on_finished(), Some(2), "the context is untouched");
    }

    // --- peeking -----------------------------------------------------------

    /// The peek has to agree with the advance, or prefetching warms the wrong
    /// track and the gap it was meant to remove comes back.
    #[test]
    fn peeking_agrees_with_advancing() {
        let mut q = queue_of(4);
        q.enqueue_last(99);
        q.enqueue_last(98);

        for _ in 0..5 {
            let peeked = q.peek_next();
            assert_eq!(peeked, q.on_next(), "peek disagreed with the advance");
        }
    }

    #[test]
    fn peeking_agrees_with_advancing_under_shuffle() {
        let mut q = queue_of(30);
        q.set_shuffle(true);

        for _ in 0..10 {
            let peeked = q.peek_next();
            assert_eq!(peeked, q.on_next());
        }
    }

    #[test]
    fn peeking_changes_nothing() {
        let mut q = queue_of(3);
        q.enqueue_last(99);

        q.peek_next();
        q.peek_next();

        assert_eq!(q.manual_len(), 1, "peeking consumed a queued track");
        assert_eq!(q.current(), Some(1), "peeking moved the cursor");
    }

    #[test]
    fn there_is_nothing_to_peek_at_the_end() {
        let mut q = queue_of(2);
        q.on_next();
        assert_eq!(q.peek_next(), None);
    }

    // --- the preview -------------------------------------------------------

    #[test]
    fn the_preview_shows_what_comes_next_without_moving_the_cursor() {
        let q = queue_of(5);
        assert_eq!(q.context_upcoming(3), vec![2, 3, 4]);
        assert_eq!(q.current(), Some(1), "reading the preview changed nothing");
    }

    #[test]
    fn the_preview_stops_at_the_end_unless_repeating_all() {
        let mut q = queue_of(3);
        assert_eq!(q.context_upcoming(10), vec![2, 3]);

        q.set_repeat(RepeatMode::All);
        assert_eq!(q.context_upcoming(10), vec![2, 3], "never repeats the current track");

        q.on_next();
        assert_eq!(q.context_upcoming(10), vec![3, 1], "wraps round");
    }

    #[test]
    fn the_preview_follows_the_shuffled_order() {
        let mut q = queue_of(30);
        q.set_shuffle(true);
        let preview = q.context_upcoming(5);

        assert_eq!(preview.len(), 5);
        assert!(!preview.contains(&q.current().unwrap()));

        for expected in preview {
            assert_eq!(q.on_next(), Some(expected), "preview matched playback");
        }
    }

    // --- clicking a row in the queue panel ------------------------------

    #[test]
    fn clicking_a_queued_track_drops_what_was_ahead_of_it() {
        let mut q = queue_of(3);
        q.enqueue_last(90);
        let chosen = q.enqueue_last(91);
        q.enqueue_last(92);

        assert_eq!(q.jump_to_manual(chosen), Some(91));
        assert_eq!(
            q.manual().map(|e| e.track_id).collect::<Vec<_>>(),
            vec![92],
            "the track queued before it was skipped, not moved behind it"
        );
    }

    #[test]
    fn clicking_the_first_queued_track_is_just_next() {
        let mut q = queue_of(3);
        let first = q.enqueue_last(90);
        q.enqueue_last(91);

        assert_eq!(q.jump_to_manual(first), Some(90));
        assert_eq!(q.manual().map(|e| e.track_id).collect::<Vec<_>>(), vec![91]);
    }

    #[test]
    fn clicking_a_queued_track_that_is_gone_does_nothing() {
        let mut q = queue_of(3);
        q.enqueue_last(90);

        assert_eq!(q.jump_to_manual(9999), None);
        assert_eq!(
            q.manual().count(),
            1,
            "an unknown entry must not drain the queue"
        );
    }

    #[test]
    fn clicking_up_next_plays_that_row() {
        let mut q = queue_of(5);
        // Showing 2,3,4,5 -- so row 2 is track 4.
        assert_eq!(q.jump_to_upcoming(2), Some(4));
        assert_eq!(q.current(), Some(4));
        assert_eq!(q.on_next(), Some(5), "the context continues from there");
    }

    #[test]
    fn clicking_the_first_up_next_row_agrees_with_next() {
        let mut clicked = queue_of(4);
        let mut pressed = queue_of(4);

        assert_eq!(clicked.jump_to_upcoming(0), pressed.on_next());
        assert_eq!(clicked.current(), pressed.current());
    }

    /// The rows shown are the permuted order, so the click has to follow the
    /// same cursor Next follows rather than indexing the underlying list.
    #[test]
    fn clicking_up_next_with_shuffle_on_plays_the_row_that_was_shown() {
        let mut q = queue_of(12);
        q.set_shuffle(true);

        let shown: Vec<i64> = q.context_upcoming(5);
        let target = shown[3];

        assert_eq!(
            q.jump_to_upcoming(3),
            Some(target),
            "displayed order and cursor order must not disagree"
        );
    }

    #[test]
    fn clicking_past_the_end_stops_rather_than_wrapping() {
        let mut q = queue_of(3);
        assert_eq!(q.jump_to_upcoming(50), Some(3), "clamps to the last track");
        assert_eq!(q.on_next(), None);
    }

    /// Going back has to reach the track that was actually playing, not the
    /// one the cursor would have stepped to.
    #[test]
    fn previous_returns_to_what_was_playing_before_a_jump() {
        let mut q = queue_of(6);
        assert_eq!(q.current(), Some(1));

        q.jump_to_upcoming(3);
        assert_eq!(q.current(), Some(5));

        assert_eq!(q.on_previous(), Some(1));
    }
}
