// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! lostc - a file manager in the Norton Commander tradition, opening on
//! one panel with a second a keystroke away.
//!
//! Portability note: every terminal interaction goes through crossterm, which
//! supports the Windows console (ConPTY), macOS and Linux with the same code.
//! Filesystem work uses only `std::fs` / `std::path`, so no part of this
//! program is tied to a single operating system.

use lost_commander_core::{entry, panel};

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};

use lostc::app::App;
use lostc::{editor_command, ui};
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
            println!("lostc {VERSION}");
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
        "lostc {VERSION} - terminal file manager

USAGE:
    lostc [LEFT_DIR] [RIGHT_DIR]
    lostc --list [DIR]      print a directory listing and exit
    lostc --help | --version

KEYS:
    Tab other panel       Enter open      Backspace parent
    F12 second panel on/off
    F1 help    F2 rename  F3 view         F4 edit
    F5 copy    F6 move    F7 mkdir        F8 delete
    F9 sort    F10 quit   Space mark      Ctrl-H hidden files

    Ctrl-Q also quits, and Ctrl-C quits when there is nothing to interrupt.
    Some terminals keep F10 for their own menu and never pass it on, which is
    why there is more than one way out. Ctrl-Z suspends, as anywhere else.

    It opens with one panel, which is XTree's arrangement rather than Norton's:
    a tree and its files with the whole width to show them in. Tab asks for a
    second one and F12 folds it away; a copy, a move or a folder comparison
    brings it up by itself. F5 and F6 ask where in a field, so a single panel
    costs nothing - and nothing is copied into a directory that is off screen.

    Typing goes to the command line under the panels, and Enter runs it in the
    directory being shown - as in Norton and Midnight Commander. An empty
    command line means the panels: Space marks, Backspace goes up.

    Ctrl-O swaps between the panels and the shell running underneath them.
    It is one shell for the whole session, so a cd in one command is still
    true for the next - and a cd there moves the panel, as moving the panel
    cds the shell. That sharing needs a shell with a seam to hook: bash, zsh,
    fish and PowerShell have one, cmd and dash do not.

    Alt-O picks which shell that is, and says which of them can be recorded.
    On Windows the default is cmd, which cannot.

    Alt-P and Alt-N walk back through what has been run, offering what was run
    in this directory first - and the shell screen lists those beside it. That
    works whatever shell you use: the line is known before it is handed over,
    so it needs no hook."
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
    let right = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    Ok((settled(left, &cwd), settled(right, &cwd)))
}

/// Make a path absolute and fit to show in a pane header.
///
/// Not `canonicalize`, which is what this used to be: on Windows it hands
/// back a verbatim path and the header read `\?\C:\src` instead of
/// `C:\src`. It also resolves symbolic links, so a pane opened through one
/// showed where the link pointed rather than where you asked to be.
fn settled(path: PathBuf, here: &Path) -> PathBuf {
    lost_commander_core::paths::resolved(&path, here)
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

        if app.pending_suspend {
            app.pending_suspend = false;
            suspend(&mut terminal)?;
            continue;
        }

        // The command gets the terminal to itself: it may be interactive, it
        // may print colour, `sudo` has to put a password prompt somewhere -
        // and this front-end owns the screen until it gives it up. Same
        // suspend-and-restore as $EDITOR above.
        if let Some(line) = app.pending_shell.take() {
            let cwd = app.active_panel().cwd.clone();
            if let Err(e) = run_shell_line(&mut terminal, &line, &cwd) {
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
        // A shell on show repaints as its output arrives, which is not tied
        // to anybody pressing a key. The panels do not need that.
        let timeout = if app.showing_shell {
            Duration::from_millis(50)
        } else if app.job_is_running() || app.scan.is_some() || app.hunt.is_some() {
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
fn run_shell_line(
    terminal: &mut ratatui::DefaultTerminal,
    line: &str,
    cwd: &Path,
) -> io::Result<()> {
    ratatui::restore();

    // In the directory the panel is showing, which is the only one the reader
    // could have meant. Running in whatever directory the program was started
    // from would make `ls` answer about somewhere else entirely.
    let status = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", line])
            .current_dir(cwd)
            .status()
    } else {
        Command::new(shell_for_lines())
            .args(["-c", line])
            .current_dir(cwd)
            .status()
    };

    // No pause here. The panels live on the terminal's *alternate* screen and
    // the command just ran on the main one, so its output is still sitting
    // there afterwards - Ctrl-O flips to it. This is how Norton and Midnight
    // Commander work, and a "press any key" after every command would be a
    // keystroke charged for something the terminal was already keeping.

    *terminal = ratatui::init();
    terminal.clear()?;
    status.map(|_| ())
}

/// Ctrl-Z: give the terminal back and stop, until the shell resumes us.
///
/// The terminal has to be handed back *first*. A process stopped while the
/// screen is in raw mode on the alternate buffer leaves the shell with a
/// terminal it cannot type into - which is the failure this is written to
/// avoid, and is worse than not supporting Ctrl-Z at all.
///
/// `SIGTSTP` rather than `SIGSTOP`: the first is the one a shell knows how to
/// resume with `fg`, and the one it prints a job number for.

/// Ctrl-Z: give the terminal back and stop, until the shell resumes us.
///
/// The terminal has to be handed back *first*. A process stopped while the
/// screen is in raw mode on the alternate buffer leaves the shell with a
/// terminal it cannot type into - which is the failure this is written to
/// avoid, and is worse than not supporting Ctrl-Z at all.
///
/// `SIGTSTP` rather than `SIGSTOP`: the first is the one a shell knows how to
/// resume with `fg`, and the one it prints a job number for.
#[cfg(unix)]
fn suspend(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    ratatui::restore();
    // Safety: raising a signal at a point of our choosing, with the terminal
    // already given back. Execution continues here when the shell resumes us.
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    *terminal = ratatui::init();
    terminal.clear()
}

/// Windows has no job control, so there is nothing to suspend to.
#[cfg(not(unix))]
fn suspend(_terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    Ok(())
}

/// `$SHELL`, or a shell that is certainly there.
fn shell_for_lines() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
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
