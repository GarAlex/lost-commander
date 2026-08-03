// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The panel and the shell agreeing about where they are.
//!
//! Driven against a real shell on a real pseudo-terminal, because that is the
//! only thing that proves it. The hook is a startup file the shell reads and
//! a sequence it prints afterwards; a test that stubbed either would be
//! checking that this file agrees with itself.
//!
//! Skipped, not failed, where the machine has no shell with a seam to hook.
//! `cmd` has none, so on a bare Windows box there is nothing to prove and
//! failing would only teach people to ignore the suite.

use std::time::{Duration, Instant};

/// A shell this machine has that the hook knows how to write a startup file
/// for, or `None`.
fn hookable() -> Option<String> {
    // PowerShell before bash on Windows: both are hooked, but a
    // Unix-flavoured bash answers in its own namespace and PowerShell answers
    // in the one the panels use, so PowerShell is the one that can actually
    // be followed. On Unix the first name wins and that is bash.
    let order: &[&str] = if cfg!(windows) {
        &["pwsh", "powershell", "bash", "zsh", "fish"]
    } else {
        &["bash", "zsh", "fish", "pwsh", "powershell"]
    };
    for name in order.iter().copied() {
        if let Some(found) = which(name) {
            return Some(found);
        }
    }
    None
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for suffix in ["", ".exe"] {
            let candidate = dir.join(format!("{name}{suffix}"));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Wait for `f` to hold, or give up. Polling rather than sleeping a fixed
/// time: a shell starts in tens of milliseconds on one machine and hundreds
/// on another, and a test that guessed would be flaky on whichever it was not
/// written on.
fn within<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if let Some(got) = f() {
            return Some(got);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[test]
fn the_panel_follows_a_cd_typed_into_the_shell() {
    let Some(shell) = hookable() else {
        eprintln!("no hookable shell on this machine - skipped");
        return;
    };

    let root = tempfile::tempdir().expect("a temporary directory");
    let here = root.path().join("here");
    let there = root.path().join("there");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&there).unwrap();

    let mut app = lostc::app::App::new(here.clone(), root.path().to_path_buf());
    // Named, not left to the machine: on Windows the machine's answer is
    // `cmd`, which has no seam for the hook - so without this the test would
    // skip on the platform it is being written on.
    app.shell_program = Some(shell.clone());
    app.send_to_shell(&format!("cd '{}'", there.display()));

    if app.shell.is_none() {
        eprintln!("{shell} would not start - skipped");
        return;
    }

    // The shell reports where it is through the hook. If it never does, this
    // shell has no seam and there is nothing to test.
    let reported = within(Duration::from_secs(20), || {
        app.shell.as_ref().and_then(|s| s.shell_cwd())
    });
    let Some(reported) = reported else {
        eprintln!("{shell} never said where it was - not hooked, skipped");
        return;
    };

    // It may report where it started before it reports where it went.
    let moved = within(Duration::from_secs(20), || {
        let now = app.shell.as_ref().and_then(|s| s.shell_cwd())?;
        (now != here).then_some(now)
    });
    let Some(moved) = moved else {
        panic!("the shell said {} and never moved", reported.display());
    };

    let before = app.active_panel().cwd.clone();
    app.follow_the_shell();

    if moved.is_dir() {
        assert_eq!(
            app.active_panel().cwd.canonicalize().unwrap(),
            moved.canonicalize().unwrap(),
            "the panel goes where the shell went"
        );
        return;
    }

    // The shell answered in a namespace this side of the pty cannot use. A
    // Unix-flavoured bash on Windows - Git Bash, MSYS, WSL - calls a
    // directory under `AppData\Local\Temp` something like `/tmp/x`, and
    // nothing translates between the two. The panel must then stay where it
    // is: sending it somewhere that does not exist here would empty it,
    // which is worse than not following at all.
    assert_eq!(
        app.active_panel().cwd,
        before,
        "a path this side cannot use is one the panel must not be sent to: {}",
        moved.display()
    );
    eprintln!(
        "{shell} answers in another namespace ({}) - the fail-safe path is what got tested",
        moved.display()
    );
}

#[test]
fn the_shell_follows_the_panel() {
    let Some(shell) = hookable() else {
        eprintln!("no hookable shell on this machine - skipped");
        return;
    };

    let root = tempfile::tempdir().expect("a temporary directory");
    let here = root.path().join("here");
    let there = root.path().join("there");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&there).unwrap();

    let mut app = lostc::app::App::new(here.clone(), root.path().to_path_buf());
    app.shell_program = Some(shell.clone());
    // Start the shell without moving it.
    app.send_to_shell("cd .");
    if app.shell.is_none() {
        eprintln!("{shell} would not start - skipped");
        return;
    }
    let started = within(Duration::from_secs(20), || {
        app.shell.as_ref().and_then(|s| s.shell_cwd())
    });
    if started.is_none() {
        eprintln!("{shell} is not hooked - skipped");
        return;
    }

    // The other direction: the panel moves, and the shell is told.
    app.active_panel_mut().chdir(there.clone());
    app.tell_the_shell();

    // Compared by *change*, not by spelling: the shell may report the same
    // directory under a name of its own, and what is being tested is that it
    // moved when it was told to.
    let arrived = within(Duration::from_secs(20), || {
        let now = app.shell.as_ref().and_then(|s| s.shell_cwd())?;
        (Some(&now) != started.as_ref()).then_some(now)
    });
    assert!(
        arrived.is_some(),
        "the shell should have followed the panel to {} (it was at {:?})",
        there.display(),
        started
    );
}
