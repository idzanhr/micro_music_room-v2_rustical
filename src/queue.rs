// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Play queue with shuffle, repeat and history.
//!
//! This replaces the v1 behaviour behind the shuffle bug: v1 shuffled the whole list and then
//! looked up the clicked track *inside the shuffled list*, so everything shuffled in front of it
//! was never played. Here the clicked track is always first, the rest follow in random order, and
//! every track plays exactly once per pass.

/// Small, dependency-free PRNG (SplitMix64). Quality is plenty for shuffling a playlist, and a
/// fixed seed makes the queue deterministic in tests.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A seed from the clock, for real use.
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::new(nanos ^ (u64::from(std::process::id()) << 32))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`), using rejection to avoid modulo bias.
    fn below(&mut self, n: usize) -> usize {
        let n = n as u64;
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next_u64();
            if v < zone {
                return (v % n) as usize;
            }
        }
    }

    /// Fisher-Yates.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Queue<T> {
    /// The list in its natural (display) order. Shuffle never changes this.
    source: Vec<T>,
    /// Already played, oldest first. `previous()` walks back through this.
    played: Vec<T>,
    current: Option<T>,
    /// What plays next, in order. Shuffle only ever rearranges this.
    upcoming: Vec<T>,
    shuffle: bool,
    repeat: Repeat,
    rng: Rng,
}

impl<T: Clone + PartialEq> Queue<T> {
    pub fn new(rng: Rng) -> Self {
        Self {
            source: Vec::new(),
            played: Vec::new(),
            current: None,
            upcoming: Vec::new(),
            shuffle: false,
            repeat: Repeat::Off,
            rng,
        }
    }

    pub fn current(&self) -> Option<&T> {
        self.current.as_ref()
    }

    pub fn upcoming(&self) -> &[T] {
        &self.upcoming
    }

    pub fn played(&self) -> &[T] {
        &self.played
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Load `tracks` and start at `start` (the row the user clicked), or at the beginning / a
    /// random track when `start` is `None` ("Play all"). With shuffle on, the start track is first
    /// and every other track follows once, in random order.
    pub fn play(&mut self, tracks: Vec<T>, start: Option<usize>) -> Option<&T> {
        self.source = tracks;
        self.played.clear();
        self.current = None;
        self.upcoming.clear();
        if self.source.is_empty() {
            return None;
        }
        let start = match start {
            Some(i) if i < self.source.len() => i,
            _ if self.shuffle => self.rng.below(self.source.len()),
            _ => 0,
        };
        self.current = Some(self.source[start].clone());
        self.upcoming = self.after_in_source(start);
        if self.shuffle {
            // Everything except the start track, in random order, so each plays once.
            self.upcoming = self.others_than(start);
            self.rng.shuffle(&mut self.upcoming);
        }
        self.current.as_ref()
    }

    /// Turn shuffle on or off mid-song. The current track and the history are untouched; only the
    /// upcoming order changes.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let Some(current) = self.current.clone() else {
            return;
        };
        if on {
            self.rng.shuffle(&mut self.upcoming);
        } else if let Some(i) = self.source.iter().position(|t| *t == current) {
            self.upcoming = self.after_in_source(i);
        }
    }

    /// Move on. `auto` is true when the track ended by itself (Repeat One then replays it); a
    /// manual skip always moves forward. Returns `None` at the end of the list with Repeat Off.
    pub fn next(&mut self, auto: bool) -> Option<&T> {
        let current = self.current.clone()?;
        if auto && self.repeat == Repeat::One {
            return self.current.as_ref();
        }
        if self.upcoming.is_empty() {
            if self.repeat == Repeat::Off {
                return None;
            }
            self.refill(&current);
        }
        if self.upcoming.is_empty() {
            return self.current.as_ref();
        }
        self.played.push(current);
        self.current = Some(self.upcoming.remove(0));
        self.current.as_ref()
    }

    /// Step back. Shuffled, that means back through what was actually played. In list order it is
    /// the track above the current one, even when playback started in the middle of the list.
    /// With nothing earlier, the current track is returned so the caller can restart it.
    pub fn previous(&mut self) -> Option<&T> {
        if let Some(prev) = self.played.pop() {
            if let Some(cur) = self.current.take() {
                self.upcoming.insert(0, cur);
            }
            self.current = Some(prev);
        } else if !self.shuffle
            && let Some(cur) = self.current.clone()
            && let Some(i) = self.source.iter().position(|t| *t == cur)
            && i > 0
        {
            self.upcoming.insert(0, cur);
            self.current = Some(self.source[i - 1].clone());
        }
        self.current.as_ref()
    }

    /// Start another pass (Repeat All): reshuffle if shuffle is on, and make sure the new pass
    /// does not open with the track that just finished.
    fn refill(&mut self, just_played: &T) {
        self.upcoming = self.source.clone();
        if self.shuffle {
            self.rng.shuffle(&mut self.upcoming);
        }
        if self.upcoming.len() > 1 && self.upcoming[0] == *just_played {
            let swap_with = 1 + self.rng.below(self.upcoming.len() - 1);
            self.upcoming.swap(0, swap_with);
        }
    }

    fn after_in_source(&self, index: usize) -> Vec<T> {
        self.source[index + 1..].to_vec()
    }

    fn others_than(&self, index: usize) -> Vec<T> {
        self.source
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, t)| t.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(shuffle: bool, seed: u64) -> Queue<u32> {
        let mut q = Queue::new(Rng::new(seed));
        q.set_shuffle(shuffle);
        q
    }

    /// Everything the queue plays from the current track until it runs out.
    fn drain(q: &mut Queue<u32>) -> Vec<u32> {
        let mut out: Vec<u32> = q.current().copied().into_iter().collect();
        while let Some(t) = q.next(true) {
            out.push(*t);
        }
        out
    }

    #[test]
    fn without_shuffle_plays_in_order_from_the_clicked_track() {
        let mut q = queue(false, 1);
        q.play((0..5).collect(), Some(2));
        assert_eq!(drain(&mut q), [2, 3, 4]);
    }

    #[test]
    fn shuffle_starts_on_the_clicked_track_and_plays_every_track_exactly_once() {
        // The v1 bug: tracks shuffled in front of the clicked one were never played.
        for seed in 0..200 {
            let mut q = queue(true, seed);
            q.play((0..20).collect(), Some(7));
            let played = drain(&mut q);
            assert_eq!(played[0], 7, "seed {seed}: must start on the clicked track");
            assert_eq!(played.len(), 20, "seed {seed}: every track must play");
            let mut sorted = played.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted,
                (0..20).collect::<Vec<_>>(),
                "seed {seed}: each exactly once"
            );
        }
    }

    #[test]
    fn shuffle_actually_changes_the_order() {
        let mut q = queue(true, 42);
        q.play((0..20).collect(), Some(0));
        assert_ne!(drain(&mut q), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn play_all_with_shuffle_covers_the_whole_list() {
        for seed in 0..50 {
            let mut q = queue(true, seed);
            q.play((0..10).collect(), None);
            let mut played = drain(&mut q);
            played.sort_unstable();
            assert_eq!(played, (0..10).collect::<Vec<_>>(), "seed {seed}");
        }
    }

    #[test]
    fn toggling_shuffle_mid_song_keeps_current_and_history() {
        let mut q = queue(false, 3);
        q.play((0..8).collect(), Some(0));
        q.next(false);
        q.next(false); // now on 2, played = [0, 1]

        q.set_shuffle(true);
        assert_eq!(q.current(), Some(&2));
        assert_eq!(q.played(), [0, 1]);
        let mut rest = q.upcoming().to_vec();
        rest.sort_unstable();
        assert_eq!(
            rest,
            [3, 4, 5, 6, 7],
            "upcoming keeps the same tracks, reordered"
        );

        q.set_shuffle(false);
        assert_eq!(q.current(), Some(&2));
        assert_eq!(
            q.upcoming(),
            [3, 4, 5, 6, 7],
            "back to list order after the current track"
        );
        assert_eq!(q.played(), [0, 1]);
    }

    #[test]
    fn previous_walks_back_through_what_was_played_even_when_shuffled() {
        let mut q = queue(true, 9);
        q.play((0..6).collect(), Some(3));
        let a = *q.current().unwrap();
        let b = *q.next(false).unwrap();
        let c = *q.next(false).unwrap();
        assert_eq!(q.previous(), Some(&b));
        assert_eq!(q.previous(), Some(&a));
        // History exhausted: stays on the first track so the caller can restart it.
        assert_eq!(q.previous(), Some(&a));
        // Going forward again replays the same order.
        assert_eq!(q.next(false), Some(&b));
        assert_eq!(q.next(false), Some(&c));
    }

    #[test]
    fn previous_in_list_order_steps_to_the_track_above_even_after_starting_mid_list() {
        let mut q = queue(false, 1);
        q.play((0..5).collect(), Some(2));
        assert_eq!(q.previous(), Some(&1));
        assert_eq!(q.previous(), Some(&0));
        assert_eq!(
            q.previous(),
            Some(&0),
            "top of the list: stays so the caller can restart it"
        );
        // Going forward again picks up in order, nothing skipped or repeated.
        assert_eq!(drain(&mut q), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn repeat_off_ends_and_stays_on_the_last_track() {
        let mut q = queue(false, 1);
        q.play((0..2).collect(), Some(0));
        assert_eq!(q.next(true), Some(&1));
        assert_eq!(q.next(true), None);
        assert_eq!(q.current(), Some(&1));
    }

    #[test]
    fn repeat_all_starts_a_new_pass_that_does_not_reopen_with_the_last_track() {
        for seed in 0..100 {
            let mut q = queue(true, seed);
            q.set_repeat(Repeat::All);
            q.play((0..5).collect(), Some(0));
            let mut last = *q.current().unwrap();
            for _ in 0..30 {
                let next = *q.next(true).expect("repeat all never ends");
                assert_ne!(next, last, "seed {seed}: same track twice in a row");
                last = next;
            }
        }
    }

    #[test]
    fn repeat_all_with_one_track_replays_it() {
        let mut q = queue(false, 1);
        q.set_repeat(Repeat::All);
        q.play(vec![7], Some(0));
        assert_eq!(q.next(true), Some(&7));
    }

    #[test]
    fn repeat_one_replays_on_auto_advance_but_a_manual_skip_moves_on() {
        let mut q = queue(false, 1);
        q.set_repeat(Repeat::One);
        q.play((0..3).collect(), Some(0));
        assert_eq!(q.next(true), Some(&0));
        assert_eq!(q.next(true), Some(&0));
        assert_eq!(q.next(false), Some(&1));
    }

    #[test]
    fn an_empty_list_and_a_bad_start_index_are_handled() {
        let mut q = queue(false, 1);
        assert_eq!(q.play(Vec::new(), None), None);
        assert_eq!(q.next(false), None);
        assert_eq!(q.previous(), None);
        assert_eq!(q.play(vec![1, 2], Some(99)), Some(&1));
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        assert_eq!(Repeat::Off.cycle(), Repeat::All);
        assert_eq!(Repeat::All.cycle(), Repeat::One);
        assert_eq!(Repeat::One.cycle(), Repeat::Off);
    }

    #[test]
    fn the_rng_is_unbiased_enough_and_deterministic() {
        let (mut a, mut b) = (Rng::new(5), Rng::new(5));
        let (mut x, mut y): (Vec<u32>, Vec<u32>) = ((0..50).collect(), (0..50).collect());
        a.shuffle(&mut x);
        b.shuffle(&mut y);
        assert_eq!(x, y);

        // Each of 4 positions should receive each of 4 values roughly a quarter of the time.
        let mut rng = Rng::new(123);
        let mut counts = [[0u32; 4]; 4];
        for _ in 0..8000 {
            let mut v = [0usize, 1, 2, 3];
            rng.shuffle(&mut v);
            for (pos, val) in v.iter().enumerate() {
                counts[pos][*val] += 1;
            }
        }
        for row in counts {
            for c in row {
                assert!((1700..2300).contains(&c), "skewed shuffle: {counts:?}");
            }
        }
    }
}
