//! The terminal front-end, as a library.
//!
//! A library and a binary, like the graphical crate, and for the same reason:
//! `main.rs` reads arguments and owns the terminal, and everything else can
//! then be driven by a test. The screen test next door renders these very
//! modules into text, which is not possible against a binary.

pub mod app;
pub mod theme;
pub mod ui;

/// The editor to hand a file to.
///
/// `$VISUAL` before `$EDITOR`, which is the convention: the first is for a
/// full-screen editor and the second may be a line editor for a terminal that
/// cannot do better. The fallbacks are the two editors that are always there.
pub fn editor_command() -> String {
    if let Ok(editor) = std::env::var("VISUAL") {
        if !editor.is_empty() {
            return editor;
        }
    }
    if let Ok(editor) = std::env::var("EDITOR") {
        if !editor.is_empty() {
            return editor;
        }
    }
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}
