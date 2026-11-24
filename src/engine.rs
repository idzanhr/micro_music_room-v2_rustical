// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Audio output: the [`Output`] trait the app talks to, the real rodio-backed [`RodioOutput`], and
//! a [`SilentOutput`] that makes no sound (used by tests, and as a stand-in when no sound device
//! can be opened).
//!
//! Every file goes through [`prepare`], which decodes it ([`crate::decode`]) and puts a peak
//! limiter in front: some files are mastered past full scale (a real M4A sample peaked at 1.22),
//! and without the limiter those peaks would clip.

use crate::decode::{self, AudioSource};
use rodio::{
    ChannelCount, DeviceSinkBuilder, Player, SampleRate, Source,
    source::{Limit, LimitSettings, SeekError},
    stream::MixerDeviceSink,
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// Why a file could not be started. A bad file is worth skipping; a missing sound device is not,
/// since the next file would fail the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    NoDevice(String),
    BadFile(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice(m) | Self::BadFile(m) => f.write_str(m),
        }
    }
}

/// Decode a file and make it safe to play. Returns the source and its length, when the format
/// reports one (the seek bar needs it).
pub fn prepare(path: &Path) -> Result<(Clamp<Limit<AudioSource>>, Option<Duration>), String> {
    let source = decode::open(path).map_err(|e| e.to_string())?;
    let length = source.total_duration();
    Ok((Clamp(source.limit(LimitSettings::default())), length))
}

/// A last guard after the limiter. The limiter takes a moment to react, so a sharp peak can poke
/// past full scale by a fraction of a percent (a real M4A overshot by 0.3%); clamping that is
/// inaudible, and it guarantees nothing wraps or clips harshly downstream.
pub struct Clamp<S>(S);

impl<S: Source<Item = f32>> Iterator for Clamp<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        self.0.next().map(|x| x.clamp(-1.0, 1.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<S: Source<Item = f32>> Source for Clamp<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.0.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.0.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.0.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.0.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.0.try_seek(pos)
    }
}

/// The slider is linear but loudness is not: squaring it gives fine control at the quiet end,
/// which is where a background-music player is mostly used.
pub fn gain_for(slider: f32) -> f32 {
    let v = slider.clamp(0.0, 1.0);
    v * v
}

/// What the app needs from an audio backend. One file plays at a time.
pub trait Output {
    /// Start a file from the beginning. Returns its length when known.
    fn load(&mut self, path: &Path) -> Result<Option<Duration>, LoadError>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn seek(&mut self, to: Duration) -> Result<(), String>;
    fn position(&self) -> Duration;
    /// True once the loaded file has played to its end (or nothing is loaded).
    fn finished(&self) -> bool;
    /// `slider` is 0.0 to 1.0 as on screen; the backend applies [`gain_for`].
    fn set_volume(&mut self, slider: f32);
    fn stop(&mut self);
}

// ---- the real thing ------------------------------------------------------------------------

/// Plays through the default Windows output device. The device is opened on first use, so the app
/// starts fine on a machine with no sound hardware and says so when you press play.
pub struct RodioOutput {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    gain: f32,
}

impl RodioOutput {
    pub fn new(slider_volume: f32) -> Self {
        Self {
            sink: None,
            player: None,
            gain: gain_for(slider_volume),
        }
    }

    fn sink(&mut self) -> Result<&MixerDeviceSink, LoadError> {
        if self.sink.is_none() {
            let mut sink = DeviceSinkBuilder::open_default_sink()
                .map_err(|e| LoadError::NoDevice(format!("No audio output is available: {e}")))?;
            sink.log_on_drop(false);
            self.sink = Some(sink);
        }
        Ok(self.sink.as_ref().expect("opened above"))
    }
}

impl Output for RodioOutput {
    fn load(&mut self, path: &Path) -> Result<Option<Duration>, LoadError> {
        // Decode first, so a bad file leaves whatever is playing untouched until we know.
        let (source, length) = prepare(path).map_err(LoadError::BadFile)?;
        let gain = self.gain;
        let player = Player::connect_new(self.sink()?.mixer());
        player.set_volume(gain);
        player.append(source);
        // Replacing the old player drops it, which stops it.
        self.player = Some(player);
        Ok(length)
    }

    fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    fn resume(&mut self) {
        if let Some(p) = &self.player {
            p.play();
        }
    }

    fn seek(&mut self, to: Duration) -> Result<(), String> {
        match &self.player {
            Some(p) => p.try_seek(to).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    fn position(&self) -> Duration {
        self.player.as_ref().map_or(Duration::ZERO, Player::get_pos)
    }

    fn finished(&self) -> bool {
        self.player.as_ref().is_none_or(Player::empty)
    }

    fn set_volume(&mut self, slider: f32) {
        self.gain = gain_for(slider);
        if let Some(p) = &self.player {
            p.set_volume(self.gain);
        }
    }

    fn stop(&mut self) {
        self.player = None;
    }
}

// ---- the silent stand-in -------------------------------------------------------------------

/// Everything a [`SilentOutput`] "plays", shared so a test can look at it and move the clock.
#[derive(Debug, Default)]
pub struct SilentState {
    pub loaded: Option<PathBuf>,
    pub paused: bool,
    pub position: Duration,
    /// Length reported for every file.
    pub length: Option<Duration>,
    pub ended: bool,
    pub slider_volume: f32,
    /// Files listed here fail to load, with this message.
    pub broken: Vec<PathBuf>,
    pub loads: usize,
}

/// An [`Output`] that makes no sound and never touches the file system.
pub struct SilentOutput(Rc<RefCell<SilentState>>);

impl SilentOutput {
    pub fn new() -> (Self, Rc<RefCell<SilentState>>) {
        let state = Rc::new(RefCell::new(SilentState {
            length: Some(Duration::from_secs(200)),
            ..SilentState::default()
        }));
        (Self(state.clone()), state)
    }
}

impl Output for SilentOutput {
    fn load(&mut self, path: &Path) -> Result<Option<Duration>, LoadError> {
        let mut s = self.0.borrow_mut();
        if s.broken.iter().any(|b| b == path) {
            return Err(LoadError::BadFile("could not decode the file".into()));
        }
        s.loaded = Some(path.to_path_buf());
        s.paused = false;
        s.position = Duration::ZERO;
        s.ended = false;
        s.loads += 1;
        Ok(s.length)
    }

    fn pause(&mut self) {
        self.0.borrow_mut().paused = true;
    }

    fn resume(&mut self) {
        self.0.borrow_mut().paused = false;
    }

    fn seek(&mut self, to: Duration) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        s.position = s.length.map_or(to, |l| to.min(l));
        Ok(())
    }

    fn position(&self) -> Duration {
        self.0.borrow().position
    }

    fn finished(&self) -> bool {
        let s = self.0.borrow();
        s.loaded.is_none() || s.ended
    }

    fn set_volume(&mut self, slider: f32) {
        self.0.borrow_mut().slider_volume = slider;
    }

    fn stop(&mut self) {
        let mut s = self.0.borrow_mut();
        s.loaded = None;
        s.position = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A one-second 440 Hz mono 16-bit WAV, written by hand so no audio dependency is needed.
    fn write_tone(path: &Path, amplitude: f32) {
        let rate = 8000u32;
        let samples: Vec<i16> = (0..rate)
            .map(|i| {
                let t = i as f32 / rate as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * amplitude * 32767.0) as i16
            })
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); // PCM
        bytes.extend(1u16.to_le_bytes()); // mono
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for s in samples {
            bytes.extend(s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn slider_to_gain_is_quiet_at_the_bottom_and_clamped() {
        assert_eq!(gain_for(0.0), 0.0);
        assert_eq!(gain_for(1.0), 1.0);
        assert!((gain_for(0.5) - 0.25).abs() < 1e-6);
        assert_eq!(gain_for(7.0), 1.0);
        assert_eq!(gain_for(-1.0), 0.0);
    }

    #[test]
    fn a_prepared_file_reports_its_length_and_can_seek() {
        let dir = TempDir::new();
        let file = dir.path().join("tone.wav");
        write_tone(&file, 0.5);
        let (mut source, length) = prepare(&file).unwrap();
        let length = length.expect("a WAV knows its length");
        assert!((length.as_secs_f32() - 1.0).abs() < 0.01, "{length:?}");
        source.try_seek(length / 2).unwrap();
        let rest = source.count();
        assert!(
            rest > 3000 && rest < 5000,
            "about half of 8000 samples, got {rest}"
        );
    }

    #[test]
    fn loud_material_is_kept_inside_full_scale() {
        let dir = TempDir::new();
        let file = dir.path().join("loud.wav");
        write_tone(&file, 1.0);
        let (source, _) = prepare(&file).unwrap();
        let peak = source.map(f32::abs).fold(0.0, f32::max);
        assert!(peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn a_missing_file_is_an_error_message_not_a_panic() {
        let err = prepare(Path::new("/nonexistent/x.mp3")).err().unwrap();
        assert!(err.contains("could not open"), "{err}");
    }
}
