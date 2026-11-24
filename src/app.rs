// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Glue between the window and the core logic: owns the state, pushes it into the Slint
//! `AppState` global, and turns each user action into a change on [`Browser`], [`Queue`] or the
//! library file.
//!
//! Playback goes through an [`Output`]: the real one on Windows, a silent one in tests. A timer
//! (see [`wire`]) moves the seek bar and starts the next song when one ends.

use crate::{AppState, MainWindow, PlaylistItem, TrackRow, dialog};
use micro_music_room::{
    engine::{LoadError, Output, RodioOutput},
    library::{Library, default_library_path},
    model::{Browser, View, format_duration},
    queue::{Queue, Repeat, Rng},
    scan::{ScanResult, scan_folder},
    track::{Track, is_supported},
};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell, collections::BTreeSet, path::PathBuf, rc::Rc, sync::mpsc, time::Duration,
};

const DEFAULT_VOLUME: f32 = 0.8;
/// Unplayable files in a row before playback gives up instead of skipping on through the list.
const MAX_SKIPS: usize = 3;
/// "Previous" restarts the song instead of going back once it has played this long.
const RESTART_AFTER: Duration = Duration::from_secs(3);

pub struct App {
    browser: Browser,
    library_path: Option<PathBuf>,
    /// False when the library file could not be read. Nothing is ever written in that case, so a
    /// damaged file is never overwritten.
    can_save: bool,
    queue: Queue<PathBuf>,
    /// Selected rows, as indexes into the visible list.
    selection: BTreeSet<usize>,
    /// The row a shift-click extends from.
    anchor: Option<usize>,
    playing: bool,
    status: String,
    scanning: bool,
    volume: f32,
    output: Box<dyn Output>,
    /// The file the output has loaded, and its length when the format reports one.
    loaded: Option<PathBuf>,
    length: Option<Duration>,
}

impl App {
    pub fn new(
        library: Library,
        library_path: Option<PathBuf>,
        can_save: bool,
        status: String,
        output: Box<dyn Output>,
    ) -> Self {
        Self {
            browser: Browser::new(library),
            library_path,
            can_save,
            queue: Queue::new(Rng::from_time()),
            selection: BTreeSet::new(),
            anchor: None,
            playing: false,
            status,
            scanning: false,
            volume: DEFAULT_VOLUME,
            output,
            loaded: None,
            length: None,
        }
    }

    /// Load the v1 library read-only. `MMR_LIBRARY` points the app at another file, so it can be
    /// tried without touching the real one.
    pub fn load() -> Self {
        let output = || Box::new(RodioOutput::new(DEFAULT_VOLUME)) as Box<dyn Output>;
        let path = std::env::var_os("MMR_LIBRARY")
            .map(PathBuf::from)
            .or_else(default_library_path);
        let Some(path) = path else {
            return Self::new(
                Library::default(),
                None,
                false,
                "No library location found; changes won't be kept.".into(),
                output(),
            );
        };
        match Library::load(&path) {
            Ok(library) => Self::new(library, Some(path), true, String::new(), output()),
            Err(e) => Self::new(
                Library::default(),
                Some(path),
                false,
                format!("{e}. The file was left untouched and changes won't be saved."),
                output(),
            ),
        }
    }

    // ---- user actions -------------------------------------------------------------------

    fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
    }

    fn select_view(&mut self, index: i32) {
        let view = match index {
            -1 => View::AllTracks,
            -2 => View::Recent,
            i => usize::try_from(i).map_or(View::AllTracks, View::Playlist),
        };
        self.browser.set_view(view);
        self.clear_selection();
    }

    fn search(&mut self, text: &str) {
        self.browser.set_search(text);
        self.clear_selection();
    }

    /// Plain click selects one row, Ctrl toggles it, Shift extends from the last plain click.
    fn select_row(&mut self, row: i32, ctrl: bool, shift: bool) {
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        if row >= self.browser.visible().len() {
            return;
        }
        if shift {
            let from = self.anchor.unwrap_or(row);
            let (lo, hi) = (from.min(row), from.max(row));
            if !ctrl {
                self.selection.clear();
            }
            self.selection.extend(lo..=hi);
        } else if ctrl {
            if !self.selection.remove(&row) {
                self.selection.insert(row);
            }
            self.anchor = Some(row);
        } else {
            self.selection.clear();
            self.selection.insert(row);
            self.anchor = Some(row);
        }
    }

    /// The rows an action applies to: `-1` is the selection; a row inside the selection means the
    /// whole selection (as in Explorer); any other row means just that row.
    fn rows_for(&self, row: i32) -> Vec<usize> {
        match usize::try_from(row) {
            Err(_) => self.selection.iter().copied().collect(),
            Ok(r) if self.selection.contains(&r) => self.selection.iter().copied().collect(),
            Ok(r) => vec![r],
        }
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
            self.start_current();
        }
    }

    /// Play everything in the list as shown. With shuffle on, the order is shuffled.
    fn play_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .browser
            .visible()
            .iter()
            .map(|e| e.track.path.clone())
            .collect();
        if self.queue.play(paths, None).is_some() {
            self.start_current();
        }
    }

    /// Load the queue's current song into the output and start it. A file that won't decode is
    /// reported and skipped; a missing sound device or a run of bad files stops playback.
    fn start_current(&mut self) {
        let mut failures = 0;
        while let Some(path) = self.queue.current().cloned() {
            match self.output.load(&path) {
                Ok(length) => {
                    self.length = length;
                    self.loaded = Some(path);
                    self.playing = true;
                    return;
                }
                Err(error) => {
                    let name = Track::from_path(path).title;
                    self.status = match &error {
                        LoadError::NoDevice(m) => m.clone(),
                        LoadError::BadFile(m) => format!("Couldn't play “{name}”: {m}"),
                    };
                    failures += 1;
                    let skippable = matches!(error, LoadError::BadFile(_)) && failures < MAX_SKIPS;
                    if !skippable || self.queue.next(false).is_none() {
                        break;
                    }
                }
            }
        }
        self.stop();
    }

    /// Silence, with the queue left where it is so play can start it again.
    fn stop(&mut self) {
        self.output.stop();
        self.loaded = None;
        self.length = None;
        self.playing = false;
    }

    fn toggle_play(&mut self) {
        if self.queue.current().is_none() {
            return;
        }
        if self.loaded.is_none() {
            self.start_current();
        } else if self.playing {
            self.output.pause();
            self.playing = false;
        } else {
            self.output.resume();
            self.playing = true;
        }
    }

    fn next(&mut self) {
        if self.queue.next(false).is_some() {
            self.start_current();
        } else {
            self.stop();
        }
    }

    /// Past the first few seconds this restarts the song, as most players do; otherwise it goes
    /// back one.
    fn previous(&mut self) {
        if self.loaded.is_some() && self.output.position() > RESTART_AFTER {
            if let Err(e) = self.output.seek(Duration::ZERO) {
                self.status = format!("Couldn't restart the song: {e}");
            }
            return;
        }
        if self.queue.previous().is_some() {
            self.start_current();
        }
    }

    /// Move the playback position to a fraction (0.0 to 1.0) of the song.
    fn seek(&mut self, fraction: f32) {
        let Some(length) = self.length.filter(|_| self.loaded.is_some()) else {
            return;
        };
        let target = length.mul_f32(fraction.clamp(0.0, 1.0));
        if let Err(e) = self.output.seek(target) {
            self.status = format!("Couldn't seek in this file: {e}");
        }
    }

    fn set_volume(&mut self, slider: f32) {
        self.volume = slider.clamp(0.0, 1.0);
        self.output.set_volume(self.volume);
    }

    /// Called by the timer. Starts the next song when the current one has played out. Returns true
    /// if the song changed, so the caller redraws the list's "playing" marker.
    pub fn tick(&mut self) -> bool {
        if !self.playing || self.loaded.is_none() || !self.output.finished() {
            return false;
        }
        if self.queue.next(true).is_some() {
            self.start_current();
        } else {
            self.stop();
        }
        true
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
                self.clear_selection();
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
            self.clear_selection();
            self.status = "Playlist deleted. Your songs are untouched.".into();
            self.save();
        }
    }

    fn add_to_playlist(&mut self, row: i32, playlist: i32) {
        let Ok(playlist) = usize::try_from(playlist) else {
            return;
        };
        let paths: Vec<PathBuf> = {
            let visible = self.browser.visible();
            self.rows_for(row)
                .into_iter()
                .filter_map(|r| visible.get(r).map(|e| e.track.path.clone()))
                .collect()
        };
        if paths.is_empty() {
            return;
        }
        let name = self
            .browser
            .playlist_names()
            .nth(playlist)
            .map(|(n, _)| n.to_owned())
            .unwrap_or_default();
        let added = paths
            .iter()
            .filter(|p| self.browser.add_to_playlist(playlist, p))
            .count();
        let songs = |n: usize| if n == 1 { "song" } else { "songs" };
        self.status = match (added, paths.len() - added) {
            (0, _) if paths.len() == 1 => format!("Already in “{name}”."),
            (0, _) => format!("All {} songs were already in “{name}”.", paths.len()),
            (n, 0) => format!("Added {n} {} to “{name}”.", songs(n)),
            (n, dup) => format!("Added {n} {} to “{name}”, {dup} already there.", songs(n)),
        };
        if added > 0 {
            self.save();
        }
    }

    fn remove_from_playlist(&mut self, row: i32) {
        let View::Playlist(playlist) = self.browser.view() else {
            return;
        };
        // Rows are positions in the filtered view; map them back to playlist entries.
        let positions = self.browser.visible_positions();
        let mut targets: Vec<usize> = self
            .rows_for(row)
            .into_iter()
            .filter_map(|r| positions.get(r).copied())
            .collect();
        // Highest first, so earlier removals don't shift the later ones.
        targets.sort_unstable_by(|a, b| b.cmp(a));
        let removed = targets
            .into_iter()
            .filter(|&p| self.browser.remove_from_playlist(playlist, p))
            .count();
        if removed > 0 {
            self.clear_selection();
            self.status = format!(
                "Removed {removed} {} from the playlist.",
                if removed == 1 { "song" } else { "songs" }
            );
            self.save();
        }
    }

    /// Add individually chosen files. Anything that isn't a supported music file is ignored.
    fn add_files(&mut self, files: Vec<PathBuf>) {
        let found = files.iter().filter(|p| is_supported(p)).count();
        if files.is_empty() {
            return;
        }
        let added = self
            .browser
            .add_tracks(files.into_iter().filter(|p| is_supported(p)));
        self.status = match (found, added) {
            (0, _) => "None of those were music files this player supports.".into(),
            (_, 0) => "Nothing new: those songs were already in your library.".into(),
            (_, n) => format!("Added {n} {}.", if n == 1 { "song" } else { "songs" }),
        };
        self.save();
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
            .enumerate()
            .map(|(i, e)| TrackRow {
                title: e.track.title.as_str().into(),
                artist: if e.track.artist.is_empty() {
                    "—".into()
                } else {
                    e.track.artist.as_str().into()
                },
                // Real album tags come with tag reading; until then the folder stands in.
                album: e.track.folder.as_str().into(),
                duration: format_duration(None).as_str().into(),
                playing: current == Some(&e.track.path),
                selected: self.selection.contains(&i),
            })
            .collect();
        state.set_rows(ModelRc::new(VecModel::from(rows)));

        let playlists: Vec<PlaylistItem> = self
            .browser
            .playlist_names()
            .map(|(name, _)| PlaylistItem { name: name.into() })
            .collect();
        state.set_playlists(ModelRc::new(VecModel::from(playlists)));

        let (index, title, crumb) = match self.browser.view() {
            View::AllTracks => (
                -1,
                "All songs".to_owned(),
                "YOUR LIBRARY  /  ALL SONGS".to_owned(),
            ),
            View::Recent => (
                -2,
                "Recently added".to_owned(),
                "YOUR LIBRARY  /  RECENTLY ADDED".to_owned(),
            ),
            View::Playlist(i) => {
                let name = self
                    .browser
                    .playlist_names()
                    .nth(i)
                    .map(|(n, _)| n.to_owned())
                    .unwrap_or_default();
                let crumb = format!("PLAYLISTS  /  {}", name.to_uppercase());
                (i as i32, name, crumb)
            }
        };
        let in_view = self.browser.view_len();
        let plural = |n: usize| if n == 1 { "song" } else { "songs" };
        let subtitle = if shown == in_view {
            format!("{in_view} {}", plural(in_view))
        } else {
            format!("{shown} of {in_view} {}", plural(in_view))
        };
        state.set_view_index(index);
        state.set_view_title(title.as_str().into());
        state.set_breadcrumb(crumb.as_str().into());
        state.set_view_subtitle(subtitle.as_str().into());
        state.set_total_tracks(self.browser.track_count() as i32);
        state.set_selected_count(self.selection.len() as i32);
        state.set_status(self.status.as_str().into());
        state.set_scanning(self.scanning);

        self.refresh_player(ui);
    }

    /// Just the player bar. Cheap enough to run several times a second, unlike [`refresh`].
    pub fn refresh_player(&self, ui: &MainWindow) {
        let state = ui.global::<AppState>();
        let current = self.queue.current();
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

        let position = if self.loaded.is_some() {
            self.output.position()
        } else {
            Duration::ZERO
        };
        let progress = self
            .length
            .filter(|l| !l.is_zero())
            .map_or(0.0, |l| (position.as_secs_f32() / l.as_secs_f32()).min(1.0));
        state.set_seekable(self.length.is_some());
        state.set_progress(progress);
        state.set_elapsed(format_duration(Some(position.as_secs())).as_str().into());
        state.set_total(
            format_duration(self.length.map(|l| l.as_secs()))
                .as_str()
                .into(),
        );
        state.set_volume(self.volume);
    }
}

/// Connect every callback. The returned timer must be kept alive for as long as the window runs:
/// it is what hands finished folder scans back to the UI thread.
pub fn wire(ui: &MainWindow, app: Rc<RefCell<App>>) -> Timer {
    app.borrow().refresh(ui);
    let state = ui.global::<AppState>();
    state.set_version(env!("CARGO_PKG_VERSION").into());
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
    on!(on_select_row, |a, row, ctrl, shift| a
        .select_row(row, ctrl, shift));
    on!(on_play_row, |a, row| a.play_row(row));
    on!(on_play_all, |a| a.play_all());
    on!(on_add_files, |a| a.add_files(dialog::pick_files()));
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
    on!(on_volume_changed, |a, v| a.set_volume(v));
    on!(on_seek, |a, f| a.seek(f));

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
        changed |= app.borrow_mut().tick();
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if changed {
            app.borrow().refresh(&ui);
        } else if app.borrow().playing {
            app.borrow().refresh_player(&ui);
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use micro_music_room::{
        engine::{SilentOutput, SilentState},
        library::Playlist,
    };

    fn app(paths: &[&str]) -> App {
        let library = Library {
            tracks: paths.iter().map(PathBuf::from).collect(),
            ..Library::default()
        };
        App::new(library, None, false, String::new(), silent().0)
    }

    fn silent() -> (Box<dyn Output>, Rc<RefCell<SilentState>>) {
        let (output, state) = SilentOutput::new();
        (Box::new(output), state)
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
        assert_eq!(state.get_view_subtitle(), "2 songs");
        assert_eq!(state.get_view_index(), -1);

        a.search("SLUSAM");
        a.refresh(&ui);
        assert_eq!(row_titles(&ui), ["I dalje slušam"]);
        assert_eq!(state.get_view_subtitle(), "1 of 2 songs");
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
        assert_eq!(a.status, "Added 1 song to “P”.");
        a.add_to_playlist(0, 0);
        assert_eq!(a.status, "Already in “P”.");
    }

    #[test]
    fn clicking_selects_ctrl_toggles_and_shift_extends() {
        let ui = window();
        let mut a = app(&["/m/1.mp3", "/m/2.mp3", "/m/3.mp3", "/m/4.mp3", "/m/5.mp3"]);
        a.select_row(1, false, false);
        a.select_row(3, false, true);
        assert_eq!(a.selection.iter().copied().collect::<Vec<_>>(), [1, 2, 3]);
        a.select_row(2, true, false);
        assert_eq!(a.selection.iter().copied().collect::<Vec<_>>(), [1, 3]);
        a.select_row(4, false, false);
        assert_eq!(a.selection.iter().copied().collect::<Vec<_>>(), [4]);

        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert_eq!(state.get_selected_count(), 1);

        a.search("2");
        assert!(a.selection.is_empty(), "a new search clears the selection");
    }

    #[test]
    fn add_to_playlist_takes_the_whole_selection_and_skips_what_is_there() {
        let mut a = app(&["/m/a.mp3", "/m/b.mp3", "/m/c.mp3"]);
        a.create_playlist("P");
        a.select_view(-1);
        a.select_row(0, false, false);
        a.select_row(1, true, false);
        a.add_to_playlist(-1, 0);
        assert_eq!(a.status, "Added 2 songs to “P”.");
        a.select_row(2, true, false);
        a.add_to_playlist(-1, 0);
        assert_eq!(a.status, "Added 1 song to “P”, 2 already there.");
        // A row outside the selection means just that row.
        a.select_row(0, false, false);
        a.add_to_playlist(2, 0);
        assert_eq!(a.status, "Already in “P”.");
    }

    #[test]
    fn removing_a_selection_from_a_playlist_removes_every_selected_entry() {
        let ui = window();
        let mut a = app(&[]);
        a.create_playlist("P");
        for p in ["/m/a.mp3", "/m/b.mp3", "/m/c.mp3", "/m/d.mp3"] {
            a.browser.add_to_playlist(0, p.as_ref());
        }
        a.select_row(0, false, false);
        a.select_row(2, true, false);
        a.remove_from_playlist(2);
        a.refresh(&ui);
        assert_eq!(row_titles(&ui), ["b", "d"]);
        assert_eq!(a.status, "Removed 2 songs from the playlist.");
    }

    #[test]
    fn recently_added_view_and_breadcrumbs_reach_the_window() {
        let ui = window();
        let mut a = app(&["/m/old.mp3", "/m/new.mp3"]);
        a.select_view(-2);
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert_eq!(row_titles(&ui), ["new", "old"]);
        assert_eq!(state.get_view_index(), -2);
        assert_eq!(state.get_view_title(), "Recently added");
        assert_eq!(state.get_breadcrumb(), "YOUR LIBRARY  /  RECENTLY ADDED");

        a.create_playlist("Road trip");
        a.refresh(&ui);
        assert_eq!(state.get_breadcrumb(), "PLAYLISTS  /  ROAD TRIP");
    }

    #[test]
    fn play_all_starts_from_the_top_and_shuffles_when_asked() {
        let ui = window();
        let mut a = app(&["/m/1.mp3", "/m/2.mp3", "/m/3.mp3"]);
        a.play_all();
        a.refresh(&ui);
        assert_eq!(ui.global::<AppState>().get_now_title(), "1");

        let mut seen = std::collections::HashSet::new();
        for _ in 0..40 {
            let mut b = app(&["/m/1.mp3", "/m/2.mp3", "/m/3.mp3", "/m/4.mp3"]);
            b.toggle_shuffle();
            b.play_all();
            seen.insert(b.queue.current().cloned());
        }
        assert!(
            seen.len() > 1,
            "shuffled play-all should not always start on row one"
        );
    }

    #[test]
    fn added_files_are_filtered_to_supported_music() {
        let mut a = app(&["/m/old.mp3"]);
        a.add_files(vec!["/m/new.flac".into(), "/m/cover.jpg".into()]);
        assert_eq!(a.browser.track_count(), 2);
        assert_eq!(a.status, "Added 1 song.");
        a.add_files(vec!["/m/cover.jpg".into()]);
        assert_eq!(
            a.status,
            "None of those were music files this player supports."
        );
        a.add_files(vec!["/m/NEW.FLAC".into()]);
        assert_eq!(
            a.status,
            "Nothing new: those songs were already in your library."
        );
    }

    fn playing_app(paths: &[&str]) -> (App, Rc<RefCell<SilentState>>) {
        let library = Library {
            tracks: paths.iter().map(PathBuf::from).collect(),
            ..Library::default()
        };
        let (output, state) = silent();
        (App::new(library, None, false, String::new(), output), state)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn play_pause_and_resume_drive_the_output() {
        let (mut a, out) = playing_app(&["/m/a - one.mp3", "/m/a - two.mp3"]);
        a.play_row(1);
        assert!(a.playing);
        assert_eq!(
            out.borrow().loaded.as_deref(),
            Some("/m/a - two.mp3".as_ref())
        );

        a.toggle_play();
        assert!(!a.playing && out.borrow().paused);
        a.toggle_play();
        assert!(a.playing && !out.borrow().paused);
        assert_eq!(out.borrow().loads, 1, "resuming does not reload the file");
    }

    #[test]
    fn play_with_nothing_loaded_starts_the_queued_song() {
        let (mut a, out) = playing_app(&["/m/one.mp3", "/m/two.mp3"]);
        a.toggle_play();
        assert!(
            !a.playing && out.borrow().loaded.is_none(),
            "empty queue: nothing to play"
        );
        a.play_all();
        a.stop();
        assert!(out.borrow().loaded.is_none());
        a.toggle_play();
        assert!(a.playing);
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/one.mp3".as_ref()));
    }

    #[test]
    fn a_song_that_plays_out_starts_the_next_one() {
        let ui = window();
        let (mut a, out) = playing_app(&["/m/one.mp3", "/m/two.mp3"]);
        a.play_row(0);
        assert!(!a.tick(), "still playing");
        out.borrow_mut().ended = true;
        assert!(a.tick());
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/two.mp3".as_ref()));
        a.refresh(&ui);
        assert_eq!(ui.global::<AppState>().get_now_title(), "two");

        // The last song ending stops playback; the queue stays on it.
        out.borrow_mut().ended = true;
        assert!(a.tick());
        assert!(!a.playing && out.borrow().loaded.is_none());
        assert!(a.queue.current().is_some());
    }

    #[test]
    fn repeat_one_replays_and_repeat_all_wraps() {
        let (mut a, out) = playing_app(&["/m/one.mp3", "/m/two.mp3"]);
        a.play_row(1);
        a.cycle_repeat(); // all
        out.borrow_mut().ended = true;
        a.tick();
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/one.mp3".as_ref()));
        a.cycle_repeat(); // one
        out.borrow_mut().ended = true;
        a.tick();
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/one.mp3".as_ref()));
        assert_eq!(out.borrow().loads, 3);
    }

    #[test]
    fn previous_restarts_late_in_a_song_and_goes_back_early() {
        let (mut a, out) = playing_app(&["/m/one.mp3", "/m/two.mp3", "/m/three.mp3"]);
        a.play_row(2);
        out.borrow_mut().position = secs(30);
        a.previous();
        assert_eq!(out.borrow().position, Duration::ZERO, "restarted");
        assert_eq!(
            out.borrow().loaded.as_deref(),
            Some("/m/three.mp3".as_ref())
        );
        a.previous();
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/two.mp3".as_ref()));
    }

    #[test]
    fn seeking_uses_a_fraction_of_the_length_and_shows_in_the_bar() {
        let ui = window();
        let (mut a, out) = playing_app(&["/m/one.mp3"]);
        a.play_row(0); // the silent output reports 200 s
        a.seek(0.25);
        assert_eq!(out.borrow().position, secs(50));
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert!((state.get_progress() - 0.25).abs() < 1e-3);
        assert_eq!(state.get_elapsed(), "0:50");
        assert_eq!(state.get_total(), "3:20");
        assert!(state.get_seekable());
        a.seek(7.0);
        assert_eq!(out.borrow().position, secs(200), "clamped to the end");
    }

    #[test]
    fn a_file_without_a_known_length_cannot_be_seeked() {
        let ui = window();
        let (mut a, out) = playing_app(&["/m/one.mp3"]);
        out.borrow_mut().length = None;
        a.play_row(0);
        a.seek(0.5);
        assert_eq!(out.borrow().position, Duration::ZERO);
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert!(!state.get_seekable());
        assert_eq!(state.get_total(), "—");
    }

    #[test]
    fn volume_reaches_the_output_and_the_slider() {
        let ui = window();
        let (mut a, out) = playing_app(&["/m/one.mp3"]);
        a.set_volume(0.3);
        a.set_volume(9.0);
        assert_eq!(out.borrow().slider_volume, 1.0, "clamped");
        a.set_volume(0.3);
        a.refresh(&ui);
        assert!((ui.global::<AppState>().get_volume() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn unplayable_files_are_skipped_with_a_message() {
        let (mut a, out) = playing_app(&["/m/bad.mp3", "/m/good.mp3"]);
        out.borrow_mut().broken.push("/m/bad.mp3".into());
        a.play_row(0);
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/good.mp3".as_ref()));
        assert!(a.status.starts_with("Couldn't play “bad”"), "{}", a.status);
        assert!(a.playing);
    }

    #[test]
    fn a_run_of_bad_files_stops_instead_of_racing_through_the_library() {
        let paths: Vec<String> = (0..10).map(|i| format!("/m/bad{i}.mp3")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let (mut a, out) = playing_app(&refs);
        out.borrow_mut().broken = paths.iter().map(PathBuf::from).collect();
        a.play_row(0);
        assert!(!a.playing && out.borrow().loaded.is_none());
        assert_eq!(a.queue.current(), Some(&PathBuf::from("/m/bad2.mp3")));
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
        let mut a = App::new(library, Some(file.clone()), true, String::new(), silent().0);
        a.create_playlist("New");
        assert_eq!(
            std::fs::read_to_string(dir.join("library.v1.json.bak")).unwrap(),
            original
        );
        assert_eq!(Library::load(&file).unwrap().playlists[0].name, "New");

        // An unreadable one is never written.
        std::fs::write(&file, b"{ broken").unwrap();
        let mut b = App::new(
            Library::default(),
            Some(file.clone()),
            false,
            String::new(),
            silent().0,
        );
        b.create_playlist("Nope");
        assert_eq!(std::fs::read(&file).unwrap(), b"{ broken");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = Playlist::default();
    }
}
