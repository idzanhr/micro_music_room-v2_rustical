// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! What the window shows: the track list, playlists, and search, with no UI toolkit involved.
//!
//! [`Browser`] owns the loaded [`Library`] and answers "which rows are visible right now". Every
//! change the user makes (add tracks, create a playlist...) goes through it so it can be tested
//! without a window, and each mutating method reports whether the library changed so the caller
//! knows when to save.

use crate::{
    library::{Library, Playlist},
    track::Track,
};
use std::{collections::HashSet, path::PathBuf};

/// Which list the table is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    AllTracks,
    /// The most recently added songs, newest first.
    Recent,
    Playlist(usize),
}

/// How many songs "Recently added" shows.
pub const RECENT_LIMIT: usize = 100;

/// A track plus the lower-cased, accent-folded text that search matches against. Computed once at
/// load so a keystroke over a 100k-song library is a plain substring scan.
#[derive(Debug, Clone)]
pub struct Entry {
    pub track: Track,
    key: String,
}

impl Entry {
    pub fn new(path: PathBuf) -> Self {
        let track = Track::from_path(path);
        let key = fold(&format!(
            "{} {} {}",
            track.title, track.artist, track.folder
        ));
        Self { track, key }
    }
}

/// Lower-case and strip the accents Croatian (and most Latin) text uses, so typing `slusam` finds
/// `slušam` and `dorde` finds `Đorđe`.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'č' | 'ć' | 'ç' => 'c',
            'š' | 'ś' => 's',
            'ž' | 'ź' | 'ż' => 'z',
            'đ' => 'd',
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Case-insensitive identity for a path. Windows paths are case-insensitive, and v1 files hold
/// whatever case the user's folder had, so `C:\Music\A.mp3` and `c:\music\a.mp3` are one file.
fn path_key(path: &std::path::Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[derive(Debug)]
pub struct Browser {
    library: Library,
    /// Mirrors `library.tracks`, same order.
    tracks: Vec<Entry>,
    /// One list per playlist, mirroring `library.playlists`.
    playlist_entries: Vec<Vec<Entry>>,
    known: HashSet<String>,
    view: View,
    search: String,
}

impl Browser {
    pub fn new(library: Library) -> Self {
        let tracks: Vec<Entry> = library.tracks.iter().cloned().map(Entry::new).collect();
        let playlist_entries = library
            .playlists
            .iter()
            .map(|p| p.paths.iter().cloned().map(Entry::new).collect())
            .collect();
        let known = library.tracks.iter().map(|p| path_key(p)).collect();
        Self {
            library,
            tracks,
            playlist_entries,
            known,
            view: View::AllTracks,
            search: String::new(),
        }
    }

    pub fn library(&self) -> &Library {
        &self.library
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn playlist_names(&self) -> impl Iterator<Item = (&str, usize)> {
        self.library
            .playlists
            .iter()
            .map(|p| (p.name.as_str(), p.paths.len()))
    }

    pub fn playlist_count(&self) -> usize {
        self.library.playlists.len()
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// How many tracks the current list holds, ignoring the search text.
    pub fn view_len(&self) -> usize {
        match self.view {
            View::AllTracks => self.tracks.len(),
            View::Recent => self.tracks.len().min(RECENT_LIMIT),
            View::Playlist(i) => self.playlist_entries[i].len(),
        }
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn set_view(&mut self, view: View) {
        self.view = match view {
            View::Playlist(i) if i >= self.library.playlists.len() => View::AllTracks,
            other => other,
        };
    }

    pub fn set_search(&mut self, text: &str) {
        self.search = fold(text.trim());
    }

    /// The rows to show: the current list narrowed by the search text. Every search word must
    /// appear somewhere in the title, artist or folder, in any order.
    pub fn visible(&self) -> Vec<&Entry> {
        let source: Vec<&Entry> = match self.view {
            View::AllTracks => self.tracks.iter().collect(),
            View::Recent => self.tracks.iter().rev().take(RECENT_LIMIT).collect(),
            View::Playlist(i) => self.playlist_entries[i].iter().collect(),
        };
        if self.search.is_empty() {
            return source;
        }
        let words: Vec<&str> = self.search.split_whitespace().collect();
        source
            .into_iter()
            .filter(|e| words.iter().all(|w| e.key.contains(w)))
            .collect()
    }

    /// Add files to the library, skipping ones already in it. Returns how many were new.
    pub fn add_tracks(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let mut added = 0;
        for path in paths {
            if self.known.insert(path_key(&path)) {
                self.tracks.push(Entry::new(path.clone()));
                self.library.tracks.push(path);
                added += 1;
            }
        }
        added
    }

    /// Create a playlist. Returns its index, or `None` for an empty or duplicate name.
    pub fn create_playlist(&mut self, name: &str) -> Option<usize> {
        let name = name.trim();
        let taken = self
            .library
            .playlists
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name));
        if name.is_empty() || taken {
            return None;
        }
        self.library.playlists.push(Playlist {
            name: name.to_owned(),
            ..Playlist::default()
        });
        self.playlist_entries.push(Vec::new());
        Some(self.library.playlists.len() - 1)
    }

    pub fn delete_playlist(&mut self, index: usize) -> bool {
        if index >= self.library.playlists.len() {
            return false;
        }
        self.library.playlists.remove(index);
        self.playlist_entries.remove(index);
        self.view = match self.view {
            View::Playlist(i) if i == index => View::AllTracks,
            View::Playlist(i) if i > index => View::Playlist(i - 1),
            other => other,
        };
        true
    }

    /// Add a track to a playlist. A track already in the playlist is left alone (returns false).
    pub fn add_to_playlist(&mut self, playlist: usize, path: &std::path::Path) -> bool {
        let Some(list) = self.library.playlists.get_mut(playlist) else {
            return false;
        };
        let key = path_key(path);
        if list.paths.iter().any(|p| path_key(p) == key) {
            return false;
        }
        list.paths.push(path.to_path_buf());
        self.playlist_entries[playlist].push(Entry::new(path.to_path_buf()));
        true
    }

    /// Remove one entry from a playlist by its position in the playlist (not in the filtered view).
    pub fn remove_from_playlist(&mut self, playlist: usize, position: usize) -> bool {
        let Some(list) = self.library.playlists.get_mut(playlist) else {
            return false;
        };
        if position >= list.paths.len() {
            return false;
        }
        list.paths.remove(position);
        self.playlist_entries[playlist].remove(position);
        true
    }

    /// Positions (within the playlist) of the rows [`visible`](Self::visible) returns, so a row
    /// the user clicked in a filtered view maps back to the right playlist entry.
    pub fn visible_positions(&self) -> Vec<usize> {
        let View::Playlist(i) = self.view else {
            return Vec::new();
        };
        let words: Vec<&str> = self.search.split_whitespace().collect();
        self.playlist_entries[i]
            .iter()
            .enumerate()
            .filter(|(_, e)| words.iter().all(|w| e.key.contains(w)))
            .map(|(pos, _)| pos)
            .collect()
    }
}

/// `m:ss`, or `h:mm:ss` past an hour. `None` shows as an em dash.
pub fn format_duration(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".to_owned(),
        Some(s) if s >= 3600 => format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60),
        Some(s) => format!("{}:{:02}", s / 60, s % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(paths: &[&str]) -> Library {
        Library {
            tracks: paths.iter().map(PathBuf::from).collect(),
            ..Library::default()
        }
    }

    fn titles(b: &Browser) -> Vec<String> {
        b.visible().iter().map(|e| e.track.title.clone()).collect()
    }

    #[test]
    fn folding_makes_croatian_text_searchable_without_diacritics() {
        assert_eq!(fold("I dalje slušam"), "i dalje slusam");
        assert_eq!(
            fold("Đorđe Balašević — Čuvaj se, ŽIVOT"),
            "dorde balasevic — cuvaj se, zivot"
        );
        assert_eq!(fold("Ćevapi"), "cevapi");
    }

    #[test]
    fn search_ignores_case_and_accents_and_matches_any_word_order() {
        let mut b = Browser::new(lib(&[
            "/m/Edo Maajka/Edo Maajka - I dalje slušam.flac",
            "/m/Oliver/Oliver - Kapetan bez broda.mp3",
        ]));
        b.set_search("SLUSAM");
        assert_eq!(titles(&b), ["I dalje slušam"]);
        b.set_search("broda kapetan");
        assert_eq!(titles(&b), ["Kapetan bez broda"]);
        b.set_search("oliver");
        assert_eq!(titles(&b), ["Kapetan bez broda"], "artist is searchable");
        b.set_search("nothing like this");
        assert!(b.visible().is_empty());
        b.set_search("  ");
        assert_eq!(b.visible().len(), 2, "blank search shows everything");
    }

    #[test]
    fn adding_tracks_skips_duplicates_ignoring_case() {
        let mut b = Browser::new(lib(&["C:\\Music\\A.mp3"]));
        let added = b.add_tracks(
            ["c:\\music\\a.mp3", "C:\\Music\\B.mp3", "C:\\Music\\B.mp3"].map(PathBuf::from),
        );
        assert_eq!(added, 1);
        assert_eq!(b.track_count(), 2);
        assert_eq!(
            b.library().tracks.len(),
            2,
            "the library that gets saved matches"
        );
    }

    #[test]
    fn playlists_can_be_created_filled_and_trimmed() {
        let mut b = Browser::new(lib(&["/m/a.mp3", "/m/b.mp3"]));
        assert_eq!(b.create_playlist("  Road trip "), Some(0));
        assert_eq!(b.create_playlist("road TRIP"), None, "duplicate name");
        assert_eq!(b.create_playlist("   "), None, "empty name");

        assert!(b.add_to_playlist(0, "/m/a.mp3".as_ref()));
        assert!(b.add_to_playlist(0, "/m/b.mp3".as_ref()));
        assert!(!b.add_to_playlist(0, "/m/A.MP3".as_ref()), "already in it");
        assert!(
            !b.add_to_playlist(5, "/m/a.mp3".as_ref()),
            "no such playlist"
        );

        b.set_view(View::Playlist(0));
        assert_eq!(b.visible().len(), 2);
        assert_eq!(b.playlist_names().next(), Some(("Road trip", 2)));

        assert!(b.remove_from_playlist(0, 0));
        assert_eq!(titles(&b), ["b"]);
        assert!(!b.remove_from_playlist(0, 9));
        assert_eq!(
            b.track_count(),
            2,
            "removing from a playlist never touches the library"
        );
    }

    #[test]
    fn search_inside_a_playlist_maps_rows_back_to_playlist_positions() {
        let mut b = Browser::new(lib(&[]));
        b.create_playlist("P");
        for p in ["/m/alpha.mp3", "/m/beta.mp3", "/m/alpine.mp3"] {
            b.add_to_playlist(0, p.as_ref());
        }
        b.set_view(View::Playlist(0));
        b.set_search("alp");
        assert_eq!(titles(&b), ["alpha", "alpine"]);
        assert_eq!(b.visible_positions(), [0, 2]);
        // The second visible row is playlist entry 2, so that is what gets removed.
        let position = b.visible_positions()[1];
        assert!(b.remove_from_playlist(0, position));
        b.set_search("");
        assert_eq!(titles(&b), ["alpha", "beta"]);
    }

    #[test]
    fn deleting_a_playlist_keeps_the_view_pointing_at_the_right_list() {
        let mut b = Browser::new(lib(&[]));
        for n in ["A", "B", "C"] {
            b.create_playlist(n);
        }
        b.set_view(View::Playlist(2));
        assert!(b.delete_playlist(0));
        assert_eq!(b.view(), View::Playlist(1), "C moved from 2 to 1");
        assert!(b.delete_playlist(1));
        assert_eq!(b.view(), View::AllTracks, "the viewed playlist is gone");
        assert!(!b.delete_playlist(7));
    }

    #[test]
    fn recently_added_shows_the_newest_first_and_stops_at_the_limit() {
        let paths: Vec<PathBuf> = (0..RECENT_LIMIT + 20)
            .map(|i| PathBuf::from(format!("/m/song {i:03}.mp3")))
            .collect();
        let mut b = Browser::new(Library {
            tracks: paths,
            ..Library::default()
        });
        b.set_view(View::Recent);
        let titles = titles(&b);
        assert_eq!(titles.len(), RECENT_LIMIT);
        assert_eq!(titles[0], "song 119", "the last one added comes first");
        assert_eq!(b.view_len(), RECENT_LIMIT);

        b.set_search("song 11");
        assert_eq!(
            b.visible().len(),
            10,
            "search narrows within the recent list"
        );

        // A new song lands at the top.
        b.set_search("");
        b.add_tracks([PathBuf::from("/m/brand new.mp3")]);
        assert_eq!(b.visible()[0].track.title, "brand new");
    }

    #[test]
    fn view_len_ignores_the_search() {
        let mut b = Browser::new(lib(&["/m/a.mp3", "/m/b.mp3", "/m/c.mp3"]));
        b.set_search("a");
        assert_eq!((b.visible().len(), b.view_len()), (1, 3));
    }

    #[test]
    fn an_out_of_range_view_falls_back_to_all_tracks() {
        let mut b = Browser::new(lib(&["/m/a.mp3"]));
        b.set_view(View::Playlist(3));
        assert_eq!(b.view(), View::AllTracks);
    }

    #[test]
    fn playlist_paths_not_in_the_library_still_show() {
        let library = Library {
            playlists: vec![Playlist {
                name: "P".into(),
                paths: vec!["/gone/x.mp3".into()],
                ..Default::default()
            }],
            ..Library::default()
        };
        let mut b = Browser::new(library);
        b.set_view(View::Playlist(0));
        assert_eq!(titles(&b), ["x"]);
    }

    #[test]
    fn durations_format_like_a_player() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(0)), "0:00");
        assert_eq!(format_duration(Some(65)), "1:05");
        assert_eq!(format_duration(Some(3599)), "59:59");
        assert_eq!(format_duration(Some(3661)), "1:01:01");
    }
}
