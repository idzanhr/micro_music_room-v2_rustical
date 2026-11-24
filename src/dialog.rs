// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The native "choose a folder" dialog.

use std::path::PathBuf;

/// Show the Windows folder picker. Returns `None` if the user cancels.
#[cfg(windows)]
pub fn pick_folder() -> Option<PathBuf> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            },
            UI::Shell::{
                FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
                SIGDN_FILESYSPATH,
            },
        },
        core::w,
    };

    // SAFETY: standard COM calls on the UI thread; every pointer comes from the API itself.
    unsafe {
        // The UI thread is normally already an apartment; a mode conflict here is harmless.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let _ = dialog.SetTitle(w!("Add a music folder"));
        // Show fails with ERROR_CANCELLED when the user closes the dialog.
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.as_ptr().cast()));
        path
    }
}

/// Off Windows there is no native picker yet. `MMR_PICK_FOLDER` stands in for it so the add-folder
/// flow can be tried during development.
#[cfg(not(windows))]
pub fn pick_folder() -> Option<PathBuf> {
    std::env::var_os("MMR_PICK_FOLDER").map(PathBuf::from)
}
