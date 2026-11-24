// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

// Release builds are a window app: without this Windows opens a black console beside the window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod dialog;

use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

slint::include_modules!();

/// Hook the player into Windows' media controls: the volume flyout, media keys and notifications.
/// If Windows refuses, the player runs without them and the About box says why.
#[cfg(windows)]
fn connect_windows_media(
    ui: &MainWindow,
    app: &Rc<RefCell<app::App>>,
    identity: &Option<windows::core::HSTRING>,
) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;

    let handle = ui.window().window_handle();
    let raw = handle
        .window_handle()
        .map_err(|e| format!("the window has no handle yet ({e})"))?;
    let RawWindowHandle::Win32(win32) = raw.as_raw() else {
        return Err("the window is not a Win32 window".into());
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    let media = micro_music_room::winmedia::WinMedia::new(hwnd, identity.clone())
        .map_err(|e| e.to_string())?;
    app.borrow_mut().attach_media(Box::new(media));
    Ok(())
}

/// Try to connect now, and if the window is not ready, keep trying for a few seconds.
#[cfg(windows)]
fn connect_windows_media_with_retries(
    ui: &MainWindow,
    app: &Rc<RefCell<app::App>>,
    identity: Option<windows::core::HSTRING>,
) -> slint::Timer {
    let timer = slint::Timer::default();
    let Err(first) = connect_windows_media(ui, app, &identity) else {
        return timer;
    };
    app.borrow_mut().media_unavailable(&first);
    let (weak, app, mut tries) = (ui.as_weak(), app.clone(), 0u32);
    let done = std::cell::Cell::new(false);
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(500),
        move || {
            tries += 1;
            if done.get() || tries > 20 {
                return;
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match connect_windows_media(&ui, &app, &identity) {
                Ok(()) => done.set(true),
                Err(e) => app.borrow_mut().media_unavailable(&e),
            }
        },
    );
    timer
}

fn main() -> Result<(), slint::PlatformError> {
    // Before the window exists, so the taskbar and notifications know who we are.
    #[cfg(windows)]
    let identity = micro_music_room::winmedia::init_app_identity();

    let ui = MainWindow::new()?;
    // Kept alive for the whole run: it hands finished folder scans and tag reads back to the UI
    // thread.
    let app = Rc::new(RefCell::new(app::App::load()));
    let _timer = app::wire(&ui, app.clone());

    // The media controls attach to the real window, so it has to be on screen first.
    ui.show()?;
    #[cfg(windows)]
    let _media_retry = connect_windows_media_with_retries(&ui, &app, identity);

    let result = slint::run_event_loop();
    app::flush(&app);
    result
}
