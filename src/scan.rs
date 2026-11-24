// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Recursive local folder scanning.

use crate::track::is_supported;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct ScanResult {
    /// Supported audio files, in a stable (file-name sorted) order.
    pub files: Vec<PathBuf>,
    /// Entries that could not be read (permissions, broken links). v1 aborted the whole import
    /// on the first such error; v2 keeps going and reports the count.
    pub skipped: usize,
}

pub fn scan_folder(root: &Path) -> ScanResult {
    let mut result = ScanResult::default();
    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        match entry {
            Ok(e) if e.file_type().is_file() && is_supported(e.path()) => {
                result.files.push(e.into_path());
            }
            Ok(_) => {}
            Err(_) => result.skipped += 1,
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    #[test]
    fn finds_supported_files_recursively_and_ignores_the_rest() {
        let dir = TempDir::new();
        let sub = dir.path().join("Album");
        fs::create_dir_all(&sub).unwrap();
        for p in [
            dir.path().join("b.mp3"),
            dir.path().join("cover.jpg"),
            dir.path().join("notes.txt"),
            sub.join("a.FLAC"),
            sub.join("c.wma"),
        ] {
            fs::write(p, b"x").unwrap();
        }
        let result = scan_folder(dir.path());
        let names: Vec<_> = result
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.FLAC", "c.wma", "b.mp3"]);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn a_missing_folder_is_reported_not_a_panic() {
        let dir = TempDir::new();
        let result = scan_folder(&dir.path().join("nope"));
        assert!(result.files.is_empty());
        assert_eq!(result.skipped, 1);
    }
}
