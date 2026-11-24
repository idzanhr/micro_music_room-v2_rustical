# Micro Music Room v2 (Rustical)

A small, local-only music player for Windows, rebuilt in Rust from the WPF original (v1.0.3.0).
No accounts, subscriptions, streaming, telemetry or ads. No Electron or embedded web runtime.

Licensed under **GPL-3.0-or-later** (same as the original).

## Stack

Rust 2024 (MSRV 1.95) · Slint 1.19 · rodio 0.23 (CPAL + Symphonia) · serde / serde_json · walkdir · windows-rs

## Status

| Area | State |
|---|---|
| Library loader (v1 `library.json`, PascalCase) | Done, read-only, tested |
| Atomic save with backups | Done and tested, **not yet called anywhere** |
| Queue: shuffle, history, repeat | Done and tested (fixes the v1 shuffle bug) |
| Folder scan, file-name parsing | Done, tested |
| Slint UI | v1 layout in rustic dark colours: sidebar (All songs, Recently added, playlists), searchable song list with multi-select, bottom player. Playback is not connected yet |
| Decoding | MP3, FLAC, WAV, M4A verified on real files; WMA via Media Foundation verified on a synthetic file in CI |
| Audio output | Not started |
| SMTC, toasts, taskbar buttons, Volume Mixer entry | Not started |
| MSIX packaging | Not started |

## Library compatibility

v1 stores `%LOCALAPPDATA%\MusicRoom\library.json`:

```json
{ "Tracks": ["C:\\Music\\a.mp3"], "Playlists": [{ "Name": "Mix", "Paths": ["C:\\Music\\a.mp3"] }] }
```

- Loading never writes, renames or deletes anything. A missing file is an empty library and is not created. A corrupt file is reported and left as it is.
- Saving (future) writes a temp file, keeps `library.v1.json.bak` once (the pristine original) and `library.json.bak` (previous version), then atomically replaces `library.json`.
- Unknown fields are preserved across a save.

## Format support (tested against real files, Nov 24 2026)

Decode, audibility, duration and mid-file seek checked with `tests/formats.rs` (rodio 0.23 / Symphonia 0.5.5, on Linux):

| Format | Result | Sample |
|---|---|---|
| MP3 | Works | real file, 320 kbps, embedded cover |
| FLAC | Works | real file, 44.1 kHz, embedded cover |
| WAV | Works | real file, 16-bit PCM |
| M4A (AAC-LC) | Works | real file, 48 kHz. Peaks slightly above full scale (AAC decoder overshoot), so output will need a limiter or clamp |
| AAC (raw ADTS) | Works | **synthetic** (made with ffmpeg from the WAV), not yet tried on a real file |
| WMA | Works on Windows (via Windows Media Foundation) | **synthetic** 1.5 s tone (wmav2), tested on a GitHub Windows runner: decode, duration, seek and a path with Croatian characters. Not yet tried on a real-world WMA (old encoders, WMA Lossless). Not supported off Windows |

Not yet verified: playback to a real audio device, and the real-sample test files have only been run on Linux. The Windows CI runner covers the WMA fixture and the unit tests, not your own music.

Run it on your own files (never committed, they are usually copyrighted):

```
MMR_SAMPLES_DIR=/path/to/samples cargo test --test formats -- --nocapture
```

## Other open questions

- **Windows APIs** (SMTC, AppUserModelID, thumbnail buttons, audio session) are not written yet. The Media Foundation WMA decoder is the only Windows-specific code so far, and it is checked by CI on `windows-latest`.

## Develop

```
cargo test --lib
cargo clippy --all-targets -- -D warnings
cargo run
```
