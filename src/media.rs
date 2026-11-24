// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! How the app talks to the operating system's media layer: the "now playing" flyout, keyboard and
//! headset media keys, and track-change notifications.
//!
//! The app only knows the [`MediaSession`] trait. On Windows the real implementation lives in
//! `winmedia.rs`; everywhere else, and in tests, there is [`NullMedia`] or [`RecordingMedia`].

use std::{cell::RefCell, rc::Rc, time::Duration};

/// Something the system asked the player to do (a media key, the flyout, a headset button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
    /// Jump to this position in the current song.
    Seek(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Stopped,
    Playing,
    Paused,
}

/// What the system should show about the current song.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The cover as a PNG file's bytes, when there is one.
    pub cover_png: Option<Vec<u8>>,
    pub length: Option<Duration>,
}

pub trait MediaSession {
    /// Show this song in the system flyout, or clear it with `None`.
    fn set_now_playing(&mut self, now: Option<&NowPlaying>);
    /// Tell the system whether music is playing and how far along it is.
    fn set_playback(&mut self, state: Playback, position: Duration, length: Option<Duration>);
    /// Everything the system asked for since the last call.
    fn take_commands(&mut self) -> Vec<MediaCommand>;
    /// Announce a new song with a notification. The implementation may stay quiet, for example
    /// when the player window is already in front.
    fn notify_track_change(&mut self, now: &NowPlaying);
}

/// Does nothing. Used where there is no system media layer to talk to.
pub struct NullMedia;

impl MediaSession for NullMedia {
    fn set_now_playing(&mut self, _: Option<&NowPlaying>) {}
    fn set_playback(&mut self, _: Playback, _: Duration, _: Option<Duration>) {}
    fn take_commands(&mut self) -> Vec<MediaCommand> {
        Vec::new()
    }
    fn notify_track_change(&mut self, _: &NowPlaying) {}
}

/// Everything a [`RecordingMedia`] was told, and the commands a test wants to deliver.
#[derive(Debug, Default)]
pub struct MediaLog {
    pub now_playing: Option<NowPlaying>,
    pub playback: Option<(Playback, Duration, Option<Duration>)>,
    pub playback_updates: usize,
    pub notifications: Vec<NowPlaying>,
    pub incoming: Vec<MediaCommand>,
}

/// A [`MediaSession`] that writes down what it is told, for tests.
pub struct RecordingMedia(Rc<RefCell<MediaLog>>);

impl RecordingMedia {
    pub fn new() -> (Self, Rc<RefCell<MediaLog>>) {
        let log = Rc::new(RefCell::new(MediaLog::default()));
        (Self(log.clone()), log)
    }
}

impl MediaSession for RecordingMedia {
    fn set_now_playing(&mut self, now: Option<&NowPlaying>) {
        self.0.borrow_mut().now_playing = now.cloned();
    }

    fn set_playback(&mut self, state: Playback, position: Duration, length: Option<Duration>) {
        let mut log = self.0.borrow_mut();
        log.playback = Some((state, position, length));
        log.playback_updates += 1;
    }

    fn take_commands(&mut self) -> Vec<MediaCommand> {
        std::mem::take(&mut self.0.borrow_mut().incoming)
    }

    fn notify_track_change(&mut self, now: &NowPlaying) {
        self.0.borrow_mut().notifications.push(now.clone());
    }
}
