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
| Slint UI | Placeholder window with the palette only |
| Playback (rodio) | Not started |
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

## Known open questions (not claimed as working)

- **Format parity.** v1 imports MP3, M4A, WAV, AAC, WMA and FLAC. Symphonia's support for each must be tested against real sample files before parity is claimed. **WMA in particular may need Windows Media Foundation.**
- **Windows APIs** (SMTC, AppUserModelID, thumbnail buttons, audio session) are untested: development so far ran on Linux, so none of the Windows-only code has been compiled or run yet.

## Develop

```
cargo test --lib
cargo clippy --all-targets -- -D warnings
cargo run
```
