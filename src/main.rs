// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

mod app;
mod dialog;

use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

slint::include_modules!();

/// Hook the player into Windows' media controls: the volume flyout, media keys and notifications.
/// If Windows refuses, the player simply runs without them.
#[cfg(windows)]
fn connect_windows_media(
    ui: &MainWindow,
    app: &Rc<RefCell<app::App>>,
    identity: Option<windows::core::HSTRING>,
) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;

    let handle = ui.window().window_handle();
    let Ok(raw) = handle.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = raw.as_raw() else {
        return;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    if let Ok(media) = micro_music_room::winmedia::WinMedia::new(hwnd, identity) {
        app.borrow_mut().attach_media(Box::new(media));
    }
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
    connect_windows_media(&ui, &app, identity);

    let result = slint::run_event_loop();
    app::flush(&app);
    result
}
