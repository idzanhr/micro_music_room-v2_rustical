// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The v1 `library.json` format: loading is strictly read-only, saving is atomic with a backup.
//!
//! v1 schema (PascalCase):
//! ```json
//! { "Tracks": ["C:\\Music\\a.mp3"], "Playlists": [{ "Name": "Mix", "Paths": ["C:\\Music\\a.mp3"] }] }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const LIBRARY_FILE: &str = "library.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Library {
    pub tracks: Vec<PathBuf>,
    pub playlists: Vec<Playlist>,
    /// Fields this version does not know about, kept so a save never drops data a newer or older
    /// version wrote.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Playlist {
    pub name: String,
    pub paths: Vec<PathBuf>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "could not read the library file: {e}"),
            Self::Parse(e) => write!(f, "the library file is not valid JSON: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// `%LOCALAPPDATA%\MusicRoom\library.json`, the same place v1 uses.
pub fn default_library_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("MusicRoom").join(LIBRARY_FILE))
}

impl Library {
    /// Read-only. A missing file is an empty library (and is *not* created); a corrupt file is an
    /// error and is left exactly as it was. Nothing here ever writes, renames or deletes.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(LoadError::Io(e)),
        };
        // v1 was written by .NET, which may prefix a UTF-8 BOM.
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        serde_json::from_slice(bytes).map_err(LoadError::Parse)
    }

    /// Atomic save that keeps backups. Not called during the initial migration.
    ///
    /// Order of operations, so a crash at any point leaves a readable library:
    /// 1. write the new content to `library.json.tmp` and flush it to disk;
    /// 2. if a library exists, keep `library.v1.json.bak` once (the pristine original) and refresh
    ///    `library.json.bak` (the previous version);
    /// 3. rename the temp file over `library.json` (an atomic replace).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = sibling(path, "tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        if path.exists() {
            let pristine = path.with_file_name("library.v1.json.bak");
            if !pristine.exists() {
                fs::copy(path, &pristine)?;
            }
            fs::copy(path, sibling(path, "bak"))?;
        }
        fs::rename(&tmp, path)
    }
}

/// `library.json` -> `library.json.<suffix>`
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const V1_SAMPLE: &str = r#"{
        "Tracks": ["C:\\Music\\a.mp3", "C:\\Music\\b.flac"],
        "Playlists": [
            { "Name": "Road trip", "Paths": ["C:\\Music\\a.mp3"] },
            { "Name": "Empty", "Paths": [] }
        ]
    }"#;

    #[test]
    fn reads_the_v1_pascal_case_schema() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        fs::write(&file, V1_SAMPLE).unwrap();

        let lib = Library::load(&file).unwrap();
        assert_eq!(
            lib.tracks,
            [
                PathBuf::from(r"C:\Music\a.mp3"),
                PathBuf::from(r"C:\Music\b.flac")
            ]
        );
        assert_eq!(lib.playlists.len(), 2);
        assert_eq!(lib.playlists[0].name, "Road trip");
        assert_eq!(lib.playlists[0].paths, [PathBuf::from(r"C:\Music\a.mp3")]);
        assert!(lib.playlists[1].paths.is_empty());
    }

    #[test]
    fn load_never_modifies_the_file_or_its_folder() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        fs::write(&file, V1_SAMPLE).unwrap();
        let before = fs::read(&file).unwrap();
        let mtime = fs::metadata(&file).unwrap().modified().unwrap();

        Library::load(&file).unwrap();

        assert_eq!(fs::read(&file).unwrap(), before);
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), mtime);
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "load must not create backups, temp files or anything else"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_library_and_is_not_created() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        let lib = Library::load(&file).unwrap();
        assert_eq!(lib, Library::default());
        assert!(!file.exists());
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_is_left_untouched() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        fs::write(&file, b"{ not json").unwrap();
        assert!(matches!(Library::load(&file), Err(LoadError::Parse(_))));
        assert_eq!(fs::read(&file).unwrap(), b"{ not json");
    }

    #[test]
    fn tolerates_a_utf8_bom_and_missing_sections() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(br#"{ "Tracks": ["x.mp3"] }"#);
        fs::write(&file, bytes).unwrap();
        let lib = Library::load(&file).unwrap();
        assert_eq!(lib.tracks, [PathBuf::from("x.mp3")]);
        assert!(lib.playlists.is_empty());
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        fs::write(
            &file,
            r#"{"Tracks":[],"Playlists":[{"Name":"A","Paths":[],"Color":"red"}],"Theme":"dark"}"#,
        )
        .unwrap();
        let lib = Library::load(&file).unwrap();
        lib.save(&file).unwrap();
        let again = Library::load(&file).unwrap();
        assert_eq!(
            again.extra.get("Theme").and_then(Value::as_str),
            Some("dark")
        );
        assert_eq!(
            again.playlists[0]
                .extra
                .get("Color")
                .and_then(Value::as_str),
            Some("red")
        );
    }

    #[test]
    fn save_keeps_a_pristine_backup_and_a_rolling_one() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        fs::write(&file, V1_SAMPLE).unwrap();

        let mut lib = Library::load(&file).unwrap();
        lib.playlists.push(Playlist {
            name: "New".into(),
            ..Playlist::default()
        });
        lib.save(&file).unwrap();
        lib.tracks.clear();
        lib.save(&file).unwrap();

        // The original v1 bytes are still on disk, untouched, after two saves.
        assert_eq!(
            fs::read_to_string(dir.path().join("library.v1.json.bak")).unwrap(),
            V1_SAMPLE
        );
        // The rolling backup is the version before the latest save.
        let previous = Library::load(&dir.path().join("library.json.bak")).unwrap();
        assert_eq!(previous.tracks.len(), 2);
        assert_eq!(previous.playlists.len(), 3);
        // And the live file is the newest one, with no temp file left behind.
        let live = Library::load(&file).unwrap();
        assert!(live.tracks.is_empty());
        assert!(!dir.path().join("library.json.tmp").exists());
    }

    #[test]
    fn saving_uses_the_v1_key_names() {
        let dir = TempDir::new();
        let file = dir.path().join(LIBRARY_FILE);
        Library {
            tracks: vec!["a.mp3".into()],
            playlists: vec![Playlist {
                name: "P".into(),
                paths: vec!["a.mp3".into()],
                ..Default::default()
            }],
            ..Default::default()
        }
        .save(&file)
        .unwrap();
        let text = fs::read_to_string(&file).unwrap();
        for key in ["\"Tracks\"", "\"Playlists\"", "\"Name\"", "\"Paths\""] {
            assert!(text.contains(key), "missing {key} in {text}");
        }
    }
}
