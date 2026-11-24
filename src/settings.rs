// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The few preferences the app keeps, in `settings.json` beside the library. Like the tag cache it
//! is separate from `library.json`; a missing or damaged file just means the defaults.

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show a notification when the song changes (only while the player is not in front).
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notifications: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write through a temporary file so a crash cannot leave half a settings file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?,
        )?;
        fs::rename(&temp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn defaults_save_and_load_round_trip() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        assert!(Settings::load(&path).notifications, "on by default");

        Settings {
            notifications: false,
        }
        .save(&path)
        .unwrap();
        assert!(!Settings::load(&path).notifications);
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn a_damaged_or_partial_file_falls_back_to_defaults() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ nope").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::write(&path, b"{}").unwrap();
        assert!(
            Settings::load(&path).notifications,
            "missing fields use defaults"
        );
    }
}
