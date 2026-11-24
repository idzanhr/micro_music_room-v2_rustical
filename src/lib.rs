// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Core logic for Micro Music Room, kept free of UI so it can be unit-tested anywhere. The only
//! platform-specific code is `wmf` (Windows Media Foundation), compiled on Windows alone.

pub mod decode;
pub mod engine;
pub mod library;
pub mod model;
pub mod queue;
pub mod scan;
pub mod tags;
pub mod track;
#[cfg(windows)]
pub mod wmf;

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A throwaway directory removed on drop (avoids a dev-dependency).
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("mmr-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
