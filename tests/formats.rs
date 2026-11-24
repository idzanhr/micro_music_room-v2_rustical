// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Real-sample decode check for the v1 import formats.
//!
//! Opt-in: set `MMR_SAMPLES_DIR` to a folder of real audio files (they are never committed, as
//! they are usually copyrighted). Without it the test is skipped. Run with:
//!
//! ```text
//! MMR_SAMPLES_DIR=/path/to/samples cargo test --test formats -- --nocapture
//! ```
//!
//! Known gap: `.wma`. Symphonia has no ASF/WMA demuxer, so WMA is reported as a KNOWN GAP (and
//! does not fail the run) until a Windows Media Foundation fallback exists. If a WMA ever starts
//! decoding, the run says so, so this list can be updated.
//!
//! Each file must decode completely, be audible (not silence), report a plausible duration, and
//! survive a seek to the middle (the progress bar depends on it).

use micro_music_room::track::is_supported;
use rodio::{Decoder, Source};
use std::{fs::File, path::Path, time::Duration};

struct Outcome {
    decoded: Duration,
    reported: Option<Duration>,
    peak: f32,
    rate: u32,
    channels: u16,
    seek: Result<(), String>,
}

fn check(path: &Path) -> Result<Outcome, String> {
    let open = || {
        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        Decoder::try_from(file).map_err(|e| format!("decoder: {e}"))
    };

    let decoder = open()?;
    let (rate, channels) = (decoder.sample_rate().get(), decoder.channels().get());
    let reported = decoder.total_duration();
    let (mut count, mut peak) = (0u64, 0f32);
    for sample in decoder {
        count += 1;
        peak = peak.max(sample.abs());
    }
    let decoded = Duration::from_secs_f64(count as f64 / (f64::from(rate) * f64::from(channels)));

    let seek = reported
        .map(|total| {
            let mut d = open()?;
            d.try_seek(total / 2).map_err(|e| format!("seek: {e}"))?;
            // Playback must continue after the seek.
            d.take(4096)
                .count()
                .gt(&0)
                .then_some(())
                .ok_or_else(|| "no samples after seek".to_owned())
        })
        .unwrap_or_else(|| Err("duration not reported, seek not attempted".into()));

    Ok(Outcome {
        decoded,
        reported,
        peak,
        rate,
        channels,
        seek,
    })
}

#[test]
fn real_samples_decode() {
    let Some(dir) = std::env::var_os("MMR_SAMPLES_DIR") else {
        eprintln!("MMR_SAMPLES_DIR not set; skipping real-sample test");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read MMR_SAMPLES_DIR")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_supported(p))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no supported files in MMR_SAMPLES_DIR");

    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy();
        let known_gap = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wma"));
        match check(path) {
            Ok(_) if known_gap => {
                eprintln!("NOTE {name}: WMA decoded! Remove it from the known gaps.")
            }
            Err(e) if known_gap => eprintln!("KNOWN GAP {name}: {e}"),
            Ok(o) => {
                let drift = o
                    .reported
                    .map(|r| (o.decoded.as_secs_f64() - r.as_secs_f64()).abs());
                eprintln!(
                    "OK   {name}\n     {} Hz, {} ch, decoded {:.2}s, reported {:?}, peak {:.3}, seek: {}",
                    o.rate,
                    o.channels,
                    o.decoded.as_secs_f64(),
                    o.reported.map(|r| r.as_secs_f64()),
                    o.peak,
                    o.seek.as_ref().map_or_else(|e| e.clone(), |()| "ok".into()),
                );
                if o.peak < 0.001 {
                    failures.push(format!("{name}: decoded to silence"));
                }
                if drift.is_some_and(|d| d > 1.0) {
                    failures.push(format!(
                        "{name}: decoded length differs from reported by {drift:?}"
                    ));
                }
                if let Err(e) = o.seek {
                    failures.push(format!("{name}: {e}"));
                }
            }
            Err(e) => {
                eprintln!("FAIL {name}: {e}");
                failures.push(format!("{name}: {e}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "format problems:\n{}",
        failures.join("\n")
    );
}
