// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! This process's slider in the Windows Volume Mixer, found through the audio session manager of
//! the default output device.

use crate::mixer::Session;
use windows::{
    Win32::{
        Media::Audio::{
            IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
            MMDeviceEnumerator, eMultimedia, eRender,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance},
    },
    core::Interface,
};

pub struct MixerSession(ISimpleAudioVolume);

impl Session for MixerSession {
    fn get(&self) -> Option<f32> {
        // SAFETY: a plain COM getter on an interface we hold.
        unsafe { self.0.GetMasterVolume().ok() }
    }

    fn set(&self, volume: f32) -> bool {
        // SAFETY: a null event context is allowed and means "no particular source".
        unsafe {
            self.0
                .SetMasterVolume(volume.clamp(0.0, 1.0), std::ptr::null())
                .is_ok()
        }
    }
}

/// The mixer entry of this process, if it has one yet (it appears once sound plays).
pub fn find() -> Option<MixerSession> {
    // SAFETY: COM calls on the UI thread, which has initialised COM at start-up.
    unsafe {
        let devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let sessions = manager.GetSessionEnumerator().ok()?;
        let me = std::process::id();
        for i in 0..sessions.GetCount().ok()? {
            let Ok(control) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(details) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            if details.GetProcessId().ok() == Some(me)
                && let Ok(volume) = control.cast::<ISimpleAudioVolume>()
            {
                return Some(MixerSession(volume));
            }
        }
        None
    }
}
