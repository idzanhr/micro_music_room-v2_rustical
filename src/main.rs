// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod dialog;

use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    // Kept alive for the whole run: it hands finished folder scans back to the UI thread.
    let _scan_timer = app::wire(&ui, Rc::new(RefCell::new(app::App::load())));
    ui.run()
}
