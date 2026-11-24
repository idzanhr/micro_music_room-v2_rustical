// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! One entry point for turning a file into a playable rodio [`Source`].
//!
//! Symphonia (through rodio) handles MP3, FLAC, WAV and AAC/M4A everywhere. On Windows, `.wma`
//! goes to Windows Media Foundation, and Media Foundation is also tried as a safety net for any
//! other file Symphonia rejects. On other platforms WMA reports [`DecodeError::Unsupported`].

use rodio::{ChannelCount, Decoder, SampleRate, Source, decoder::DecoderError, source::SeekError};
use std::{fs::File, io::BufReader, path::Path, time::Duration};

#[cfg(windows)]
use crate::wmf::WmfSource;

#[derive(Debug)]
pub enum DecodeError {
    Open(std::io::Error),
    Symphonia(DecoderError),
    /// The format needs a decoder this platform does not have.
    Unsupported(&'static str),
    #[cfg(windows)]
    MediaFoundation(windows::core::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "could not open the file: {e}"),
            Self::Symphonia(e) => write!(f, "could not decode the file: {e}"),
            Self::Unsupported(why) => f.write_str(why),
            #[cfg(windows)]
            Self::MediaFoundation(e) => {
                write!(f, "Windows Media Foundation could not decode the file: {e}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A decoded audio stream, whichever decoder produced it.
pub enum AudioSource {
    Symphonia(Decoder<BufReader<File>>),
    #[cfg(windows)]
    MediaFoundation(WmfSource),
}

pub fn is_wma(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wma"))
}

pub fn open(path: &Path) -> Result<AudioSource, DecodeError> {
    if is_wma(path) {
        return open_wma(path);
    }
    let file = File::open(path).map_err(DecodeError::Open)?;
    match Decoder::try_from(file) {
        Ok(decoder) => Ok(AudioSource::Symphonia(decoder)),
        Err(e) => {
            #[cfg(windows)]
            if let Ok(source) = WmfSource::open(path) {
                return Ok(AudioSource::MediaFoundation(source));
            }
            Err(DecodeError::Symphonia(e))
        }
    }
}

#[cfg(windows)]
fn open_wma(path: &Path) -> Result<AudioSource, DecodeError> {
    WmfSource::open(path)
        .map(AudioSource::MediaFoundation)
        .map_err(DecodeError::MediaFoundation)
}

#[cfg(not(windows))]
fn open_wma(_path: &Path) -> Result<AudioSource, DecodeError> {
    Err(DecodeError::Unsupported(
        "WMA needs Windows Media Foundation, which is only available on Windows",
    ))
}

impl Iterator for AudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        match self {
            Self::Symphonia(s) => s.next(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Symphonia(s) => s.size_hint(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.size_hint(),
        }
    }
}

impl Source for AudioSource {
    fn current_span_len(&self) -> Option<usize> {
        match self {
            Self::Symphonia(s) => s.current_span_len(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.current_span_len(),
        }
    }

    fn channels(&self) -> ChannelCount {
        match self {
            Self::Symphonia(s) => s.channels(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.channels(),
        }
    }

    fn sample_rate(&self) -> SampleRate {
        match self {
            Self::Symphonia(s) => s.sample_rate(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            Self::Symphonia(s) => s.total_duration(),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.total_duration(),
        }
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        match self {
            Self::Symphonia(s) => s.try_seek(pos),
            #[cfg(windows)]
            Self::MediaFoundation(s) => s.try_seek(pos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wma_is_detected_case_insensitively() {
        assert!(is_wma(Path::new("a.wma")));
        assert!(is_wma(Path::new("A.WMA")));
        assert!(!is_wma(Path::new("a.mp3")));
        assert!(!is_wma(Path::new("wma")));
    }

    #[cfg(not(windows))]
    #[test]
    fn wma_off_windows_is_a_clear_error_not_a_panic() {
        // Reported before the file is touched, so the path need not exist.
        let err = open(Path::new("/nonexistent/song.wma"))
            .err()
            .expect("must fail");
        assert!(matches!(err, DecodeError::Unsupported(_)), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_open_error() {
        let err = open(Path::new("/nonexistent/song.mp3"))
            .err()
            .expect("must fail");
        assert!(matches!(err, DecodeError::Open(_)), "{err}");
    }
}
