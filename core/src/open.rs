// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Handing a file to the desktop - the "open" that `Enter` means.
//!
//! Two things get confused under one word, and this module keeps them apart:
//!
//! * **Opening** a document - asking the operating system which application is
//!   registered for it and starting that. Safe, expected, and what every file
//!   manager does on a double-click.
//! * **Executing** the file itself. That is a different act with a different
//!   risk, and it is the one worth a question first.
//!
//! Nothing here ever runs the selected file. It runs *the platform's opener*
//! and hands it a path, so which application starts, and whether the desktop
//! wants to ask about it, stays the desktop's decision rather than becoming a
//! policy of ours. [`runs_code`] exists only to catch the cases where that
//! delegation would amount to execution anyway.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::mount::Platform;
use crate::preview::program_exists;

/// Starts the desktop's handler for a path - [`open`], normally.
///
/// Both front-ends hold one of these as a field rather than calling [`open`]
/// directly, so their tests can watch what would be opened without any real
/// application starting.
pub type Opener = Box<dyn Fn(&Path) -> Result<(), String> + Send>;

/// Starts an already-resolved command - [`launch`], normally.
///
/// The same seam as [`Opener`], one step lower: "Open with..." has already
/// decided *which* application, so what it needs to hand over is a command
/// rather than a path.
pub type Launcher = Box<dyn Fn(&Launch) -> Result<(), String> + Send>;

/// A command to start: resolved for this platform, not yet run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
}

/// The Linux openers, best first.
///
/// `xdg-open` is the standard and is what should normally answer. It is a
/// shell script from `xdg-utils`, though, and a slim container or a minimal
/// desktop may not have it - so the desktops' own equivalents follow, and the
/// first one actually installed wins.
pub const LINUX_OPENERS: &[(&str, &[&str])] = &[
    ("xdg-open", &[]),
    ("gio", &["open"]),
    ("kde-open5", &[]),
    ("kde-open", &[]),
    ("gnome-open", &[]),
    ("exo-open", &[]),
];

/// What Windows will *run* rather than display, from the file name alone.
///
/// On Windows "open" is genuinely execution for these: the registered handler
/// for `.exe` is the program itself, and `ShellExecute` on one starts it with
/// no further ceremony.
const WINDOWS_EXECUTABLE: &[&str] = &[
    "exe", "com", "bat", "cmd", "scr", "pif", "msi", "msp", "ps1", "psm1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "hta", "cpl", "jar", "reg", "lnk",
];

/// Names that mean code on Unix once the execute bit is set.
const UNIX_SCRIPT: &[&str] = &[
    "sh", "bash", "zsh", "ksh", "csh", "fish", "py", "pl", "rb", "lua", "tcl", "php", "r", "jar",
    "run", "appimage", "bin", "out",
];

/// The command that hands `path` to whatever the desktop uses to open it.
///
/// Pure: the platform and the "is this installed" predicate are parameters, so
/// every branch is reachable from a test on any machine.
pub fn open_command(
    platform: Platform,
    path: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<Launch, String> {
    let target = path.display().to_string();
    match platform {
        // Always present, and the same call Finder makes.
        Platform::MacOs => Ok(Launch {
            program: "open".into(),
            args: vec![target],
        }),

        // `ShellExec_RunDLL` is `ShellExecuteEx` - what Explorer does on a
        // double-click - reached without a shell in the way.
        //
        // The obvious `cmd /C start "" <path>` is wrong twice over. `start`
        // reads a leading quoted token as the *window title*, which is what
        // the empty pair is working around; and `cmd` re-parses its command
        // line, so `&`, `^`, `|` and `%` in a file name break out of it.
        // Rust quotes arguments by the C runtime's rules, which `cmd` does not
        // use, so there is no quoting that makes an arbitrary path safe
        // through it. `rundll32` takes its argument through argv untouched.
        Platform::Windows => Ok(Launch {
            program: "rundll32.exe".into(),
            args: vec!["shell32.dll,ShellExec_RunDLL".into(), target],
        }),

        Platform::Linux => {
            for (program, prefix) in LINUX_OPENERS {
                if program_exists(program, exists) {
                    let mut args: Vec<String> = prefix.iter().map(|a| (*a).to_string()).collect();
                    args.push(target);
                    return Ok(Launch {
                        program: (*program).to_string(),
                        args,
                    });
                }
            }
            Err("no desktop opener found - install xdg-utils".into())
        }
    }
}

/// Whether opening this would run code rather than show something.
///
/// `executable` is the file's execute bit, which [`is_executable`] reads;
/// passing it in keeps this a pure function of facts the caller already has.
pub fn runs_code(platform: Platform, path: &Path, executable: bool) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match platform {
        Platform::Windows => WINDOWS_EXECUTABLE.contains(&ext.as_str()),
        _ => {
            // A launcher runs what it names whether or not it is itself
            // marked executable, which is exactly why a stray `.desktop` in a
            // downloads folder is a classic way in.
            if ext == "desktop" {
                return true;
            }
            // The execute bit on its own proves nothing: a FAT stick or an
            // SMB share reports every file as 0777, holiday photos included.
            // It counts when the name agrees - or says nothing at all, which
            // is what a compiled binary looks like.
            executable && (ext.is_empty() || UNIX_SCRIPT.contains(&ext.as_str()))
        }
    }
}

/// The most files `Enter` hands over at once without asking.
///
/// Opening is one window per file, so a stray `Ctrl-A` followed by `Enter` is
/// how you end up with two hundred of them. Five is about where "I meant that"
/// stops being the obvious reading.
pub const OPEN_MANY: usize = 5;

/// Why opening this set is worth a question first, if it is.
///
/// `None` means go ahead. Each target carries its own execute bit, since
/// reading it is a filesystem call and this stays pure.
///
/// The wording has to match what the button will actually do. A set of eight
/// documents with one script among them is not "run it?" - confirming opens
/// the eight *and* runs the one - and a question that describes only part of
/// what follows is worse than no question, because it is answered anyway.
pub fn open_warning(platform: Platform, targets: &[(std::path::PathBuf, bool)]) -> Option<String> {
    let programs: Vec<String> = targets
        .iter()
        .filter(|(path, executable)| runs_code(platform, path, *executable))
        .map(|(path, _)| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into()
        })
        .collect();

    let total = targets.len();
    match (programs.len(), total) {
        (0, _) => {}
        // Nothing here but programs, so "run" is the whole story.
        (1, 1) => return Some(format!("{} is a program. Run it?", programs[0])),
        (n, t) if n == t => return Some(format!("All {n} of these are programs. Run them?")),
        // Mixed: lead with what is about to happen to the set, and name the
        // part of it that is execution.
        (1, t) => {
            return Some(format!(
                "Open these {t}? {} is a program and will be run.",
                programs[0]
            ))
        }
        (n, t) => {
            return Some(format!(
                "Open these {t}? {n} of them are programs and will be run."
            ))
        }
    }

    if total > OPEN_MANY {
        return Some(format!("Open {total} files? That is {total} windows."));
    }
    None
}

/// The file's execute bit. Always false on Windows, which has no such thing.
#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_executable(_path: &Path) -> bool {
    false
}

/// Start it, and do not wait for it.
///
/// Three deliberate choices, each of which is a bug if it goes the other way:
///
/// * **stdio is null.** The terminal front-end is in raw mode on the alternate
///   screen; a handler that inherited the tty and printed one line would draw
///   over the panels. Closing stdin also stops anything that wants to prompt
///   from hanging on a terminal nobody is reading.
/// * **The wait happens on a thread.** `open` and `rundll32` return at once,
///   but `xdg-open` can live as long as the application it started, and
///   nothing here may block a frame. The thread exists to reap the child
///   rather than leave a zombie.
/// * **Only the failure to *start* is reported.** Whether the application then
///   liked the file is between it and the user; the file manager's job ended
///   when the handler was launched.
pub fn launch(launch: &Launch) -> Result<(), String> {
    let child = Command::new(&launch.program)
        .args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", launch.program))?;

    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Resolve and start, for this machine. What both front-ends call.
///
/// The path must be absolute - which is what a panel holds - since an opener
/// given something beginning with `-` would read it as an option.
pub fn open(path: &Path) -> Result<(), String> {
    let plan = open_command(Platform::current(), path, &crate::preview::on_disk)?;
    launch(&plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// An `exists` predicate that only knows about the named programs.
    fn installed(names: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path: &Path| {
            path.file_name()
                .map(|n| names.contains(&n.to_string_lossy().as_ref()))
                .unwrap_or(false)
        }
    }

    fn none(_: &Path) -> bool {
        false
    }

    #[test]
    fn macos_uses_open() {
        let plan = open_command(Platform::MacOs, Path::new("/tmp/a.pdf"), &none).unwrap();
        assert_eq!(plan.program, "open");
        assert_eq!(plan.args, vec!["/tmp/a.pdf".to_string()]);
    }

    #[test]
    fn windows_goes_through_shellexecute_and_never_through_cmd() {
        let plan = open_command(Platform::Windows, Path::new(r"C:\a b\r&d.txt"), &none).unwrap();
        assert_eq!(plan.program, "rundll32.exe");
        assert_eq!(plan.args[0], "shell32.dll,ShellExec_RunDLL");
        // The path arrives whole, and no shell is asked to re-read it.
        assert_eq!(plan.args[1], r"C:\a b\r&d.txt");
        assert!(!plan.program.contains("cmd"));
        assert!(!plan.args.iter().any(|a| a == "start"));
    }

    #[test]
    fn linux_prefers_xdg_open() {
        let exists = installed(&["xdg-open", "gio"]);
        let plan = open_command(Platform::Linux, Path::new("/tmp/a.txt"), &exists).unwrap();
        assert_eq!(plan.program, "xdg-open");
        assert_eq!(plan.args, vec!["/tmp/a.txt".to_string()]);
    }

    #[test]
    fn linux_falls_back_to_whatever_is_installed() {
        let exists = installed(&["gio"]);
        let plan = open_command(Platform::Linux, Path::new("/tmp/a.txt"), &exists).unwrap();
        assert_eq!(plan.program, "gio");
        assert_eq!(
            plan.args,
            vec!["open".to_string(), "/tmp/a.txt".to_string()]
        );

        let exists = installed(&["exo-open"]);
        let plan = open_command(Platform::Linux, Path::new("/tmp/a.txt"), &exists).unwrap();
        assert_eq!(plan.program, "exo-open");
    }

    #[test]
    fn linux_with_no_opener_says_what_to_install() {
        let err = open_command(Platform::Linux, Path::new("/tmp/a.txt"), &none).unwrap_err();
        assert!(err.contains("xdg-utils"), "{err}");
    }

    #[test]
    fn the_path_is_passed_through_untouched() {
        // Spaces, ampersands and quotes all survive, on every platform, since
        // each opener takes the path as one argv entry.
        let awkward = PathBuf::from("/tmp/a b & c's file.txt");
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            let exists = installed(&["xdg-open"]);
            let plan = open_command(platform, &awkward, &exists).unwrap();
            assert_eq!(
                plan.args.last().unwrap(),
                &awkward.display().to_string(),
                "{platform:?}"
            );
        }
    }

    #[test]
    fn windows_executables_are_recognised_by_extension() {
        for name in ["setup.exe", "go.BAT", "x.Cmd", "s.ps1", "m.msi", "l.lnk"] {
            assert!(
                runs_code(Platform::Windows, Path::new(name), false),
                "{name}"
            );
        }
        for name in ["notes.txt", "photo.jpg", "readme"] {
            assert!(
                !runs_code(Platform::Windows, Path::new(name), false),
                "{name}"
            );
        }
    }

    #[test]
    fn a_desktop_file_counts_even_without_the_bit() {
        assert!(runs_code(
            Platform::Linux,
            Path::new("invoice.desktop"),
            false
        ));
    }

    #[test]
    fn the_execute_bit_alone_does_not_make_a_photo_code() {
        // Everything on a FAT stick looks executable. That must not turn
        // every holiday photo into a confirmation dialog.
        assert!(!runs_code(Platform::Linux, Path::new("photo.jpg"), true));
        assert!(!runs_code(Platform::Linux, Path::new("notes.txt"), true));
        assert!(!runs_code(Platform::MacOs, Path::new("song.mp3"), true));
    }

    #[test]
    fn a_script_or_a_bare_binary_with_the_bit_does() {
        assert!(runs_code(Platform::Linux, Path::new("build.sh"), true));
        assert!(runs_code(Platform::Linux, Path::new("tool.PY"), true));
        assert!(runs_code(Platform::Linux, Path::new("rcmd"), true)); // no extension
        assert!(runs_code(Platform::Linux, Path::new("App.AppImage"), true));
        // ...and without the bit they are just text.
        assert!(!runs_code(Platform::Linux, Path::new("build.sh"), false));
        assert!(!runs_code(Platform::Linux, Path::new("rcmd"), false));
    }

    #[cfg(unix)]
    #[test]
    fn the_execute_bit_is_read_from_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.txt");
        let script = dir.path().join("run.sh");
        std::fs::write(&plain, b"hello").unwrap();
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!is_executable(&plain));
        assert!(is_executable(&script));
        // A directory is not an executable file, whatever its bits say.
        assert!(!is_executable(dir.path()));
        assert!(!is_executable(&dir.path().join("gone")));
    }

    fn targets(names: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        names
            .iter()
            .map(|(n, x)| (PathBuf::from("/tmp").join(n), *x))
            .collect()
    }

    #[test]
    fn a_handful_of_documents_opens_without_a_question() {
        let set = targets(&[("a.txt", false), ("b.png", false), ("c.pdf", false)]);
        assert_eq!(open_warning(Platform::Linux, &set), None);
    }

    #[test]
    fn one_program_on_its_own_is_asked_about_by_name() {
        let set = targets(&[("setup.exe", false)]);
        let question = open_warning(Platform::Windows, &set).unwrap();
        assert!(question.contains("setup.exe"), "{question}");
        assert!(question.contains("Run it?"), "{question}");
    }

    #[test]
    fn a_set_of_nothing_but_programs_says_so() {
        let set = targets(&[("a.sh", true), ("b.py", true)]);
        let question = open_warning(Platform::Linux, &set).unwrap();
        assert!(question.contains("All 2"), "{question}");
        assert!(question.contains("Run them?"), "{question}");
    }

    #[test]
    fn a_mixed_set_says_what_will_actually_happen_to_it() {
        // Eight documents and a script is not "run it?" - confirming opens
        // the eight and runs the one, and the question has to say that or it
        // is answered on a false premise.
        let mut set = targets(&[("build.sh", true)]);
        set.extend(targets(&[
            ("a.txt", false),
            ("b.txt", false),
            ("c.txt", false),
        ]));
        let question = open_warning(Platform::Linux, &set).unwrap();
        assert!(question.contains("Open these 4"), "{question}");
        assert!(question.contains("build.sh"), "{question}");
        assert!(question.contains("will be run"), "{question}");

        // Several programs among documents: the count, not a name.
        let mut set = targets(&[("a.sh", true), ("b.py", true)]);
        set.extend(targets(&[("c.txt", false), ("d.txt", false)]));
        let question = open_warning(Platform::Linux, &set).unwrap();
        assert!(question.contains("Open these 4"), "{question}");
        assert!(question.contains("2 of them"), "{question}");
    }

    #[test]
    fn opening_a_whole_marked_directory_is_asked_about_too() {
        let many: Vec<(&str, bool)> = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .map(|n| (*n, false))
            .collect();
        // Names without extensions, so this is the count talking and not
        // `runs_code` - none of them carries the bit.
        let question = open_warning(Platform::Linux, &targets(&many)).unwrap();
        assert!(question.contains('7'), "{question}");

        // ...and exactly the limit still goes straight through.
        let at_limit: Vec<(&str, bool)> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| (*n, false))
            .collect();
        assert_eq!(open_warning(Platform::Linux, &targets(&at_limit)), None);
    }

    #[test]
    fn a_program_is_asked_about_even_when_it_is_the_only_one() {
        // The count check must not shadow the program check: one executable
        // among four documents is the case that matters most.
        let set = targets(&[
            ("a.txt", false),
            ("run.sh", true),
            ("b.txt", false),
            ("c.txt", false),
        ]);
        let question = open_warning(Platform::Linux, &set).unwrap();
        assert!(question.contains("run.sh"), "{question}");
    }

    #[test]
    fn launching_something_that_is_not_there_reports_it() {
        let err = launch(&Launch {
            program: "rcmd-no-such-opener".into(),
            args: vec!["/tmp/x".into()],
        })
        .unwrap_err();
        assert!(err.contains("rcmd-no-such-opener"), "{err}");
    }
}
