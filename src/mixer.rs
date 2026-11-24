// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Keeps the app's volume slider and the app's slider in the Windows Volume Mixer as one control:
//! moving either moves the other.
//!
//! Windows creates the app's mixer entry (an audio *session*) only once sound is playing, so the
//! link is made lazily and re-made if the output device changes. [`VolumeSync`] holds the logic
//! and talks to the session through the small [`Session`] trait, so it is tested without Windows.

/// One app's volume slider in the system mixer.
pub trait Session {
    /// The mixer's value, 0.0 to 1.0, or `None` if the session has gone away.
    fn get(&self) -> Option<f32>;
    fn set(&self, volume: f32) -> bool;
}

type Finder = Box<dyn FnMut() -> Option<Box<dyn Session>>>;

/// Polls ~7 times a second, so look for a missing session about once a second.
const SEARCH_EVERY: u32 = 6;
/// A change smaller than this is rounding noise, not the user.
const NOISE: f32 = 0.004;

pub struct VolumeSync {
    slider: f32,
    /// True once the app's own slider has been used; then the app's value wins over the mixer's
    /// when the link is first made.
    touched: bool,
    last: f32,
    session: Option<Box<dyn Session>>,
    finder: Finder,
    wait: u32,
}

impl VolumeSync {
    pub fn new(slider: f32, finder: Finder) -> Self {
        Self {
            slider,
            touched: false,
            last: slider,
            session: None,
            finder,
            wait: 0,
        }
    }

    /// The real finder: the Windows mixer on Windows, nothing elsewhere.
    pub fn system(slider: f32) -> Self {
        Self::new(slider, Box::new(system_session))
    }

    pub fn slider(&self) -> f32 {
        self.slider
    }

    /// True while the mixer owns the loudness curve (the player itself then plays at full gain).
    pub fn linked(&self) -> bool {
        self.session.is_some()
    }

    /// The app's slider moved.
    pub fn user_set(&mut self, slider: f32) {
        self.touched = true;
        self.slider = slider;
        if let Some(s) = &self.session
            && s.set(slider)
        {
            self.last = slider;
        }
    }

    /// Call often. `playing_audio` is whether an output stream exists (so the mixer entry can).
    /// Returns the new slider value when the mixer was the one that changed it.
    pub fn poll(&mut self, playing_audio: bool) -> Option<f32> {
        if let Some(session) = &self.session {
            let Some(now) = session.get() else {
                self.session = None; // device changed; find it again
                self.wait = 0;
                return None;
            };
            if (now - self.last).abs() > NOISE {
                self.last = now;
                self.slider = now;
                return Some(now);
            }
            return None;
        }
        if !playing_audio {
            return None;
        }
        if self.wait > 0 {
            self.wait -= 1;
            return None;
        }
        self.wait = SEARCH_EVERY;
        let session = (self.finder)()?;
        let result = if self.touched {
            session.set(self.slider);
            self.last = self.slider;
            None
        } else {
            // Windows remembers each app's mixer volume, so on first contact take it.
            let v = session.get().unwrap_or(self.slider);
            self.last = v;
            self.slider = v;
            Some(v)
        };
        self.session = Some(session);
        result
    }
}

#[cfg(windows)]
fn system_session() -> Option<Box<dyn Session>> {
    crate::winvolume::find().map(|s| Box::new(s) as Box<dyn Session>)
}

#[cfg(not(windows))]
fn system_session() -> Option<Box<dyn Session>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct Fake(Rc<Cell<Option<f32>>>);
    impl Session for Fake {
        fn get(&self) -> Option<f32> {
            self.0.get()
        }
        fn set(&self, v: f32) -> bool {
            self.0.set(Some(v));
            true
        }
    }

    fn sync(slider: f32, mixer: Option<f32>) -> (VolumeSync, Rc<Cell<Option<f32>>>) {
        let cell = Rc::new(Cell::new(mixer));
        let c = cell.clone();
        let finder: Finder = Box::new(move || {
            c.get()?;
            Some(Box::new(Fake(c.clone())) as Box<dyn Session>)
        });
        (VolumeSync::new(slider, finder), cell)
    }

    #[test]
    fn nothing_is_linked_before_sound_plays() {
        let (mut s, _) = sync(0.8, Some(0.3));
        assert_eq!(s.poll(false), None);
        assert!(!s.linked());
    }

    #[test]
    fn first_contact_adopts_the_mixers_remembered_volume() {
        let (mut s, _) = sync(0.8, Some(0.3));
        assert_eq!(s.poll(true), Some(0.3));
        assert!(s.linked());
        assert_eq!(s.slider(), 0.3);
    }

    #[test]
    fn a_slider_the_user_already_moved_wins_over_the_mixer() {
        let (mut s, mixer) = sync(0.8, Some(0.3));
        s.user_set(0.6);
        assert_eq!(s.poll(true), None);
        assert_eq!(mixer.get(), Some(0.6));
    }

    #[test]
    fn app_slider_moves_the_mixer_and_does_not_echo_back() {
        let (mut s, mixer) = sync(0.5, Some(0.5));
        s.poll(true);
        s.user_set(0.9);
        assert_eq!(mixer.get(), Some(0.9));
        assert_eq!(s.poll(true), None);
    }

    #[test]
    fn mixer_slider_moves_the_app() {
        let (mut s, mixer) = sync(0.5, Some(0.5));
        s.poll(true);
        mixer.set(Some(0.2));
        assert_eq!(s.poll(true), Some(0.2));
        assert_eq!(s.slider(), 0.2);
        assert_eq!(s.poll(true), None, "reported once");
    }

    #[test]
    fn a_vanished_session_is_found_again() {
        let (mut s, mixer) = sync(0.5, Some(0.5));
        s.poll(true);
        mixer.set(None);
        assert_eq!(s.poll(true), None);
        assert!(!s.linked());
        mixer.set(Some(0.7));
        let mut found = None;
        for _ in 0..10 {
            found = found.or(s.poll(true));
        }
        assert_eq!(found, Some(0.7));
    }

    #[test]
    fn no_mixer_means_the_app_keeps_working_alone() {
        let (mut s, _) = sync(0.5, None);
        for _ in 0..20 {
            assert_eq!(s.poll(true), None);
        }
        s.user_set(0.4);
        assert_eq!(s.slider(), 0.4);
        assert!(!s.linked());
    }
}
