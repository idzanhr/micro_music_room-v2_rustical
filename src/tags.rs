// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Song tags (title, artist, album, length) and cover art, read lazily and cached.
//!
//! A 100k-song library cannot be read up front, so the window asks only for the rows it is
//! showing. A [`TagWorker`] reads them on a background thread and hands the results back; the
//! answers are kept in a [`TagCache`] saved next to the library as `tagcache.json`. That file is
//! only a cache: it is never the source of truth, can be deleted at any time, and is separate from
//! `library.json`, which tag reading never touches.
//!
//! The cache is trusted on sight and re-checked in the background (file size + modified time), so a
//! song you retagged is picked up the next time it scrolls into view.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::UNIX_EPOCH,
};

use lofty::{
    config::ParseOptions,
    file::{AudioFile, TaggedFileExt},
    picture::PictureType,
    probe::Probe,
    tag::Accessor,
};

/// Cover art is shown small, so it is shrunk to this many pixels on its longest side.
pub const COVER_SIZE: u32 = 160;
/// Anything bigger than this is not a cover, it is a mistake.
const MAX_COVER_BYTES: usize = 24 * 1024 * 1024;

/// What identifies one version of a file: if either changes, the tags are read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
}

pub fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(Stamp {
        size: meta.len(),
        modified,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Whole seconds, when the format says how long it is.
    pub seconds: Option<u64>,
}

fn clean(text: Option<impl AsRef<str>>) -> String {
    text.map(|t| {
        t.as_ref()
            .trim_matches(|c: char| c.is_whitespace() || c == '\0')
            .to_owned()
    })
    .unwrap_or_default()
}

/// Read the text tags and length of a file. `None` if the file cannot be read as audio at all.
/// Cover art is skipped here on purpose: it is the expensive part, and the list does not need it.
pub fn read_tags(path: &Path) -> Option<Tags> {
    let options = ParseOptions::new().read_cover_art(false);
    let file = Probe::open(path).ok()?.options(options).read().ok()?;
    let seconds = Some(file.properties().duration().as_secs()).filter(|&s| s > 0);
    let tag = file.primary_tag().or_else(|| file.first_tag());
    Some(Tags {
        title: clean(tag.and_then(|t| t.title())),
        artist: clean(tag.and_then(|t| t.artist())),
        album: clean(tag.and_then(|t| t.album())),
        seconds,
    })
}

/// A small RGBA picture ready for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn decode_cover(bytes: &[u8]) -> Option<Cover> {
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
        return None;
    }
    let picture = image::load_from_memory(bytes).ok()?;
    // Only ever shrink: `thumbnail` would blow a small picture up.
    let small = if picture.width() > COVER_SIZE || picture.height() > COVER_SIZE {
        picture.thumbnail(COVER_SIZE, COVER_SIZE)
    } else {
        picture
    }
    .to_rgba8();
    Some(Cover {
        width: small.width(),
        height: small.height(),
        rgba: small.into_raw(),
    })
}

/// Names a folder's cover picture commonly has, checked when the song has none embedded.
const SIDECAR_NAMES: [&str; 5] = ["cover", "folder", "front", "albumart", "album"];
const SIDECAR_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

fn sidecar_cover(song: &Path) -> Option<Vec<u8>> {
    let dir = song.parent()?;
    let mut found: Option<(usize, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let stem = path.file_stem()?.to_string_lossy().to_lowercase();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        let rank = SIDECAR_NAMES.iter().position(|n| *n == stem);
        if let (Some(rank), Some(ext)) = (rank, ext)
            && SIDECAR_EXTENSIONS.contains(&ext.as_str())
            && found.as_ref().is_none_or(|(best, _)| rank < *best)
        {
            found = Some((rank, path));
        }
    }
    let (_, path) = found?;
    (fs::metadata(&path).ok()?.len() <= MAX_COVER_BYTES as u64)
        .then(|| fs::read(path).ok())
        .flatten()
}

/// The song's cover: the embedded front cover (or any embedded picture), else `cover.jpg` and
/// friends from the same folder.
pub fn read_cover(path: &Path) -> Option<Cover> {
    let embedded = Probe::open(path)
        .ok()
        .and_then(|p| p.options(ParseOptions::new()).read().ok())
        .and_then(|file| {
            let tag = file.primary_tag().or_else(|| file.first_tag())?;
            let pictures = tag.pictures();
            pictures
                .iter()
                .find(|p| p.pic_type() == PictureType::CoverFront)
                .or_else(|| pictures.first())
                .map(|p| p.data().to_vec())
        })
        .and_then(|bytes| decode_cover(&bytes));
    embedded.or_else(|| sidecar_cover(path).and_then(|bytes| decode_cover(&bytes)))
}

// ---- the cache -----------------------------------------------------------------------------

/// Case-insensitive identity for a path, like the library uses.
pub fn key_of(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub stamp: Stamp,
    /// `None` means the file was looked at and could not be read, so it is not retried on every
    /// scroll (it is re-checked when its size or date changes).
    pub tags: Option<Tags>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Default)]
pub struct TagCache {
    entries: HashMap<String, CacheEntry>,
    dirty: bool,
}

impl TagCache {
    /// Load the cache. A missing, damaged or newer-format file just gives an empty cache: it is
    /// rebuilt as songs are viewed, and nothing is ever lost that cannot be read again.
    pub fn load(path: &Path) -> Self {
        let entries = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
            .filter(|f| f.version == CACHE_VERSION)
            .map(|f| f.entries)
            .unwrap_or_default();
        Self {
            entries,
            dirty: false,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, path: &Path) -> Option<&CacheEntry> {
        self.entries.get(&key_of(path))
    }

    /// The tags to show for a file, if it has readable ones cached.
    pub fn tags(&self, path: &Path) -> Option<&Tags> {
        self.get(path).and_then(|e| e.tags.as_ref())
    }

    pub fn put(&mut self, path: &Path, entry: CacheEntry) {
        if self.entries.get(&key_of(path)) != Some(&entry) {
            self.entries.insert(key_of(path), entry);
            self.dirty = true;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write the cache through a temporary file, so a crash mid-write cannot leave half a file.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = CacheFile {
            version: CACHE_VERSION,
            entries: std::mem::take(&mut self.entries),
        };
        let json = serde_json::to_vec(&file);
        self.entries = file.entries;
        let json = json.map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(&temp, path)?;
        self.dirty = false;
        Ok(())
    }
}

/// Where the cache lives: beside `library.json`.
pub fn default_cache_path() -> Option<PathBuf> {
    crate::library::default_library_path().map(|p| p.with_file_name("tagcache.json"))
}

// ---- the background worker -----------------------------------------------------------------

/// A request for the worker. `known` is the stamp the cache already holds for the file, so the
/// worker can skip reading it when nothing changed.
#[derive(Debug)]
pub enum Job {
    Tags(Vec<(PathBuf, Option<Stamp>)>),
    Cover(PathBuf),
}

#[derive(Debug)]
pub enum Done {
    Tags {
        path: PathBuf,
        stamp: Stamp,
        tags: Option<Tags>,
    },
    /// Files the worker dropped because a newer request replaced theirs (the user scrolled on).
    /// The caller may ask for them again later.
    Dropped(Vec<PathBuf>),
    Cover {
        path: PathBuf,
        cover: Option<Cover>,
    },
}

pub struct TagWorker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

impl TagWorker {
    pub fn spawn() -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("tag-reader".into())
            .spawn(move || work(job_rx, done_tx))
            .expect("spawn the tag reader thread");
        Self { jobs, done }
    }

    pub fn request(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }
}

fn work(jobs: Receiver<Job>, done: Sender<Done>) {
    let mut pending: Vec<Job> = Vec::new();
    loop {
        if pending.is_empty() {
            match jobs.recv() {
                Ok(job) => pending.push(job),
                Err(_) => return,
            }
        }
        pending.extend(jobs.try_iter());

        // Covers first: the song that just started needs its art now.
        if let Some(i) = pending.iter().position(|j| matches!(j, Job::Cover(_))) {
            if let Job::Cover(path) = pending.remove(i) {
                let cover = read_cover(&path);
                if done.send(Done::Cover { path, cover }).is_err() {
                    return;
                }
            }
            continue;
        }

        // Of several tag requests only the newest matters; the older ones are what the user has
        // already scrolled past.
        let mut batches = std::mem::take(&mut pending);
        let Some(Job::Tags(newest)) = batches.pop() else {
            continue;
        };
        let dropped: Vec<PathBuf> = batches
            .into_iter()
            .flat_map(|job| match job {
                Job::Tags(files) => files,
                Job::Cover(_) => Vec::new(),
            })
            .map(|(path, _)| path)
            .collect();
        if !dropped.is_empty() && done.send(Done::Dropped(dropped)).is_err() {
            return;
        }
        if !read_batch(newest, &jobs, &done, &mut pending) {
            return;
        }
    }
}

/// Read one batch. Between files it looks for a newer request; if one is waiting (it is left in
/// `pending`), the rest of this batch is handed back as dropped. Returns false when the receiving
/// side is gone.
fn read_batch(
    files: Vec<(PathBuf, Option<Stamp>)>,
    jobs: &Receiver<Job>,
    done: &Sender<Done>,
    pending: &mut Vec<Job>,
) -> bool {
    let mut files = files.into_iter();
    while let Some((path, known)) = files.next() {
        let stamp = stamp_of(&path);
        let result = match stamp {
            // Unchanged since the cache was written: nothing to do.
            Some(s) if Some(s) == known => None,
            Some(s) => Some((s, read_tags(&path))),
            None => Some((Stamp::default(), None)),
        };
        if let Some((stamp, tags)) = result
            && done.send(Done::Tags { path, stamp, tags }).is_err()
        {
            return false;
        }
        pending.extend(jobs.try_iter());
        if !pending.is_empty() {
            let rest: Vec<PathBuf> = files.map(|(path, _)| path).collect();
            return rest.is_empty() || done.send(Done::Dropped(rest)).is_ok();
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use lofty::{
        config::WriteOptions,
        picture::{MimeType, Picture},
        tag::{Tag, TagExt, TagType},
    };
    use std::time::Duration;

    /// A silent mono 8 kHz 16-bit WAV of the given length, written by hand.
    fn write_wav(path: &Path, seconds: u32) {
        let rate = 8000u32;
        let data_len = rate * 2 * seconds;
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
        fs::write(path, b).unwrap();
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 116, 60, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A WAV with an ID3v2 tag, optionally with an embedded cover.
    fn tagged_wav(path: &Path, title: &str, artist: &str, album: &str, cover: Option<Vec<u8>>) {
        write_wav(path, 3);
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title(title.into());
        tag.set_artist(artist.into());
        tag.set_album(album.into());
        if let Some(bytes) = cover {
            tag.push_picture(
                Picture::unchecked(bytes)
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        }
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn text_tags_and_length_are_read_including_croatian_letters() {
        let dir = TempDir::new();
        let file = dir.path().join("a.wav");
        tagged_wav(&file, "Slušaj mater", "Đorđe Balašević", "Čežnja", None);
        let tags = read_tags(&file).expect("readable");
        assert_eq!(tags.title, "Slušaj mater");
        assert_eq!(tags.artist, "Đorđe Balašević");
        assert_eq!(tags.album, "Čežnja");
        assert_eq!(tags.seconds, Some(3));
    }

    #[test]
    fn an_untagged_file_still_reports_its_length() {
        let dir = TempDir::new();
        let file = dir.path().join("plain.wav");
        write_wav(&file, 2);
        let tags = read_tags(&file).expect("readable");
        assert_eq!((tags.title.as_str(), tags.artist.as_str()), ("", ""));
        assert_eq!(tags.seconds, Some(2));
    }

    #[test]
    fn something_that_is_not_audio_has_no_tags() {
        let dir = TempDir::new();
        let file = dir.path().join("notes.mp3");
        fs::write(&file, b"this is not audio").unwrap();
        assert_eq!(read_tags(&file), None);
        assert_eq!(read_tags(&dir.path().join("missing.mp3")), None);
    }

    #[test]
    fn an_embedded_cover_is_decoded_and_shrunk() {
        let dir = TempDir::new();
        let file = dir.path().join("a.wav");
        tagged_wav(&file, "t", "a", "b", Some(png(640, 320)));
        let cover = read_cover(&file).expect("cover");
        assert_eq!((cover.width, cover.height), (COVER_SIZE, COVER_SIZE / 2));
        assert_eq!(cover.rgba.len(), (cover.width * cover.height * 4) as usize);
        assert_eq!(&cover.rgba[..4], &[200, 116, 60, 255]);
    }

    #[test]
    fn without_an_embedded_cover_the_folder_picture_is_used() {
        let dir = TempDir::new();
        let file = dir.path().join("song.wav");
        write_wav(&file, 1);
        assert_eq!(read_cover(&file), None, "nothing to show yet");
        fs::write(dir.path().join("Folder.PNG"), png(40, 40)).unwrap();
        fs::write(dir.path().join("album.png"), png(10, 10)).unwrap();
        let cover = read_cover(&file).expect("folder cover");
        assert_eq!(cover.width, 40, "folder.* beats album.*");
    }

    #[test]
    fn a_broken_picture_is_ignored_not_fatal() {
        let dir = TempDir::new();
        let file = dir.path().join("song.wav");
        write_wav(&file, 1);
        fs::write(dir.path().join("cover.jpg"), b"not a jpeg").unwrap();
        assert_eq!(read_cover(&file), None);
    }

    #[test]
    fn the_cache_round_trips_and_ignores_damage() {
        let dir = TempDir::new();
        let path = dir.path().join("tagcache.json");
        let song = Path::new("C:\\Music\\A.mp3");
        let entry = CacheEntry {
            stamp: Stamp {
                size: 5,
                modified: 9,
            },
            tags: Some(Tags {
                title: "T".into(),
                artist: "A".into(),
                album: "B".into(),
                seconds: Some(61),
            }),
        };
        let mut cache = TagCache::default();
        cache.put(song, entry.clone());
        assert!(cache.is_dirty());
        cache.save(&path).unwrap();
        assert!(!cache.is_dirty());
        assert!(!dir.path().join("tagcache.json.tmp").exists());

        let loaded = TagCache::load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get(Path::new("c:\\music\\a.mp3")),
            Some(&entry),
            "path case does not matter"
        );
        assert_eq!(loaded.tags(song).unwrap().seconds, Some(61));

        // Putting the same thing again is not a change worth saving.
        let mut again = TagCache::load(&path);
        again.put(song, entry);
        assert!(!again.is_dirty());

        fs::write(&path, b"{ not json").unwrap();
        assert!(TagCache::load(&path).is_empty());
        fs::write(&path, br#"{"version":99,"entries":{}}"#).unwrap();
        assert!(
            TagCache::load(&path).is_empty(),
            "unknown version is ignored"
        );
        assert!(TagCache::load(&dir.path().join("absent.json")).is_empty());
    }

    fn wait_for(worker: &TagWorker, mut want: impl FnMut(&Done) -> bool) -> Done {
        for _ in 0..500 {
            if let Some(done) = worker.try_recv()
                && want(&done)
            {
                return done;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the worker never answered");
    }

    #[test]
    fn the_worker_reads_tags_and_skips_unchanged_files() {
        let dir = TempDir::new();
        let a = dir.path().join("a.wav");
        let b = dir.path().join("b.wav");
        tagged_wav(&a, "Alpha", "X", "Y", None);
        tagged_wav(&b, "Beta", "X", "Y", None);
        let known_b = stamp_of(&b);

        let worker = TagWorker::spawn();
        worker.request(Job::Tags(vec![(a.clone(), None), (b.clone(), known_b)]));
        let Done::Tags { path, tags, stamp } = wait_for(&worker, |_| true) else {
            panic!("expected tags");
        };
        assert_eq!(path, a);
        assert_eq!(tags.unwrap().title, "Alpha");
        assert_eq!(Some(stamp), stamp_of(&a));

        // b was already cached with its current stamp, so nothing comes back for it: ask for
        // something else and expect that answer next.
        worker.request(Job::Cover(a.clone()));
        let next = wait_for(&worker, |_| true);
        assert!(matches!(next, Done::Cover { .. }), "{next:?}");
    }

    #[test]
    fn a_missing_file_is_reported_as_unreadable() {
        let worker = TagWorker::spawn();
        let ghost = PathBuf::from("/nonexistent/ghost.mp3");
        worker.request(Job::Tags(vec![(ghost.clone(), None)]));
        let Done::Tags { path, tags, .. } = wait_for(&worker, |_| true) else {
            panic!("expected a tags answer");
        };
        assert_eq!((path, tags), (ghost, None));
    }

    #[test]
    fn a_newer_request_replaces_the_older_one() {
        let dir = TempDir::new();
        let files: Vec<PathBuf> = (0..40)
            .map(|i| {
                let p = dir.path().join(format!("{i}.wav"));
                write_wav(&p, 1);
                p
            })
            .collect();
        let worker = TagWorker::spawn();
        worker.request(Job::Tags(
            files[..30].iter().map(|p| (p.clone(), None)).collect(),
        ));
        worker.request(Job::Tags(
            files[30..].iter().map(|p| (p.clone(), None)).collect(),
        ));

        let (mut read, mut dropped) = (Vec::new(), Vec::new());
        for _ in 0..500 {
            while let Some(done) = worker.try_recv() {
                match done {
                    Done::Tags { path, .. } => read.push(path),
                    Done::Dropped(paths) => dropped.extend(paths),
                    Done::Cover { .. } => {}
                }
            }
            // Everything is either read or reported dropped: nothing is silently lost.
            if read.len() + dropped.len() >= 40 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(read.len() + dropped.len(), 40);
        assert!(
            files[30..].iter().all(|p| read.contains(p)),
            "the newest batch is read in full"
        );
    }
}
