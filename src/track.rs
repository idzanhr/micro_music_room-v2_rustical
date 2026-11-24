// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Track identity and the v1 filename conventions.

use std::path::{Path, PathBuf};

/// Import extensions carried over from v1. Whether each one actually *plays* is a separate,
/// still-unverified question (see the WMA note in the README).
pub const SUPPORTED_EXTENSIONS: [&str; 6] = ["mp3", "m4a", "wav", "aac", "wma", "flac"];

/// Case-insensitive extension check.
pub fn is_supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(e))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub folder: String,
}

impl Track {
    /// `Artist - Title.ext` splits on the first `" - "`. Otherwise the title is the file name and
    /// the artist is left empty (the list shows a dash, as v1 does) until real tags are read. The
    /// parent folder is kept separately and shows in the Album column.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder = path
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (artist, title) = match stem.split_once(" - ") {
            Some((artist, title)) => (artist.trim().to_owned(), title.trim().to_owned()),
            None => (String::new(), stem),
        };
        Self {
            path,
            title,
            artist,
            folder,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_check_is_case_insensitive_and_covers_v1_formats() {
        for name in ["a.mp3", "a.M4A", "a.Wav", "a.aac", "a.WMA", "a.flac"] {
            assert!(is_supported(Path::new(name)), "{name}");
        }
        for name in ["a.txt", "a.jpg", "a", "mp3"] {
            assert!(!is_supported(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn artist_and_title_come_from_the_file_name_when_it_has_a_dash() {
        let t = Track::from_path("/music/Mix/Daft Punk - One More Time.mp3");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.title, "One More Time");
        assert_eq!(t.folder, "Mix");
    }

    #[test]
    fn only_the_first_dash_separator_splits() {
        let t = Track::from_path("/m/A - B - C.flac");
        assert_eq!(t.artist, "A");
        assert_eq!(t.title, "B - C");
    }

    #[test]
    fn without_a_separator_the_artist_is_empty_and_the_folder_is_kept() {
        let t = Track::from_path("/music/Kerber/2002 - Slušaj mater/01. Intro.mp3");
        assert_eq!(t.artist, "");
        assert_eq!(t.title, "01. Intro");
        assert_eq!(t.folder, "2002 - Slušaj mater");
    }
}
