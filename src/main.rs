// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

#![cfg_attr(windows, windows_subsystem = "windows")]

use micro_music_room::library::{Library, default_library_path};
use slint::ComponentHandle;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    // Read-only: the v1 library is loaded but never written during migration.
    if let Some(path) = default_library_path() {
        match Library::load(&path) {
            Ok(lib) => eprintln!(
                "library: {} tracks, {} playlists",
                lib.tracks.len(),
                lib.playlists.len()
            ),
            Err(e) => eprintln!("library: {e} (file left untouched)"),
        }
    }
    MainWindow::new()?.run()
}
