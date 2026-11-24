// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The native "choose a folder" and "choose songs" dialogs.

use std::path::PathBuf;

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FILEOPENDIALOGOPTIONS, FOS_ALLOWMULTISELECT,
                FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
                SIGDN_FILESYSPATH,
            },
        },
        core::{PCWSTR, w},
    };

    /// A new file dialog with the given extra options.
    ///
    /// # Safety
    /// Calls COM; must run on the UI thread.
    unsafe fn dialog(extra: FILEOPENDIALOGOPTIONS, title: PCWSTR) -> Option<IFileOpenDialog> {
        unsafe {
            // The UI thread is normally already an apartment; a mode conflict here is harmless.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let options = dialog.GetOptions().ok()?;
            dialog
                .SetOptions(options | FOS_FORCEFILESYSTEM | extra)
                .ok()?;
            let _ = dialog.SetTitle(title);
            Some(dialog)
        }
    }

    /// The file-system path of a shell item.
    ///
    /// # Safety
    /// Calls COM; `item` must be a valid shell item.
    unsafe fn path_of(item: &IShellItem) -> Option<PathBuf> {
        unsafe {
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = name.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(name.as_ptr().cast()));
            path
        }
    }

    pub fn pick_folder() -> Option<PathBuf> {
        // SAFETY: standard COM calls on the UI thread; every pointer comes from the API itself.
        unsafe {
            let dialog = dialog(FOS_PICKFOLDERS, w!("Add a music folder"))?;
            // Show fails with ERROR_CANCELLED when the user closes the dialog.
            dialog.Show(None).ok()?;
            path_of(&dialog.GetResult().ok()?)
        }
    }

    pub fn pick_files() -> Vec<PathBuf> {
        // SAFETY: as above.
        unsafe {
            let Some(dialog) = dialog(FOS_ALLOWMULTISELECT, w!("Add songs")) else {
                return Vec::new();
            };
            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Music files"),
                    pszSpec: w!("*.mp3;*.m4a;*.wav;*.aac;*.wma;*.flac"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = dialog.SetFileTypes(&filters);
            if dialog.Show(None).is_err() {
                return Vec::new();
            }
            let Ok(results) = dialog.GetResults() else {
                return Vec::new();
            };
            let count = results.GetCount().unwrap_or(0);
            (0..count)
                .filter_map(|i| results.GetItemAt(i).ok())
                .filter_map(|item| path_of(&item))
                .collect()
        }
    }
}

/// Show the Windows folder picker. Returns `None` if the user cancels.
#[cfg(windows)]
pub fn pick_folder() -> Option<PathBuf> {
    win::pick_folder()
}

/// Show the Windows multi-select file picker. Empty if the user cancels.
#[cfg(windows)]
pub fn pick_files() -> Vec<PathBuf> {
    win::pick_files()
}

/// Off Windows there is no native picker yet. `MMR_PICK_FOLDER` stands in for it so the add-folder
/// flow can be tried during development.
#[cfg(not(windows))]
pub fn pick_folder() -> Option<PathBuf> {
    std::env::var_os("MMR_PICK_FOLDER").map(PathBuf::from)
}

/// Off Windows, `MMR_PICK_FILES` (a `:`-separated list of paths) stands in for the picker.
#[cfg(not(windows))]
pub fn pick_files() -> Vec<PathBuf> {
    std::env::var_os("MMR_PICK_FILES")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default()
}
