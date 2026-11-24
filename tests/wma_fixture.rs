// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! WMA through Windows Media Foundation, against a tiny synthetic tone (`tests/fixtures/tone.wma`,
//! a generated 440 Hz sine, safe to commit). Windows only; runs by default, no setup needed.

#![cfg(windows)]

use micro_music_room::decode;
use rodio::Source;
use std::{fs, path::PathBuf, time::Duration};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.wma")
}

#[test]
fn wma_decodes_through_media_foundation() {
    let source = decode::open(&fixture()).expect("WMA should open on Windows");
    assert_eq!(source.channels().get(), 2);
    assert_eq!(source.sample_rate().get(), 44100);
    let reported = source.total_duration().expect("duration").as_secs_f64();
    assert!((reported - 1.5).abs() < 0.2, "reported {reported}");

    let (mut count, mut peak) = (0u64, 0f32);
    for s in source {
        count += 1;
        peak = peak.max(s.abs());
    }
    let decoded = count as f64 / (44100.0 * 2.0);
    assert!((decoded - 1.5).abs() < 0.2, "decoded {decoded}s");
    assert!(peak > 0.05, "silent output, peak {peak}");
}

#[test]
fn wma_seek_continues_playback() {
    let mut source = decode::open(&fixture()).unwrap();
    source.try_seek(Duration::from_millis(750)).expect("seek");
    let remaining = source.count() as f64 / (44100.0 * 2.0);
    assert!(
        remaining > 0.3 && remaining < 1.1,
        "remaining after seek: {remaining}s"
    );
}

#[test]
fn wma_path_with_croatian_characters_opens() {
    let dir = std::env::temp_dir().join(format!("mmr-wma-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("Čćšđž - tón.wma");
    fs::copy(fixture(), &copy).unwrap();
    let opened = decode::open(&copy);
    let _ = fs::remove_dir_all(&dir);
    assert!(opened.is_ok(), "{:?}", opened.err());
}
