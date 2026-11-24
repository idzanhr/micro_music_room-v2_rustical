// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Glue between the window and the core logic: owns the state, pushes it into the Slint
//! `AppState` global, and turns each user action into a change on [`Browser`], [`Queue`] or the
//! library file.
//!
//! Playback is not connected yet. The transport buttons move the queue and flip the play/pause
//! state, and the UI says so, but no sound is produced until the audio step.

use crate::{AppState, MainWindow, PlaylistItem, TrackRow, dialog};
use micro_music_room::{
    library::{Library, default_library_path},
    model::{Browser, View, format_duration},
    queue::{Queue, Repeat, Rng},
    scan::{ScanResult, scan_folder},
    track::Track,
};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc, time::Duration};

const PLAYBACK_NOTE: &str = "Playback isn't connected yet: the controls move the queue only.";

pub struct App {
    browser: Browser,
    library_path: Option<PathBuf>,
    /// False when the library file could not be read. Nothing is ever written in that case, so a
    /// damaged file is never overwritten.
    can_save: bool,
    queue: Queue<PathBuf>,
    playing: bool,
    status: String,
    scanning: bool,
    volume: f32,
}

impl App {
    pub fn new(
        library: Library,
        library_path: Option<PathBuf>,
        can_save: bool,
        status: String,
    ) -> Self {
        Self {
            browser: Browser::new(library),
            library_path,
            can_save,
            queue: Queue::new(Rng::from_time()),
            playing: false,
            status,
            scanning: false,
            volume: 0.8,
        }
    }

    /// Load the v1 library read-only. `MMR_LIBRARY` points the app at another file, so it can be
    /// tried without touching the real one.
    pub fn load() -> Self {
        let path = std::env::var_os("MMR_LIBRARY")
            .map(PathBuf::from)
            .or_else(default_library_path);
        let Some(path) = path else {
            return Self::new(
                Library::default(),
                None,
                false,
                "No library location found; changes won't be kept.".into(),
            );
        };
        match Library::load(&path) {
            Ok(library) => Self::new(library, Some(path), true, String::new()),
            Err(e) => Self::new(
                Library::default(),
                Some(path),
                false,
                format!("{e}. The file was left untouched and changes won't be saved."),
            ),
        }
    }

    // ---- user actions -------------------------------------------------------------------

    fn select_view(&mut self, index: i32) {
        let view = usize::try_from(index).map_or(View::AllTracks, View::Playlist);
        self.browser.set_view(view);
    }

    fn search(&mut self, text: &str) {
        self.browser.set_search(text);
    }

    fn play_row(&mut self, row: i32) {
        let paths: Vec<PathBuf> = self
            .browser
            .visible()
            .iter()
            .map(|e| e.track.path.clone())
            .collect();
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        if self.queue.play(paths, Some(row)).is_some() {
            self.playing = true;
            self.status = PLAYBACK_NOTE.into();
        }
    }

    fn toggle_play(&mut self) {
        if self.queue.current().is_some() {
            self.playing = !self.playing;
        }
    }

    fn next(&mut self) {
        if self.queue.next(false).is_none() {
            self.playing = false;
        }
    }

    fn previous(&mut self) {
        self.queue.previous();
    }

    fn toggle_shuffle(&mut self) {
        self.queue.set_shuffle(!self.queue.shuffle());
    }

    fn cycle_repeat(&mut self) {
        self.queue.set_repeat(self.queue.repeat().cycle());
    }

    fn create_playlist(&mut self, name: &str) {
        match self.browser.create_playlist(name) {
            Some(index) => {
                self.browser.set_view(View::Playlist(index));
                self.status = format!("Created the playlist “{}”.", name.trim());
                self.save();
            }
            None => self.status = "Pick a name that isn't empty or already used.".into(),
        }
    }

    fn delete_playlist(&mut self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if self.browser.delete_playlist(index) {
            self.status = "Playlist deleted. Your tracks are untouched.".into();
            self.save();
        }
    }

    fn add_to_playlist(&mut self, row: i32, playlist: i32) {
        let (Ok(row), Ok(playlist)) = (usize::try_from(row), usize::try_from(playlist)) else {
            return;
        };
        let Some(path) = self
            .browser
            .visible()
            .get(row)
            .map(|e| e.track.path.clone())
        else {
            return;
        };
        let name = self
            .browser
            .playlist_names()
            .nth(playlist)
            .map(|(n, _)| n.to_owned())
            .unwrap_or_default();
        if self.browser.add_to_playlist(playlist, &path) {
            self.status = format!("Added to “{name}”.");
            self.save();
        } else {
            self.status = format!("Already in “{name}”.");
        }
    }

    fn remove_from_playlist(&mut self, row: i32) {
        let View::Playlist(playlist) = self.browser.view() else {
            return;
        };
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        // The row is a position in the filtered view; map it back to the playlist entry.
        let Some(&position) = self.browser.visible_positions().get(row) else {
            return;
        };
        if self.browser.remove_from_playlist(playlist, position) {
            self.status = "Removed from the playlist.".into();
            self.save();
        }
    }

    pub fn finish_scan(&mut self, result: ScanResult) {
        let found = result.files.len();
        let added = self.browser.add_tracks(result.files);
        self.scanning = false;
        self.status = match (added, found - added, result.skipped) {
            (0, 0, 0) => "No supported music files found in that folder.".into(),
            (0, _, _) => format!("Nothing new: all {found} tracks were already in your library."),
            (n, 0, 0) => format!("Added {n} tracks."),
            (n, dup, skipped) => {
                let mut text = format!("Added {n} tracks");
                if dup > 0 {
                    text += &format!(", {dup} already there");
                }
                if skipped > 0 {
                    text += &format!(", {skipped} unreadable");
                }
                text + "."
            }
        };
        self.save();
    }

    /// Atomic write with backups (see `Library::save`). Refused if the file could not be read.
    fn save(&mut self) {
        if !self.can_save {
            return;
        }
        let Some(path) = &self.library_path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = self.browser.library().save(path) {
            self.status = format!("Couldn't save your library: {e}");
        }
    }

    // ---- showing it ---------------------------------------------------------------------

    pub fn refresh(&self, ui: &MainWindow) {
        let state = ui.global::<AppState>();
        let current = self.queue.current();

        let visible = self.browser.visible();
        let shown = visible.len();
        let rows: Vec<TrackRow> = visible
            .iter()
            .map(|e| TrackRow {
                title: e.track.title.as_str().into(),
                artist: e.track.artist.as_str().into(),
                folder: e.track.folder.as_str().into(),
                playing: current == Some(&e.track.path),
            })
            .collect();
        state.set_rows(ModelRc::new(VecModel::from(rows)));

        let playlists: Vec<PlaylistItem> = self
            .browser
            .playlist_names()
            .map(|(name, count)| PlaylistItem {
                name: name.into(),
                count: count as i32,
            })
            .collect();
        state.set_playlists(ModelRc::new(VecModel::from(playlists)));

        let (index, title) = match self.browser.view() {
            View::AllTracks => (-1, "All tracks".to_owned()),
            View::Playlist(i) => (
                i as i32,
                self.browser
                    .playlist_names()
                    .nth(i)
                    .map(|(n, _)| n.to_owned())
                    .unwrap_or_default(),
            ),
        };
        let in_view = self.browser.view_len();
        let plural = |n: usize| if n == 1 { "track" } else { "tracks" };
        let subtitle = if shown == in_view {
            format!("{in_view} {}", plural(in_view))
        } else {
            format!("{shown} of {in_view} {}", plural(in_view))
        };
        state.set_view_index(index);
        state.set_view_title(title.as_str().into());
        state.set_view_subtitle(subtitle.as_str().into());
        state.set_total_tracks(self.browser.track_count() as i32);
        state.set_status(self.status.as_str().into());
        state.set_scanning(self.scanning);

        state.set_has_current(current.is_some());
        let now = current.map(|p| Track::from_path(p.clone()));
        state.set_now_title(now.as_ref().map_or("", |t| t.title.as_str()).into());
        state.set_now_artist(now.as_ref().map_or("", |t| t.artist.as_str()).into());
        state.set_playing(self.playing);
        state.set_shuffle(self.queue.shuffle());
        state.set_repeat_mode(match self.queue.repeat() {
            Repeat::Off => 0,
            Repeat::All => 1,
            Repeat::One => 2,
        });
        // No engine yet, so there is no position or length to report.
        state.set_elapsed("0:00".into());
        state.set_total(format_duration(None).as_str().into());
        state.set_volume(self.volume);
    }
}

/// Connect every callback. The returned timer must be kept alive for as long as the window runs:
/// it is what hands finished folder scans back to the UI thread.
pub fn wire(ui: &MainWindow, app: Rc<RefCell<App>>) -> Timer {
    app.borrow().refresh(ui);
    let state = ui.global::<AppState>();
    let weak = ui.as_weak();

    // Run a change, then redraw.
    macro_rules! on {
        ($setter:ident, |$a:ident $(, $arg:ident)*| $body:expr) => {{
            let (app, weak) = (app.clone(), weak.clone());
            state.$setter(move |$($arg),*| {
                {
                    let mut $a = app.borrow_mut();
                    $body;
                }
                if let Some(ui) = weak.upgrade() {
                    app.borrow().refresh(&ui);
                }
            });
        }};
    }

    on!(on_select_view, |a, i| a.select_view(i));
    on!(on_search_edited, |a, text| a.search(&text));
    on!(on_play_row, |a, row| a.play_row(row));
    on!(on_toggle_play, |a| a.toggle_play());
    on!(on_next, |a| a.next());
    on!(on_previous, |a| a.previous());
    on!(on_toggle_shuffle, |a| a.toggle_shuffle());
    on!(on_cycle_repeat, |a| a.cycle_repeat());
    on!(on_create_playlist, |a, name| a.create_playlist(&name));
    on!(on_delete_playlist, |a, i| a.delete_playlist(i));
    on!(on_add_to_playlist, |a, row, playlist| a
        .add_to_playlist(row, playlist));
    on!(on_remove_from_playlist, |a, row| a
        .remove_from_playlist(row));
    on!(on_volume_changed, |a, v| a.volume = v);
    // The seek bar moves itself; there is nothing to seek until playback exists.
    state.on_seek(|_| {});

    // Adding a folder: pick it, scan on a background thread, then merge on the UI thread.
    let (tx, rx) = mpsc::channel::<ScanResult>();
    {
        let (app, weak) = (app.clone(), weak.clone());
        state.on_add_folder(move || {
            if app.borrow().scanning {
                return;
            }
            let Some(folder) = dialog::pick_folder() else {
                return;
            };
            {
                let mut a = app.borrow_mut();
                a.scanning = true;
                a.status = format!("Scanning {}…", folder.display());
            }
            if let Some(ui) = weak.upgrade() {
                app.borrow().refresh(&ui);
            }
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(scan_folder(&folder));
            });
        });
    }

    let timer = Timer::default();
    let (app, weak) = (app, weak);
    timer.start(TimerMode::Repeated, Duration::from_millis(150), move || {
        let mut changed = false;
        while let Ok(result) = rx.try_recv() {
            app.borrow_mut().finish_scan(result);
            changed = true;
        }
        if changed && let Some(ui) = weak.upgrade() {
            app.borrow().refresh(&ui);
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use micro_music_room::library::Playlist;

    fn app(paths: &[&str]) -> App {
        let library = Library {
            tracks: paths.iter().map(PathBuf::from).collect(),
            ..Library::default()
        };
        App::new(library, None, false, String::new())
    }

    fn window() -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        MainWindow::new().expect("window")
    }

    fn row_titles(ui: &MainWindow) -> Vec<String> {
        use slint::Model;
        let rows = ui.global::<AppState>().get_rows();
        rows.iter().map(|r| r.title.to_string()).collect()
    }

    #[test]
    fn rows_playlists_and_counts_reach_the_window() {
        let ui = window();
        let mut a = app(&[
            "/m/Edo Maajka - I dalje slušam.flac",
            "/m/Oliver - Kapetan bez broda.mp3",
        ]);
        a.browser.create_playlist("Mix");
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert_eq!(row_titles(&ui), ["I dalje slušam", "Kapetan bez broda"]);
        assert_eq!(state.get_total_tracks(), 2);
        assert_eq!(state.get_view_subtitle(), "2 tracks");
        assert_eq!(state.get_view_index(), -1);

        a.search("SLUSAM");
        a.refresh(&ui);
        assert_eq!(row_titles(&ui), ["I dalje slušam"]);
        assert_eq!(state.get_view_subtitle(), "1 of 2 tracks");
    }

    #[test]
    fn double_click_starts_the_queue_on_that_row_and_marks_it_playing() {
        use slint::Model;
        let ui = window();
        let mut a = app(&["/m/a - one.mp3", "/m/a - two.mp3", "/m/a - three.mp3"]);
        a.play_row(1);
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert!(state.get_has_current() && state.get_playing());
        assert_eq!(state.get_now_title(), "two");
        let flags: Vec<bool> = state.get_rows().iter().map(|r| r.playing).collect();
        assert_eq!(flags, [false, true, false]);

        a.next();
        a.refresh(&ui);
        assert_eq!(state.get_now_title(), "three");
        a.previous();
        a.previous();
        a.refresh(&ui);
        assert_eq!(state.get_now_title(), "one");
    }

    #[test]
    fn shuffle_starts_on_the_clicked_track_through_the_ui_path() {
        let ui = window();
        for _ in 0..30 {
            let mut a = app(&[
                "/m/a - 1.mp3",
                "/m/a - 2.mp3",
                "/m/a - 3.mp3",
                "/m/a - 4.mp3",
                "/m/a - 5.mp3",
            ]);
            a.toggle_shuffle();
            a.play_row(3);
            a.refresh(&ui);
            assert_eq!(ui.global::<AppState>().get_now_title(), "4");
        }
    }

    #[test]
    fn removing_a_row_in_a_filtered_playlist_removes_the_right_entry() {
        let ui = window();
        let mut a = app(&[]);
        a.create_playlist("P");
        for p in ["/m/alpha.mp3", "/m/beta.mp3", "/m/alpine.mp3"] {
            a.browser.add_to_playlist(0, p.as_ref());
        }
        a.search("alp");
        a.remove_from_playlist(1); // second visible row = "alpine"
        a.search("");
        a.refresh(&ui);
        assert_eq!(row_titles(&ui), ["alpha", "beta"]);
    }

    #[test]
    fn add_to_playlist_reports_duplicates() {
        let mut a = app(&["/m/a.mp3"]);
        a.create_playlist("P");
        a.select_view(-1);
        a.add_to_playlist(0, 0);
        assert_eq!(a.status, "Added to “P”.");
        a.add_to_playlist(0, 0);
        assert_eq!(a.status, "Already in “P”.");
    }

    #[test]
    fn a_finished_scan_adds_new_tracks_and_says_what_happened() {
        let mut a = app(&["/m/old.mp3"]);
        a.scanning = true;
        a.finish_scan(ScanResult {
            files: vec!["/m/old.mp3".into(), "/m/new.mp3".into()],
            skipped: 0,
        });
        assert!(!a.scanning);
        assert_eq!(a.browser.track_count(), 2);
        assert_eq!(a.status, "Added 1 tracks, 1 already there.");

        a.finish_scan(ScanResult {
            files: vec!["/m/new.mp3".into()],
            skipped: 0,
        });
        assert_eq!(
            a.status,
            "Nothing new: all 1 tracks were already in your library."
        );
        a.finish_scan(ScanResult::default());
        assert_eq!(a.status, "No supported music files found in that folder.");
    }

    #[test]
    fn changes_are_saved_with_backups_and_a_damaged_library_is_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("mmr-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("library.json");
        let original = r#"{"Tracks":["/m/a.mp3"],"Playlists":[]}"#;
        std::fs::write(&file, original).unwrap();

        // A readable library saves, keeping the pristine original as a backup.
        let library = Library::load(&file).unwrap();
        let mut a = App::new(library, Some(file.clone()), true, String::new());
        a.create_playlist("New");
        assert_eq!(
            std::fs::read_to_string(dir.join("library.v1.json.bak")).unwrap(),
            original
        );
        assert_eq!(Library::load(&file).unwrap().playlists[0].name, "New");

        // An unreadable one is never written.
        std::fs::write(&file, b"{ broken").unwrap();
        let mut b = App::new(Library::default(), Some(file.clone()), false, String::new());
        b.create_playlist("Nope");
        assert_eq!(std::fs::read(&file).unwrap(), b"{ broken");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = Playlist::default();
    }
}
