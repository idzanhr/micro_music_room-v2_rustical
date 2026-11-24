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
    media::{MediaCommand, MediaSession, NowPlaying, NullMedia, Playback},
    model::{Browser, Entry, View, format_duration},
    queue::{Queue, Repeat, Rng},
    scan::{ScanResult, scan_folder},
    settings::Settings,
    tags::{CacheEntry, Cover, Done, Job, TagCache, TagWorker, default_cache_path, key_of},
    track::{Track, is_supported},
};
use slint::{
    ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

const DEFAULT_VOLUME: f32 = 0.8;
/// Rows read beyond the visible ones, so a slow scroll finds them already filled in.
const TAG_MARGIN: usize = 20;
/// How often a changed tag cache is written out.
const CACHE_SAVE_EVERY: Duration = Duration::from_secs(10);
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
    tags: TagCache,
    cache_path: Option<PathBuf>,
    last_cache_save: Instant,
    worker: TagWorker,
    /// Files already requested this session, so scrolling back and forth asks only once.
    asked: RefCell<HashSet<String>>,
    /// First visible row and how many fit, as last reported by the list.
    view_range: (usize, usize),
    cover: Option<Cover>,
    cover_dirty: Cell<bool>,
    /// The system media layer: flyout, media keys, notifications.
    media: Box<dyn MediaSession>,
    /// What the media layer was last told, so it is only told again when something changed.
    media_state: Option<Playback>,
    media_dirty: bool,
    /// The song a notification was last shown for, so repeating one song does not re-notify.
    last_notified: Option<PathBuf>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// Whether the system media controls connected, shown in the About box.
    media_status: String,
}

/// What a worker poll changed.
#[derive(Debug, Default, Clone, Copy)]
pub struct Polled {
    pub rows: bool,
    pub cover: bool,
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
            tags: TagCache::default(),
            cache_path: None,
            last_cache_save: Instant::now(),
            worker: TagWorker::spawn(),
            asked: RefCell::new(HashSet::new()),
            view_range: (0, 30),
            cover: None,
            cover_dirty: Cell::new(false),
            media: Box::new(NullMedia),
            media_state: None,
            media_dirty: false,
            last_notified: None,
            settings: Settings::default(),
            settings_path: None,
            media_status: String::new(),
        }
    }

    /// Load the v1 library read-only. `MMR_LIBRARY` points the app at another file, so it can be
    /// tried without touching the real one.
    pub fn load() -> Self {
        let mut app = Self::load_library();
        // The tag cache sits beside the real library, or beside the one `MMR_LIBRARY` points at.
        app.cache_path = app
            .library_path
            .as_ref()
            .map(|p| p.with_file_name("tagcache.json"))
            .or_else(default_cache_path);
        if let Some(path) = &app.cache_path {
            app.tags = TagCache::load(path);
        }
        app.settings_path = app
            .cache_path
            .as_ref()
            .map(|p| p.with_file_name("settings.json"));
        if let Some(path) = &app.settings_path {
            app.settings = Settings::load(path);
        }
        app
    }

    fn load_library() -> Self {
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
                    self.cover = None;
                    self.cover_dirty.set(true);
                    self.worker.request(Job::Cover(path.clone()));
                    self.loaded = Some(path);
                    self.playing = true;
                    self.media_dirty = true;
                    self.publish_now_playing();
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
        self.media_dirty = true;
        self.media.set_now_playing(None);
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
        self.seek_to(length.mul_f32(fraction.clamp(0.0, 1.0)));
    }

    fn seek_to(&mut self, target: Duration) {
        if self.loaded.is_none() {
            return;
        }
        if let Err(e) = self.output.seek(target) {
            self.status = format!("Couldn't seek in this file: {e}");
        }
        self.media_dirty = true;
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

    /// One table row: real tags where they have been read, else what the file name says.
    fn row_for(&self, index: usize, entry: &Entry, current: Option<&PathBuf>) -> TrackRow {
        let path = &entry.track.path;
        let tags = self.tags.tags(path);
        let pick = |tag: Option<&str>, fallback: &str| -> String {
            tag.filter(|t| !t.is_empty()).unwrap_or(fallback).to_owned()
        };
        let artist = pick(tags.map(|t| t.artist.as_str()), &entry.track.artist);
        TrackRow {
            title: pick(tags.map(|t| t.title.as_str()), &entry.track.title).into(),
            artist: if artist.is_empty() {
                "—".into()
            } else {
                artist.into()
            },
            album: pick(tags.map(|t| t.album.as_str()), &entry.track.folder).into(),
            duration: format_duration(tags.and_then(|t| t.seconds)).into(),
            playing: current == Some(path),
            selected: self.selection.contains(&index),
        }
    }

    /// The window reports which rows are on screen. Remember it and read their tags.
    fn rows_visible(&mut self, first: i32, count: i32) {
        self.view_range = (
            usize::try_from(first).unwrap_or(0),
            usize::try_from(count).unwrap_or(0).max(1),
        );
        self.want_tags();
    }

    /// Ask the worker for the tags of every row on screen (plus a margin) that has not been asked
    /// for yet this session. Cached rows are asked about too, once, to notice a retagged file.
    fn want_tags(&self) {
        let visible = self.browser.visible();
        let (first, count) = self.view_range;
        let lo = first.saturating_sub(TAG_MARGIN).min(visible.len());
        let hi = (first + count + TAG_MARGIN).min(visible.len());
        let mut asked = self.asked.borrow_mut();
        let wanted: Vec<_> = visible[lo..hi]
            .iter()
            .filter(|e| asked.insert(key_of(&e.track.path)))
            .map(|e| {
                let known = self.tags.get(&e.track.path).map(|c| c.stamp);
                (e.track.path.clone(), known)
            })
            .collect();
        if !wanted.is_empty() {
            self.worker.request(Job::Tags(wanted));
        }
    }

    /// Collect what the worker has finished. Cheap; the timer calls it constantly.
    pub fn poll_worker(&mut self) -> Polled {
        let mut polled = Polled::default();
        let mut dropped = false;
        while let Some(done) = self.worker.try_recv() {
            match done {
                Done::Tags { path, stamp, tags } => {
                    self.tags.put(&path, CacheEntry { stamp, tags });
                    polled.rows = true;
                }
                Done::Dropped(paths) => {
                    let mut asked = self.asked.borrow_mut();
                    for path in paths {
                        asked.remove(&key_of(&path));
                    }
                    dropped = true;
                }
                Done::Cover { path, cover } => {
                    // The song may have changed while this was being read.
                    if self.loaded.as_ref() == Some(&path) {
                        self.cover = cover;
                        self.cover_dirty.set(true);
                        polled.cover = true;
                        // The song's tags and cover are all here now: update the flyout, and
                        // announce the song if it is new.
                        self.publish_now_playing();
                        self.announce(&path);
                    }
                }
            }
        }
        if dropped {
            self.want_tags();
        }
        polled
    }

    /// Redraw only the rows near the viewport, after tags arrived. Replacing the whole list for
    /// every batch would be wasteful and would jump the scroll position.
    pub fn refresh_tag_rows(&self, ui: &MainWindow) {
        let rows = ui.global::<AppState>().get_rows();
        let visible = self.browser.visible();
        let (first, count) = self.view_range;
        let lo = first.saturating_sub(TAG_MARGIN).min(visible.len());
        let hi = (first + count + TAG_MARGIN).min(visible.len());
        let current = self.queue.current();
        for (i, entry) in visible.iter().enumerate().take(hi).skip(lo) {
            if i >= rows.row_count() {
                break;
            }
            let row = self.row_for(i, entry, current);
            if rows.row_data(i).as_ref() != Some(&row) {
                rows.set_row_data(i, row);
            }
        }
        self.refresh_player(ui);
    }

    /// What the system flyout and notifications should say about the current song.
    fn now_playing(&self) -> Option<NowPlaying> {
        let path = self.queue.current()?;
        let fallback = Track::from_path(path.clone());
        let tags = self.tags.tags(path);
        let pick = |tag: Option<&str>, fallback: &str| {
            tag.filter(|t| !t.is_empty()).unwrap_or(fallback).to_owned()
        };
        Some(NowPlaying {
            title: pick(tags.map(|t| t.title.as_str()), &fallback.title),
            artist: pick(tags.map(|t| t.artist.as_str()), &fallback.artist),
            album: tags.map_or(String::new(), |t| t.album.clone()),
            cover_png: self.cover.as_ref().and_then(Cover::to_png),
            length: self.length,
        })
    }

    fn publish_now_playing(&mut self) {
        let now = self.now_playing();
        self.media.set_now_playing(now.as_ref());
    }

    /// Show the track-change notification, once per song and only if the user wants them.
    fn announce(&mut self, path: &PathBuf) {
        if !self.settings.notifications || self.last_notified.as_ref() == Some(path) {
            return;
        }
        self.last_notified = Some(path.clone());
        if let Some(now) = self.now_playing() {
            self.media.notify_track_change(&now);
        }
    }

    /// Carry out what media keys, headset buttons and the flyout asked for. Returns true if
    /// anything was done, so the window should be redrawn.
    pub fn poll_media(&mut self) -> bool {
        let commands = self.media.take_commands();
        let acted = !commands.is_empty();
        for command in commands {
            match command {
                MediaCommand::Play if !self.playing => self.toggle_play(),
                MediaCommand::Pause if self.playing => self.toggle_play(),
                MediaCommand::Play | MediaCommand::Pause => {}
                MediaCommand::Next => self.next(),
                MediaCommand::Previous => self.previous(),
                MediaCommand::Stop => self.stop(),
                MediaCommand::Seek(to) => self.seek_to(to),
            }
        }
        acted
    }

    /// Keep the system's play/pause state and timeline in step with the player. Cheap: it only
    /// talks to the system when the state changed or a jump (start, seek) happened.
    pub fn sync_media(&mut self) {
        let state = match (self.loaded.is_some(), self.playing) {
            (false, _) => Playback::Stopped,
            (true, true) => Playback::Playing,
            (true, false) => Playback::Paused,
        };
        if self.media_dirty || self.media_state != Some(state) {
            self.media
                .set_playback(state, self.output.position(), self.length);
            self.media_state = Some(state);
            self.media_dirty = false;
        }
    }

    /// Connect the system media layer. Done once, after the window exists.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn attach_media(&mut self, media: Box<dyn MediaSession>) {
        self.media_status = media.status();
        self.media = media;
        self.media_dirty = true;
        self.publish_now_playing();
    }

    /// The system media controls could not be connected. Shown in the About box.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn media_unavailable(&mut self, reason: &str) {
        self.media_status = format!("Windows media controls are not connected: {reason}");
    }

    fn test_notification(&mut self) {
        let result = self.media.send_test_notification();
        self.media_status = format!("{}\n{result}", self.media.status());
    }

    fn set_notifications(&mut self, on: bool) {
        self.settings.notifications = on;
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.status = format!("Couldn't save your settings: {e}");
        }
    }

    /// Write the tag cache if it changed and it has been a while (or `force`).
    pub fn save_tags(&mut self, force: bool) {
        if !self.tags.is_dirty() || !(force || self.last_cache_save.elapsed() >= CACHE_SAVE_EVERY) {
            return;
        }
        if let Some(path) = &self.cache_path {
            // A cache that cannot be written costs nothing but re-reading tags next time.
            let _ = self.tags.save(path);
        }
        self.last_cache_save = Instant::now();
    }

    pub fn refresh(&self, ui: &MainWindow) {
        let state = ui.global::<AppState>();
        let current = self.queue.current();

        let visible = self.browser.visible();
        let shown = visible.len();
        let rows: Vec<TrackRow> = visible
            .iter()
            .enumerate()
            .map(|(i, e)| self.row_for(i, e, current))
            .collect();
        state.set_rows(ModelRc::new(VecModel::from(rows)));
        drop(visible);
        self.want_tags();

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
        let tags = current.and_then(|p| self.tags.tags(p));
        let pick = |tag: Option<&str>, fallback: &str| -> String {
            tag.filter(|t| !t.is_empty()).unwrap_or(fallback).to_owned()
        };
        let title = pick(
            tags.map(|t| t.title.as_str()),
            now.as_ref().map_or("", |t| t.title.as_str()),
        );
        let artist = pick(
            tags.map(|t| t.artist.as_str()),
            now.as_ref().map_or("", |t| t.artist.as_str()),
        );
        state.set_now_title(title.into());
        state.set_now_artist(artist.into());
        if self.cover_dirty.replace(false) {
            match &self.cover {
                Some(c) => {
                    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(c.width, c.height);
                    pixels.make_mut_bytes().copy_from_slice(&c.rgba);
                    state.set_cover(slint::Image::from_rgba8(pixels));
                    state.set_has_cover(true);
                }
                None => state.set_has_cover(false),
            }
        }
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
        state.set_notifications(self.settings.notifications);
        state.set_media_status(self.media_status.as_str().into());
    }
}

/// Write out anything unsaved. Call when the window closes.
pub fn flush(app: &Rc<RefCell<App>>) {
    app.borrow_mut().save_tags(true);
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
    {
        // Reporting the visible rows must not redraw the list, so it skips the `on!` refresh.
        let app = app.clone();
        state.on_rows_visible(move |first, count| app.borrow_mut().rows_visible(first, count));
    }
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
    on!(on_test_notification, |a| a.test_notification());
    on!(on_notifications_changed, |a, on| a.set_notifications(on));

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
        changed |= app.borrow_mut().poll_media();
        app.borrow_mut().sync_media();
        let polled = app.borrow_mut().poll_worker();
        app.borrow_mut().save_tags(false);
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if changed {
            app.borrow().refresh(&ui);
        } else if polled.rows {
            app.borrow().refresh_tag_rows(&ui);
        } else if polled.cover || app.borrow().playing {
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
        media::{MediaLog, RecordingMedia},
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

    /// A 3-second silent WAV with an ID3v2 tag, optionally with a 64x64 PNG cover.
    fn tagged_wav(path: &std::path::Path, title: &str, artist: &str, album: &str, cover: bool) {
        use lofty::{
            config::WriteOptions,
            picture::{MimeType, Picture, PictureType},
            tag::{Accessor, Tag, TagExt, TagType},
        };
        let rate = 8000u32;
        let data_len = rate * 2 * 3;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        b.extend(std::iter::repeat_n(0u8, data_len as usize));
        std::fs::write(path, b).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title(title.into());
        tag.set_artist(artist.into());
        tag.set_album(album.into());
        if cover {
            let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 116, 60, 255]));
            let mut png = std::io::Cursor::new(Vec::new());
            img.write_to(&mut png, image::ImageFormat::Png).unwrap();
            tag.push_picture(
                Picture::unchecked(png.into_inner())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        }
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    /// Run the worker poll until `done` says so (or give up).
    fn poll_until(a: &mut App, mut done: impl FnMut(Polled) -> bool) {
        for _ in 0..500 {
            if done(a.poll_worker()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the worker never delivered");
    }

    #[test]
    fn rows_show_real_tags_once_the_worker_has_read_them() {
        use slint::Model;
        let ui = window();
        let dir = std::env::temp_dir().join(format!("mmr-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("01 - raw name.wav");
        tagged_wav(&song, "Slušaj mater", "Kerber", "Čežnja", false);

        let mut a = app(&[song.to_str().unwrap()]);
        a.refresh(&ui);
        let state = ui.global::<AppState>();
        assert_eq!(
            state.get_rows().row_data(0).unwrap().title,
            "raw name",
            "file name first"
        );
        assert_eq!(state.get_rows().row_data(0).unwrap().duration, "—");

        poll_until(&mut a, |p| p.rows);
        a.refresh_tag_rows(&ui);
        let row = state.get_rows().row_data(0).unwrap();
        assert_eq!(row.title, "Slušaj mater");
        assert_eq!(row.artist, "Kerber");
        assert_eq!(row.album, "Čežnja");
        assert_eq!(row.duration, "0:03");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tag_cache_is_saved_and_shows_instantly_next_time() {
        use slint::Model;
        let ui = window();
        let dir = std::env::temp_dir().join(format!("mmr-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("a.wav");
        tagged_wav(
            &song,
            "Cached title",
            "Cached artist",
            "Cached album",
            false,
        );
        let cache = dir.join("tagcache.json");

        let mut first = app(&[song.to_str().unwrap()]);
        first.cache_path = Some(cache.clone());
        first.refresh(&ui);
        poll_until(&mut first, |p| p.rows);
        first.save_tags(true);
        assert!(cache.exists());

        // A fresh app starts from the file: the row is right on the very first draw.
        let mut second = app(&[song.to_str().unwrap()]);
        second.tags = TagCache::load(&cache);
        second.refresh(&ui);
        let row = ui.global::<AppState>().get_rows().row_data(0).unwrap();
        assert_eq!(
            (row.title.as_str(), row.duration.as_str()),
            ("Cached title", "0:03")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_rows_near_the_viewport_are_asked_for() {
        let paths: Vec<String> = (0..500).map(|i| format!("/m/{i}.mp3")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let (a, _) = playing_app(&refs);
        a.want_tags();
        let asked = a.asked.borrow().len();
        assert!(
            asked <= 30 + TAG_MARGIN + 1,
            "first screen only, asked {asked}"
        );
        assert!(asked > 0);
    }

    #[test]
    fn the_cover_of_the_playing_song_reaches_the_player_bar() {
        let ui = window();
        let dir = std::env::temp_dir().join(format!("mmr-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("a.wav");
        tagged_wav(&song, "t", "a", "b", true);

        let (mut a, _) = playing_app(&[song.to_str().unwrap()]);
        a.play_row(0);
        a.refresh(&ui);
        assert!(
            !ui.global::<AppState>().get_has_cover(),
            "none until it has been read"
        );
        poll_until(&mut a, |p| p.cover);
        a.refresh_player(&ui);
        assert!(ui.global::<AppState>().get_has_cover());

        // Moving to a song without art clears it again.
        a.cover = None;
        a.cover_dirty.set(true);
        a.refresh_player(&ui);
        assert!(!ui.global::<AppState>().get_has_cover());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn media_app(paths: &[&str]) -> (App, Rc<RefCell<SilentState>>, Rc<RefCell<MediaLog>>) {
        let (mut a, out) = playing_app(paths);
        let (media, log) = RecordingMedia::new();
        a.attach_media(Box::new(media));
        (a, out, log)
    }

    #[test]
    fn the_flyout_is_told_about_the_song_and_its_state() {
        let (mut a, _out, log) = media_app(&["/m/Kerber - Slušaj mater.mp3", "/m/two.mp3"]);
        a.play_row(0);
        a.sync_media();
        let now = log.borrow().now_playing.clone().expect("now playing");
        assert_eq!(
            (now.title.as_str(), now.artist.as_str()),
            ("Slušaj mater", "Kerber")
        );
        assert_eq!(log.borrow().playback.map(|p| p.0), Some(Playback::Playing));

        a.toggle_play();
        a.sync_media();
        assert_eq!(log.borrow().playback.map(|p| p.0), Some(Playback::Paused));

        // Nothing changed: the system is not bothered again.
        let updates = log.borrow().playback_updates;
        a.sync_media();
        a.sync_media();
        assert_eq!(log.borrow().playback_updates, updates);

        // A seek is a jump the timeline must hear about.
        a.toggle_play();
        a.seek(0.5);
        a.sync_media();
        let (_, position, length) = log.borrow().playback.unwrap();
        assert_eq!((position, length), (secs(100), Some(secs(200))));
    }

    #[test]
    fn stopping_clears_the_flyout() {
        let (mut a, _out, log) = media_app(&["/m/one.mp3"]);
        a.play_row(0);
        a.stop();
        a.sync_media();
        assert!(log.borrow().now_playing.is_none());
        assert_eq!(log.borrow().playback.map(|p| p.0), Some(Playback::Stopped));
    }

    #[test]
    fn media_keys_and_headset_buttons_drive_the_player() {
        let (mut a, out, log) = media_app(&["/m/one.mp3", "/m/two.mp3", "/m/three.mp3"]);
        a.play_row(0);

        log.borrow_mut().incoming = vec![MediaCommand::Pause];
        assert!(a.poll_media());
        assert!(!a.playing && out.borrow().paused);

        // Pause again changes nothing, and Play resumes without reloading the file.
        log.borrow_mut().incoming = vec![MediaCommand::Pause, MediaCommand::Play];
        a.poll_media();
        assert!(a.playing && !out.borrow().paused);
        assert_eq!(out.borrow().loads, 1);

        log.borrow_mut().incoming = vec![MediaCommand::Next];
        a.poll_media();
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/two.mp3".as_ref()));

        log.borrow_mut().incoming = vec![MediaCommand::Seek(secs(42))];
        a.poll_media();
        assert_eq!(out.borrow().position, secs(42));

        log.borrow_mut().incoming = vec![MediaCommand::Previous];
        a.poll_media(); // 42 s in: restarts instead of going back
        assert_eq!(out.borrow().position, Duration::ZERO);
        log.borrow_mut().incoming = vec![MediaCommand::Previous];
        a.poll_media();
        assert_eq!(out.borrow().loaded.as_deref(), Some("/m/one.mp3".as_ref()));

        log.borrow_mut().incoming = vec![MediaCommand::Stop];
        a.poll_media();
        assert!(!a.playing && out.borrow().loaded.is_none());
        assert!(!a.poll_media(), "nothing queued, nothing to do");
    }

    #[test]
    fn a_notification_comes_once_per_song_with_its_tags_and_cover() {
        let dir = std::env::temp_dir().join(format!("mmr-toast-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("a.wav");
        tagged_wav(&song, "Faca (intro)", "Edo Maajka", "Slušaj mater", true);
        let (mut a, _out, log) = media_app(&[song.to_str().unwrap()]);

        a.play_row(0);
        assert!(
            log.borrow().notifications.is_empty(),
            "waits for tags and cover"
        );
        poll_until(&mut a, |p| p.cover);
        let toast = log.borrow().notifications[0].clone();
        assert_eq!(toast.title, "Faca (intro)");
        assert_eq!(toast.artist, "Edo Maajka");
        assert_eq!(toast.album, "Slušaj mater");
        assert!(toast.cover_png.is_some());
        // The flyout got the same, cover included.
        assert!(
            log.borrow()
                .now_playing
                .as_ref()
                .unwrap()
                .cover_png
                .is_some()
        );

        // The same song again (repeat one) is not announced twice.
        a.play_row(0);
        poll_until(&mut a, |p| p.cover);
        assert_eq!(log.borrow().notifications.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notifications_can_be_switched_off() {
        let dir = std::env::temp_dir().join(format!("mmr-quiet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("a.wav");
        tagged_wav(&song, "t", "a", "b", false);
        let (mut a, _out, log) = media_app(&[song.to_str().unwrap()]);
        a.settings_path = Some(dir.join("settings.json"));
        a.set_notifications(false);
        assert!(!Settings::load(&dir.join("settings.json")).notifications);

        a.play_row(0);
        poll_until(&mut a, |p| p.cover);
        assert!(log.borrow().notifications.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
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
