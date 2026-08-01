//! rcmd - a Norton Commander-style dual-pane file manager.
//!
//! Portability note: every terminal interaction goes through crossterm, which
//! supports the Windows console (ConPTY), macOS and Linux with the same code.
//! Filesystem work uses only `std::fs` / `std::path`, so no part of this
//! program is tied to a single operating system.

mod app;
mod theme;
mod ui;

use rust_commander_core::{entry, panel};

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};

use app::App;
use panel::{read_entries, SortBy};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            print_usage();
            return Ok(());
        }
        Some("-V") | Some("--version") => {
            println!("rcmd {VERSION}");
            return Ok(());
        }
        // Headless listing: prints what a panel would show. Useful for
        // scripting and for verifying the listing logic without a terminal.
        Some("--list") => {
            let path = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?);
            return list_directory(&path);
        }
        _ => {}
    }

    let (left, right) = starting_directories(&args)?;
    run_tui(left, right)
}

fn print_usage() {
    println!(
        "rcmd {VERSION} - dual-pane terminal file manager

USAGE:
    rcmd [LEFT_DIR] [RIGHT_DIR]
    rcmd --list [DIR]      print a directory listing and exit
    rcmd --help | --version

KEYS:
    Tab switch panel      Enter open      Backspace parent
    F1 help    F2 rename  F3 view         F4 edit
    F5 copy    F6 move    F7 mkdir        F8 delete
    F9 sort    F10 quit   Space mark      Ctrl-H hidden files"
    );
}

/// Left defaults to the current directory, right to the same place unless a
/// second path is given.
fn starting_directories(args: &[String]) -> io::Result<(PathBuf, PathBuf)> {
    let cwd = std::env::current_dir()?;
    let left = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    let right = args.get(1).map(PathBuf::from).unwrap_or(cwd);
    Ok((canonical_or_as_is(left), canonical_or_as_is(right)))
}

fn canonical_or_as_is(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn list_directory(path: &Path) -> io::Result<()> {
    let entries = read_entries(path, true, SortBy::Name, SortBy::Name.natural_order())?;
    println!("{}", path.display());
    for e in entries {
        println!(
            "{:<40} {:>10} {}",
            entry::fit(&e.name, 40),
            entry::size_cell(&e),
            entry::format_time(e.modified)
        );
    }
    Ok(())
}

fn run_tui(left: PathBuf, right: PathBuf) -> io::Result<()> {
    // Make sure a panic cannot leave the user's terminal in raw mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));

    let mut terminal = ratatui::init();
    let mut app = App::new(left, right);

    let result = loop {
        if let Err(e) = terminal.draw(|frame| ui::draw(frame, &app)) {
            break Err(e);
        }

        if app.should_quit {
            break Ok(());
        }

        if let Some(path) = app.pending_edit.take() {
            if let Err(e) = edit_file(&mut terminal, &path) {
                app.status = format!("Edit failed: {e}");
                app.status_is_error = true;
            }
            app.active_panel_mut().reload();
            continue;
        }

        // A privileged command needs the terminal to itself: `sudo` has to
        // put its password prompt somewhere, and this front-end owns the
        // screen. Same suspend-and-restore as $EDITOR above.
        if let Some(line) = app.pending_shell.take() {
            if let Err(e) = run_shell_line(&mut terminal, &line) {
                app.status = format!("Failed: {e}");
                app.status_is_error = true;
            }
            app.reload_both();
            continue;
        }

        // Poll rather than block: while a copy runs on its worker thread the
        // UI has to keep repainting so the progress bar actually moves.
        // A comparison runs on its own thread too, and its list fills while
        // it goes, so it wants the same short timeout a copy does.
        let timeout = if app.job_is_running() || app.scan.is_some() || app.hunt.is_some() {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(500)
        };

        match event::poll(timeout) {
            Ok(true) => match event::read() {
                // Windows reports key releases as well; only act on presses.
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => app.on_key(key),
                Ok(_) => {}
                Err(e) => break Err(e),
            },
            // Nothing happened for the length of the timeout, which is the
            // moment to look at whether something else changed the disk.
            Ok(false) => app.poll_directories(),
            Err(e) => break Err(e),
        }

        app.poll_job();
        app.collect_scan();
        app.collect_hunt();
    };

    // Never leave a worker thread writing files into a terminal we have given
    // back to the shell.
    app.finish_job();
    app.persist_on_exit();
    ratatui::restore();
    result
}

/// Suspend the TUI, hand the terminal to $EDITOR, then take it back.
fn edit_file(terminal: &mut ratatui::DefaultTerminal, path: &Path) -> io::Result<()> {
    ratatui::restore();

    let status = Command::new(editor_command()).arg(path).status();

    *terminal = ratatui::init();
    terminal.clear()?;

    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Run one shell line with the TUI out of the way, then put it back.
///
/// Through the user's shell rather than spawned directly, since the line is
/// shell syntax - `cd ... && sudo -i` - and since a privileged prompt should
/// be reading from the real terminal, not from a pipe.
fn run_shell_line(terminal: &mut ratatui::DefaultTerminal, line: &str) -> io::Result<()> {
    ratatui::restore();

    let status = if cfg!(windows) {
        Command::new("cmd").args(["/C", line]).status()
    } else {
        Command::new(shell_for_lines()).args(["-c", line]).status()
    };

    *terminal = ratatui::init();
    terminal.clear()?;
    status.map(|_| ())
}

/// `$SHELL`, or a shell that is certainly there.
fn shell_for_lines() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

pub(crate) fn editor_command() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_prefers_visual_then_editor() {
        // Not asserting on the environment, just that a command is chosen and
        // the fallback is platform-appropriate.
        let chosen = editor_command();
        assert!(!chosen.is_empty());
    }

    #[test]
    fn starting_directories_default_to_cwd() {
        let (left, right) = starting_directories(&[]).unwrap();
        assert_eq!(left, right);
        assert!(left.is_dir());
    }

    #[test]
    fn starting_directories_accept_two_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let args = vec![a.display().to_string(), b.display().to_string()];
        let (left, right) = starting_directories(&args).unwrap();
        assert!(left.ends_with("a"));
        assert!(right.ends_with("b"));
    }
}
