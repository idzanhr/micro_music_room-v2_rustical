# Micro Music Room v2 (Rustical)

A small, local-only music player for Windows, rebuilt in Rust from the WPF original (v1.0.3.0).
No accounts, subscriptions, streaming, telemetry or ads. No Electron or embedded web runtime.

Licensed under **GPL-3.0-or-later** (same as the original).

## Download (portable)

Get **`MicroMusicRoom-v2.0.1-Portable-win-x64.zip`** from the [Releases page](https://github.com/idzanhr/micro_music_room-v2_rustical/releases/latest).

1. Unzip it anywhere (a USB stick works too). No installer, no admin rights, no extra runtimes.
2. Run `MicroMusicRoom.exe`.
3. Windows may show a SmartScreen warning because the exe is not code-signed yet: choose *More info*, then *Run anyway*.

Windows 10 or 11, 64-bit. Your v1 library is picked up automatically.

What the app writes outside its own folder (all in your user profile, nothing system-wide):

- `%LOCALAPPDATA%\MusicRoom\`: `library.json` (shared with v1), `settings.json`, `tagcache.json` (disposable), small cover images for notifications.
- `HKCU\Software\Classes\AppUserModelId\IdzanMarko.MicroMusicRoom`: the name and icon Windows shows on notifications.
- A Start Menu shortcut, `Micro Music Room.lnk`, in your own Start Menu: it lets Windows label the volume flyout "Micro Music Room" instead of "Unknown app".

To remove it: delete the folder, the Start Menu shortcut and that registry key. Your music files are never touched.

## Stack

Rust 2024 (MSRV 1.95) · Slint 1.19 · rodio 0.23 (CPAL + Symphonia) · serde / serde_json · walkdir · windows-rs

## Status

| Area | State |
|---|---|
| Library loader (v1 `library.json`, PascalCase) | Done, read-only, tested |
| Atomic save with backups | Done, tested, used when you add songs or change playlists |
| Queue: shuffle, history, repeat | Done and tested (fixes the v1 shuffle bug) |
| Folder scan, file-name parsing | Done, tested |
| Slint UI | v1 layout in rustic dark colours: sidebar (All songs, Recently added, playlists), searchable song list with multi-select, bottom player. Playback works through the default output device (tried on Windows 11 by the author) |
| Decoding | MP3, FLAC, WAV, M4A verified on real files; WMA via Media Foundation verified on a synthetic file in CI |
| Audio output | Play, pause, seek, volume, next/previous, auto-advance, repeat. Peak limiter for over-loud files. Unit-tested with a silent backend and tried on Windows 11 with real music |
| Tags and cover art | Title, artist, album, length and cover (embedded, or `cover.jpg`/`folder.jpg` beside the song) via lofty. Read lazily for the rows on screen on a background thread, cached in `%LOCALAPPDATA%\MusicRoom\tagcache.json` (a disposable cache; `library.json` is never touched). WMA tags are not read yet |
| Media flyout, media keys, track-change toasts | Done. Tried on Windows 11 with a keyboard, a Bluetooth headset and the quick-settings media card. Notifications can be switched off in About. Flyout progress bar is not shown |
| Taskbar thumbnail buttons | Done: previous, play/pause, next under the taskbar preview |
| Volume Mixer | Done: the app's slider and its Windows Volume Mixer slider are one control |
| Portable zip | Done: see Download above |
| MSIX packaging / Store update | Package builds in CI (same identity as v1, unsigned for the Store, plus a test-signed copy). Not yet submitted. See [docs/STORE-UPDATE.md](docs/STORE-UPDATE.md) |

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
| M4A (AAC-LC) | Works | real file, 48 kHz. Peaks above full scale (AAC decoder overshoot); playback runs through a limiter and clamp |
| AAC (raw ADTS) | Works | **synthetic** (made with ffmpeg from the WAV), not yet tried on a real file |
| WMA | Works on Windows (via Windows Media Foundation) | **synthetic** 1.5 s tone (wmav2), tested on a GitHub Windows runner: decode, duration, seek and a path with Croatian characters. Not yet tried on a real-world WMA (old encoders, WMA Lossless). Not supported off Windows |

Playback to a real audio device has been tried on Windows 11 with MP3, M4A and other files from the author's library; the scripted real-sample tests have only been run on Linux. The Windows CI runner covers the WMA fixture and the unit tests, not your own music.

Run it on your own files (never committed, they are usually copyrighted):

```
MMR_SAMPLES_DIR=/path/to/samples cargo test --test formats -- --nocapture
```

## Other open questions

- Not yet done: Store submission, flyout progress bar, saving volume/shuffle/repeat between runs, searching by tag text (search matches file and folder names), reading WMA tags, and a real-world WMA test.
- The exe is unsigned, hence the SmartScreen prompt.

## Releasing

Change the version in `RELEASE_VERSION` on `main` (or push a tag such as `v2.0.1`) and the *Release* workflow tests, builds, zips (with a SHA-256 file) and publishes the GitHub Release.

## Develop

```
cargo test --lib
cargo clippy --all-targets -- -D warnings
cargo run
```

## Credits

The app icon is from [Icons8](https://icons8.com), used under their free licence with attribution.
