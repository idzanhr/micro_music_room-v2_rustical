// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

use slint_build::CompilerConfiguration;

fn main() {
    // `fluent-dark` gives the stock widgets (scrollbars, text selection) a dark base that sits
    // well on the walnut palette. Everything visible is styled by `ui/theme.slint`.
    let config = CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("failed to compile the Slint UI");
}
