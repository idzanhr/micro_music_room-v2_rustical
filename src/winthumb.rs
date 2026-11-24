// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The previous / play-pause / next buttons under the taskbar preview of the window.
//!
//! Windows reports a click on one of these as a `WM_COMMAND` sent to the window itself, and the
//! window belongs to the UI toolkit. So a window subclass listens for those clicks and turns them
//! into [`MediaCommand`]s on the same channel the media keys use. Windows only accepts the buttons
//! once the taskbar button exists (it says so with a `TaskbarButtonCreated` message), so adding
//! them is retried from [`ThumbBar::tick`] until it works, and done again if Explorer restarts.

use crate::media::MediaCommand;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc::Sender,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::{
            Shell::{
                DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, THB_FLAGS,
                THB_ICON, THB_TOOLTIP, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON, TaskbarList,
            },
            WindowsAndMessaging::{
                CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR,
                RegisterWindowMessageW, WM_COMMAND,
            },
        },
    },
    core::w,
};

const ID_PREVIOUS: u32 = 0;
const ID_PLAY_PAUSE: u32 = 1;
const ID_NEXT: u32 = 2;
/// Identifies our subclass among any others on the window.
const SUBCLASS_ID: usize = 0x4D4D_5231;
/// Give up re-adding after this many failed tries per "taskbar button created" message.
const MAX_TRIES: u32 = 60;

const PREVIOUS_PNG: &[u8] = include_bytes!("../assets/thumb/prev.png");
const NEXT_PNG: &[u8] = include_bytes!("../assets/thumb/next.png");
const PLAY_PNG: &[u8] = include_bytes!("../assets/thumb/play.png");
const PAUSE_PNG: &[u8] = include_bytes!("../assets/thumb/pause.png");

/// What the window subclass needs. It lives in a `Box` owned by the [`ThumbBar`], and the subclass
/// is removed before the box is freed.
struct Shared {
    commands: Sender<MediaCommand>,
    playing: AtomicBool,
    /// Set when the buttons should be (re)added.
    want_buttons: AtomicBool,
    tries: AtomicU32,
    created_message: u32,
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the address of the `Shared` box, which outlives the subclass.
    let shared = unsafe { &*(data as *const Shared) };
    if message == shared.created_message {
        shared.tries.store(0, Ordering::Relaxed);
        shared.want_buttons.store(true, Ordering::Relaxed);
    } else if message == WM_COMMAND && ((wparam.0 >> 16) & 0xFFFF) as u32 == THBN_CLICKED {
        let command = match (wparam.0 & 0xFFFF) as u32 {
            ID_PREVIOUS => Some(MediaCommand::Previous),
            ID_NEXT => Some(MediaCommand::Next),
            ID_PLAY_PAUSE if shared.playing.load(Ordering::Relaxed) => Some(MediaCommand::Pause),
            ID_PLAY_PAUSE => Some(MediaCommand::Play),
            _ => None,
        };
        if let Some(command) = command {
            let _ = shared.commands.send(command);
            return LRESULT(0);
        }
    }
    // SAFETY: passing the message on, as a subclass must.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn icon(png: &[u8]) -> windows::core::Result<HICON> {
    // SAFETY: Windows reads the PNG bytes and returns an icon it owns until `DestroyIcon`.
    unsafe { CreateIconFromResourceEx(png, true, 0x0003_0000, 32, 32, LR_DEFAULTCOLOR) }
}

fn button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
    let mut b = THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        hIcon: icon,
        dwFlags: THBF_ENABLED,
        ..THUMBBUTTON::default()
    };
    for (slot, unit) in b.szTip.iter_mut().take(259).zip(tip.encode_utf16()) {
        *slot = unit;
    }
    b
}

pub struct ThumbBar {
    taskbar: ITaskbarList3,
    window: HWND,
    shared: Box<Shared>,
    previous: HICON,
    next: HICON,
    play: HICON,
    pause: HICON,
    added: bool,
}

impl ThumbBar {
    pub fn new(window: HWND, commands: Sender<MediaCommand>) -> windows::core::Result<Self> {
        // SAFETY: COM calls on the UI thread, which has joined an apartment by now.
        let taskbar: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)? };
        // SAFETY: as above.
        unsafe { taskbar.HrInit()? };
        let (previous, next, play, pause) = (
            icon(PREVIOUS_PNG)?,
            icon(NEXT_PNG)?,
            icon(PLAY_PNG)?,
            icon(PAUSE_PNG)?,
        );
        let shared = Box::new(Shared {
            commands,
            playing: AtomicBool::new(false),
            want_buttons: AtomicBool::new(true),
            tries: AtomicU32::new(0),
            // SAFETY: registering a well-known message name.
            created_message: unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) },
        });
        // SAFETY: the box's address stays valid until `drop` removes the subclass.
        unsafe {
            SetWindowSubclass(
                window,
                Some(subclass_proc),
                SUBCLASS_ID,
                &*shared as *const Shared as usize,
            )
            .ok()?;
        }
        Ok(Self {
            taskbar,
            window,
            shared,
            previous,
            next,
            play,
            pause,
            added: false,
        })
    }

    fn buttons(&self) -> [THUMBBUTTON; 3] {
        let playing = self.shared.playing.load(Ordering::Relaxed);
        [
            button(ID_PREVIOUS, self.previous, "Previous"),
            if playing {
                button(ID_PLAY_PAUSE, self.pause, "Pause")
            } else {
                button(ID_PLAY_PAUSE, self.play, "Play")
            },
            button(ID_NEXT, self.next, "Next"),
        ]
    }

    /// Call regularly: adds the buttons as soon as Windows will take them.
    pub fn tick(&mut self) {
        if !self.shared.want_buttons.load(Ordering::Relaxed) {
            return;
        }
        let buttons = self.buttons();
        // SAFETY: the window is ours and the buttons hold icons that outlive the call.
        let result = unsafe { self.taskbar.ThumbBarAddButtons(self.window, &buttons) };
        if result.is_ok() {
            self.added = true;
            self.shared.want_buttons.store(false, Ordering::Relaxed);
        } else if self.shared.tries.fetch_add(1, Ordering::Relaxed) >= MAX_TRIES {
            self.shared.want_buttons.store(false, Ordering::Relaxed);
        }
    }

    /// Swap the middle button between play and pause.
    pub fn set_playing(&mut self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
        if self.added {
            let buttons = self.buttons();
            // SAFETY: as in `tick`.
            let _ = unsafe { self.taskbar.ThumbBarUpdateButtons(self.window, &buttons) };
        }
    }
}

impl Drop for ThumbBar {
    fn drop(&mut self) {
        // SAFETY: the subclass is removed before the data it points to is freed.
        unsafe {
            let _ = RemoveWindowSubclass(self.window, Some(subclass_proc), SUBCLASS_ID);
            for icon in [self.previous, self.next, self.play, self.pause] {
                let _ = DestroyIcon(icon);
            }
        }
    }
}
