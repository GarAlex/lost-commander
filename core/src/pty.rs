// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Real interactive shells, each in its own pseudo-terminal.
//!
//! The shell here is not simulated in any way: it is the user's actual shell
//! binary, running on a pty, sourcing its own startup files, doing its own
//! completion and history and job control. What *is* emulated is the terminal
//! device it talks to - the VT100 whose escape sequences every shell expects.
//! That is the same job iTerm2, Windows Terminal and an editor's terminal
//! panel do, and [`vt100`] does the parsing.
//!
//! Compared with [`crate::shell`], which runs one command and collects its
//! output, a session here is persistent: `export` sticks, `cd` sticks, and
//! `vim` works.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Lines of scrollback each session keeps.
pub const SCROLLBACK: usize = 5_000;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The arguments that make a shell behave the way a terminal panel needs.
///
/// Without these a POSIX shell started on a pty is still *non-interactive*: it
/// prints no prompt, sources no rc file, and keeps no history, so the user
/// gets none of their aliases or `PATH`. On macOS it also has to be a **login**
/// shell, because that is where `PATH` is actually assembled - `/etc/profile`
/// and `~/.zprofile` are login-only.
///
/// Shells outside the POSIX family are left alone: `fish` and `nu` are
/// interactive by default and reject these flags.
pub fn interactive_args(program: &str, platform: crate::mount::Platform) -> Vec<String> {
    let name = crate::shell::program_name(program);
    let posix = matches!(
        name.as_str(),
        "sh" | "bash" | "rbash" | "zsh" | "dash" | "ksh" | "mksh" | "ash"
    );
    if !posix {
        return Vec::new();
    }
    match platform {
        crate::mount::Platform::MacOs => vec!["-l".to_string(), "-i".to_string()],
        crate::mount::Platform::Linux => vec!["-i".to_string()],
        // cmd and PowerShell never reach here; a Git-Bash on Windows does.
        crate::mount::Platform::Windows => vec!["-i".to_string()],
    }
}

/// Put the hook's own options together with the ones that make a shell
/// interactive.
///
/// Order matters, and not obviously: bash parses its long options before its
/// short ones and refuses one that arrives afterwards, so `bash -i --rcfile x`
/// is an error where `bash --rcfile x -i` is a working shell. The hook's
/// arguments therefore go first, and anything the hook has replaced comes out.
pub fn arguments_for(interactive: &[String], hook: &[String], without: &[String]) -> Vec<String> {
    let mut out: Vec<String> = hook.to_vec();
    out.extend(
        interactive
            .iter()
            .filter(|argument| !without.contains(argument))
            .cloned(),
    );
    out
}

/// A file name for a saved transcript: the shell, when it was saved, `.log`.
///
/// The stamp is a parameter rather than read from the clock, so the name is
/// testable and the caller decides what "now" means.
pub fn transcript_name(title: &str, stamp: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "shell" } else { slug };
    format!("{slug}-{stamp}.log")
}

/// The plain shell these tests drive, and the few things they need to say
/// to it, spelled in its own language.
///
/// Most of this file's tests are about the pty and the emulator - the
/// scrollback, resizing, the transcript, the recording - and a shell is
/// only a convenient way to make output appear. Naming `/bin/sh` in each
/// of them made every one a Unix test for no reason. They ask here for
/// what they need instead, and run against whatever shell the platform
/// has, which is the whole point of driving a real one.
///
/// Lives out here rather than inside `mod tests` because the graphical
/// front-end's terminal tests need exactly the same answers - and now that
/// they are a separate crate, "the same answers" has to be something this one
/// can actually hand over. `#[cfg(test)]` code is not compiled into the
/// library another crate links against, and `pub(crate)` would not be visible
/// if it were, so the helpers are behind a feature that only a dev-dependency
/// turns on. It stays out of any real build: nothing depends on
/// `lost-commander-core` with `testing` except test targets.
#[cfg(any(test, feature = "testing"))]
pub mod plain {
    /// The shell to spawn.
    pub fn program() -> &'static str {
        if cfg!(windows) {
            "cmd.exe"
        } else {
            "/bin/sh"
        }
    }

    /// A shell with a seam to hook, or `None` if this machine has not got one.
    ///
    /// Anything about the account of what was typed needs one of these, and
    /// [`program`] is not it: the default shell on Windows is `cmd`, which has
    /// no seam and is correctly reported as "not recorded" instead. Tests
    /// about the hook therefore ask for a shell that has one, and say they
    /// were skipped when there is none, rather than asserting against a
    /// warning that is the right answer.
    ///
    /// Only the graphical front-end's tests ask for one.
    pub fn hookable() -> Option<String> {
        // The three the hook knows how to write a startup file for.
        found(&["bash", "zsh", "fish"])
    }

    /// Where the first of `names` is, or `None` if this machine has none.
    ///
    /// zsh and fish are not on every developer's machine and are certainly not
    /// on every build box, so the tests that need them say so and are skipped
    /// rather than failing for the wrong reason.
    ///
    /// The two fixed directories are where Unix keeps shells; PATH is searched
    /// as well because Windows has no such convention, and a Git Bash lands
    /// wherever its installer chose. Without that, the tests needing bash
    /// would not fail on Windows - they would quietly skip even on a machine
    /// that has one, which is the worse outcome.
    pub fn found(names: &[&str]) -> Option<String> {
        let mut dirs = vec![
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/bin"),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        // Only Windows needs the suffix, but asking for the bare name second
        // costs nothing and keeps one code path.
        let suffixes: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };

        for name in names {
            for dir in &dirs {
                for suffix in suffixes {
                    let candidate = dir.join(format!("{name}{suffix}"));
                    if candidate.is_file() {
                        return Some(candidate.to_string_lossy().into_owned());
                    }
                }
            }
        }
        None
    }

    /// What a tab running it is called - `shell::program_name` of the above.
    pub fn title() -> &'static str {
        if cfg!(windows) {
            "cmd"
        } else {
            "sh"
        }
    }

    /// Set a variable, then print it back with `prefix` in front. Two
    /// separate lines, because the point is that the second command still
    /// sees what the first one did.
    pub fn set_then_echo(name: &str, value: &str, prefix: &str) -> [String; 2] {
        if cfg!(windows) {
            [
                format!("set {name}={value}"),
                format!("echo {prefix}%{name}%"),
            ]
        } else {
            [format!("{name}={value}"), format!("echo {prefix}${name}")]
        }
    }

    /// Print the working directory.
    pub fn print_cwd() -> &'static str {
        // `cd` with no argument is how cmd prints it.
        if cfg!(windows) {
            "cd"
        } else {
            "pwd"
        }
    }

    /// List the current directory, bare names only.
    pub fn list() -> &'static str {
        if cfg!(windows) {
            "dir /b"
        } else {
            "ls"
        }
    }

    /// Copy a file to the terminal byte for byte.
    ///
    /// Used where the bytes matter and the shell must not be allowed to
    /// touch them - carriage returns that overwrite a line, an escape
    /// sequence that switches screens. Typing those at a prompt would not
    /// do: a bare escape is what the Windows line editor uses to clear
    /// what you have typed, so it would never reach the emulator.
    pub fn dump(file: &str) -> String {
        if cfg!(windows) {
            format!("type {file}")
        } else {
            format!("cat {file}")
        }
    }

    /// A command printing `count` numbered lines, with the labels it will
    /// print, in order.
    ///
    /// The numbers are all the same width, because the tests look for a
    /// label by substring and `line-1` would otherwise be found inside
    /// `line-10`. Starting at a round hundred buys that width for one
    /// addition, where zero-padding needs a different incantation in
    /// every shell. Both commands stay well under eighty columns, so the
    /// echo of the command itself never wraps.
    pub fn numbered(prefix: &str, count: usize) -> (String, Vec<String>) {
        assert!(
            (1..=899).contains(&count),
            "the fixed three-digit width only covers 1..=899 lines"
        );
        let (first, last) = (101, 100 + count);
        let command = if cfg!(windows) {
            format!("for /l %i in ({first},1,{last}) do @echo {prefix}-%i")
        } else {
            format!("i={first}; while [ $i -le {last} ]; do echo {prefix}-$i; i=$((i+1)); done")
        };
        let labels = (first..=last).map(|n| format!("{prefix}-{n}")).collect();
        (command, labels)
    }

    /// Print `text` from a process that is not the shell, after a moment.
    ///
    /// The delay is what makes it clearly another process's output rather
    /// than part of running the command.
    pub fn background_echo(text: &str) -> String {
        if cfg!(windows) {
            // `start /b` shares this console rather than opening a window,
            // and ping is the sleep every cmd script has always used.
            format!(r#"start /b cmd /c "ping -n 2 127.0.0.1 >nul & echo {text}""#)
        } else {
            format!("(sleep 0.2; echo {text}) &")
        }
    }
}

/// Watches the stream for queries a terminal is expected to answer.
///
/// A terminal is not only a screen. A program may ask it where the cursor is
/// and wait for the reply before printing anything else, and this one never
/// replied. On Unix that cost a hang in whatever asked - a prompt measuring
/// what it had just printed, mostly. On Windows it was the whole panel:
/// ConPTY opens by asking, and renders not one byte until it is told, so a
/// terminal tab there stayed blank forever and looked like a shell that had
/// failed to start. It had started; it was waiting for us.
///
/// Only the cursor-position report is answered, because it is the only one
/// anything has been seen to block on. Replying to queries nobody asked would
/// be inventing answers, which is the failure this codebase is most careful
/// about.
struct Answering {
    /// The tail of the previous chunk, so a query split across two reads is
    /// still recognised. One byte short of the sequence is all that can matter.
    tail: Vec<u8>,
}

impl Answering {
    /// Device Status Report, "where is the cursor?".
    const QUERY: &'static [u8] = b"\x1b[6n";

    fn new() -> Self {
        Answering { tail: Vec::new() }
    }

    /// How many cursor-position queries `chunk` completed.
    ///
    /// A count rather than a flag: two queries in one read need two replies,
    /// or the second asker waits just as long as before.
    fn asked(&mut self, chunk: &[u8]) -> usize {
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(chunk);
        // The sequence cannot overlap itself, so plain windows counting is
        // exact rather than approximate.
        let asked = joined
            .windows(Self::QUERY.len())
            .filter(|window| *window == Self::QUERY)
            .count();
        let keep = joined.len().saturating_sub(Self::QUERY.len() - 1);
        self.tail = joined[keep..].to_vec();
        asked
    }

    /// The reply to one query: where the cursor is, counted from one.
    fn reply(row: u16, col: u16) -> String {
        format!("\x1b[{};{}R", row + 1, col + 1)
    }
}

/// One shell on one pty.
pub struct PtySession {
    /// What to call it in the tab strip.
    pub title: String,
    /// The shell binary this session is running.
    pub program: String,
    pub cwd: PathBuf,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Shared with the reader thread, which has to write back: a terminal that
    /// never answers a query leaves whoever asked waiting forever. See
    /// [`Answering`].
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    /// Behind a lock only so that [`Self::finished`] can ask it from `&self`.
    child: Mutex<Box<dyn Child + Send + Sync>>,
    /// Set by the reader thread when the pty reaches end of file, and by
    /// [`Self::finished`] when it finds the shell gone without one.
    finished: Arc<AtomicBool>,
    /// A recording, if one is running. Shared with the reader thread, because
    /// that is the only place every byte passes through.
    recorder: Arc<Mutex<Option<crate::record::Recorder>>>,
    /// The startup files that taught this shell to report what it runs, kept
    /// alive because dropping them deletes them.
    hook: Option<crate::shellhook::Installed>,
    /// Commands this session has finished, waiting to be collected.
    commands: Arc<Mutex<Vec<crate::shellhook::Ran>>>,
    /// Where the shell itself says it is, which is not always where the panel
    /// is looking once someone has typed `cd`.
    shell_cwd: Arc<Mutex<Option<PathBuf>>>,
    rows: u16,
    cols: u16,
}

impl PtySession {
    /// Start `program` on a fresh pty, in `cwd`.
    pub fn spawn(program: &str, cwd: &Path, rows: u16, cols: u16) -> std::io::Result<PtySession> {
        PtySession::spawn_with_scrollback(program, cwd, rows, cols, SCROLLBACK)
    }

    /// As [`Self::spawn`], with the history depth chosen.
    ///
    /// Only tests pass anything but [`SCROLLBACK`], and they need it: proving
    /// a recording outlives the scrollback means having a scrollback small
    /// enough to overrun without printing five thousand lines first.
    pub fn spawn_with_scrollback(
        program: &str,
        cwd: &Path,
        rows: u16,
        cols: u16,
        scrollback: usize,
    ) -> std::io::Result<PtySession> {
        let rows = rows.max(1);
        let cols = cols.max(1);

        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;

        let interactive = interactive_args(program, crate::mount::Platform::current());
        // Teaching the shell to report what it runs is best-effort in every
        // sense: a shell with no seam to hook, or a temporary directory that
        // cannot be written, leaves the session exactly as it was before.
        let login = interactive.iter().any(|a| a == "-l" || a == "--login");
        let hook = crate::shellhook::install(program, login).ok().flatten();
        let arguments = match &hook {
            Some(hook) => arguments_for(&interactive, &hook.args, &hook.without),
            None => interactive,
        };

        let mut command = CommandBuilder::new(program);
        for argument in arguments {
            command.arg(argument);
        }
        command.cwd(cwd);
        // Tell the shell what it is talking to, so it emits sequences vt100
        // understands rather than probing for something exotic.
        command.env("TERM", "xterm-256color");
        if let Some(hook) = &hook {
            for (name, value) in &hook.env {
                command.env(name, value);
            }
        }

        let child = pty
            .slave
            .spawn_command(command)
            .map_err(std::io::Error::other)?;
        // Dropping the slave lets the reader see EOF when the shell exits.
        drop(pty.slave);

        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pty.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, scrollback)));
        let finished = Arc::new(AtomicBool::new(false));

        let recorder: Arc<Mutex<Option<crate::record::Recorder>>> = Arc::new(Mutex::new(None));

        let commands: Arc<Mutex<Vec<crate::shellhook::Ran>>> = Arc::new(Mutex::new(Vec::new()));
        let shell_cwd: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

        let worker_parser = Arc::clone(&parser);
        let worker_finished = Arc::clone(&finished);
        let worker_recorder = Arc::clone(&recorder);
        let worker_commands = Arc::clone(&commands);
        let worker_cwd = Arc::clone(&shell_cwd);
        let worker_writer = Arc::clone(&writer);
        let mut answering = Answering::new();
        let mut reading_marks = hook
            .as_ref()
            .map(|hook| crate::shellhook::Marks::new(&hook.nonce));
        let mut pairing = crate::shellhook::Pairing::new();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let chunk = &buffer[..read];
                        // The marks are read here for the same reason the
                        // recording is: this is the only place every byte
                        // passes through. They are not removed from the
                        // stream - an emulator drops an operating-system
                        // command it does not recognise, and the transcriber
                        // does too, so nothing downstream sees them.
                        if let Some(marks) = reading_marks.as_mut() {
                            for mark in marks.feed(chunk) {
                                if let Some(ran) = pairing.take(mark) {
                                    lock(&worker_commands).push(ran);
                                }
                            }
                            if let Some(where_it_is) = pairing.cwd() {
                                let mut held = lock(&worker_cwd);
                                if held.as_deref() != Some(where_it_is) {
                                    *held = Some(where_it_is.to_path_buf());
                                }
                            }
                        }
                        // The recording is fed first, before the emulator has
                        // had a chance to lose anything: the screen holds a
                        // bounded scrollback and resolves `clear` and an
                        // editor's alternate screen away, but every byte the
                        // shell and its children write passes through here.
                        //
                        // Before, and not after, so that anything visible on
                        // the screen is already in the file. The other way
                        // round leaves a window where output has been drawn
                        // but not recorded, and stopping the recording in
                        // that window loses it - which is exactly how the
                        // test for this came to be flaky.
                        if let Some(recorder) = lock(&worker_recorder).as_mut() {
                            recorder.write(chunk);
                        }
                        lock(&worker_parser).process(chunk);

                        // After processing, never before: the answer has to
                        // describe the screen as it stands once everything in
                        // this chunk has been drawn, which is what the asker
                        // is waiting to hear about.
                        let asked = answering.asked(chunk);
                        if asked > 0 {
                            let (row, col) = {
                                let parser = lock(&worker_parser);
                                parser.screen().cursor_position()
                            };
                            let reply = Answering::reply(row, col);
                            let mut writer = lock(&worker_writer);
                            for _ in 0..asked {
                                let _ = writer.write_all(reply.as_bytes());
                            }
                            let _ = writer.flush();
                        }
                    }
                }
            }
            worker_finished.store(true, Ordering::Relaxed);
        });

        Ok(PtySession {
            title: crate::shell::program_name(program),
            program: program.to_string(),
            cwd: cwd.to_path_buf(),
            parser,
            recorder,
            hook,
            commands,
            shell_cwd,
            writer,
            master: pty.master,
            child: Mutex::new(child),
            finished,
            rows,
            cols,
        })
    }

    /// Send raw bytes to the shell - this is what typing does.
    ///
    /// Anything sent snaps the view back to the live screen. Every terminal
    /// does this, and for a good reason: typing while scrolled up would
    /// otherwise echo somewhere the user cannot see.
    pub fn write(&mut self, bytes: &[u8]) {
        {
            let mut writer = lock(&self.writer);
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
        self.scroll_to_bottom();
    }

    /// How far back through the scrollback the view is, in lines. `0` is the
    /// live screen, where the prompt is.
    pub fn scrollback_offset(&self) -> usize {
        self.with_screen(|screen| screen.scrollback())
    }

    /// Move the view to an absolute offset, and report where it landed.
    ///
    /// The emulator clamps to however much history it actually holds, so
    /// [`usize::MAX`] means "as far back as this session goes".
    pub fn scroll_to(&mut self, offset: usize) -> usize {
        let mut guard = lock(&self.parser);
        let screen = guard.screen_mut();
        screen.set_scrollback(offset);
        screen.scrollback()
    }

    /// Move the view by whole lines; positive goes back into history.
    pub fn scroll_by(&mut self, lines: i64) -> usize {
        let current = self.scrollback_offset() as i64;
        self.scroll_to(current.saturating_add(lines).max(0) as usize)
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_to(0);
    }

    // ---- what was run -------------------------------------------------------

    /// Whether this session can report the commands run in it.
    ///
    /// False for `sh`, `dash` and the rest of the POSIX family, which have no
    /// seam to hook - and the account says so rather than leaving a silence
    /// that reads as "nothing was run".
    pub fn journals(&self) -> bool {
        self.hook.is_some()
    }

    /// The commands that have finished since this was last asked.
    ///
    /// Draining rather than accumulating, because the caller writes each one
    /// down and a session left open for a day should not hold a day of them.
    pub fn take_commands(&self) -> Vec<crate::shellhook::Ran> {
        std::mem::take(&mut *lock(&self.commands))
    }

    /// Where the shell says it is standing.
    ///
    /// `None` until it has said - an unhooked shell never does, and a hooked
    /// one first says so at its first prompt.
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        lock(&self.shell_cwd).clone()
    }

    // ---- recording ----------------------------------------------------------

    /// Start writing everything this session prints to `path`.
    ///
    /// Unlike [`Self::transcript`], which can only offer what is still in the
    /// scrollback, this keeps going for as long as it is left on - the file
    /// is not bounded by anything but the disk.
    ///
    /// Fails rather than overwriting, and refuses to start a second recording
    /// over a running one.
    pub fn start_recording(&mut self, path: &Path) -> std::io::Result<()> {
        let mut slot = lock(&self.recorder);
        if slot.is_some() {
            return Err(std::io::Error::other("already recording"));
        }
        *slot = Some(crate::record::Recorder::create(path)?);
        Ok(())
    }

    /// Stop, and report the file and how much plain text went into it.
    pub fn stop_recording(&mut self) -> Option<(PathBuf, u64)> {
        let recorder = lock(&self.recorder).take()?;
        let lines = recorder.lines();
        Some((recorder.finish(), lines))
    }

    /// The file being recorded to, if any.
    pub fn recording(&self) -> Option<PathBuf> {
        lock(&self.recorder)
            .as_ref()
            .map(|recorder| recorder.path().to_path_buf())
    }

    /// How many lines the running recording has written.
    pub fn recorded_lines(&self) -> u64 {
        lock(&self.recorder)
            .as_ref()
            .map_or(0, |recorder| recorder.lines())
    }

    /// Everything this session still holds, as plain text: the scrollback
    /// followed by the live screen, with the escape sequences resolved.
    ///
    /// It is read the only way the emulator offers - a window at a time,
    /// walked from the oldest line down - so the view is moved and put back.
    /// That is also why it is `&mut`. What comes out is what the user could
    /// have scrolled through and read, not the raw byte stream, so a progress
    /// bar that rewrote its own line appears once, in its final state.
    pub fn transcript(&mut self) -> String {
        let restore = self.scrollback_offset();
        let rows = self.rows as usize;
        let cols = self.cols;
        let max = self.scroll_to(usize::MAX);

        // Scrollback lines, then the live screen: `max + rows` in all.
        let mut lines: Vec<String> = vec![String::new(); max + rows];
        let mut start = 0usize;
        loop {
            let offset = max.saturating_sub(start);
            self.scroll_to(offset);
            // Where this window sits in the document. Not `start`: the last
            // step lands on offset 0, which shows the live screen at `max`
            // however far past it `start` has walked.
            let base = max - offset;
            let window: Vec<String> = self.with_screen(|screen| screen.rows(0, cols).collect());
            for (index, line) in window.into_iter().enumerate() {
                // Windows overlap when the history is not a whole number of
                // screens; writing by index makes that harmless.
                if let Some(slot) = lines.get_mut(base + index) {
                    *slot = line;
                }
            }
            if offset == 0 {
                break;
            }
            start += rows;
        }

        self.scroll_to(restore);

        // The live screen is mostly blank below the prompt.
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }

    pub fn write_str(&mut self, text: &str) {
        self.write(text.as_bytes());
    }

    /// Type a line and press return.
    pub fn run_line(&mut self, line: &str) {
        self.write_str(line);
        self.write(b"\r");
    }

    /// Whether a full-screen program is running in here.
    ///
    /// The alternate screen is what `vim`, `less`, `top` and `git rebase -i`
    /// switch to, and it is the one honest signal a pty offers that a tab is
    /// busy with something other than a prompt. A command typed at a tab in
    /// this state is not run - it reaches the editor as keystrokes, which is
    /// how `sudo -i` ends up as a search in someone's vim.
    pub fn is_busy(&self) -> bool {
        self.with_screen(|screen| screen.alternate_screen())
    }

    /// Tell both the pty and the emulator about a new window size.
    ///
    /// Both halves matter: the pty size is what makes the shell re-wrap and
    /// what `SIGWINCH` reports, and the emulator has to agree or the grid and
    /// the shell's idea of the screen drift apart.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        lock(&self.parser).screen_mut().set_size(rows, cols);
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// True once the shell has exited.
    ///
    /// The reader reaching end of file is the cheap answer and on Unix it is
    /// the whole answer, because the last close of the slave ends the stream.
    /// Windows does not oblige: the pseudoconsole belongs to the master and
    /// stays open after the shell is gone, so the read never returns zero and
    /// a tab whose shell had exited would sit there forever, never reaped.
    /// So ask the process itself as well.
    pub fn finished(&self) -> bool {
        if self.finished.load(Ordering::Relaxed) {
            return true;
        }
        let exited = matches!(lock(&self.child).try_wait(), Ok(Some(_)));
        if exited {
            // Cache it: try_wait on an already-reaped child need not keep
            // saying yes, and the reader may still be draining what is left.
            self.finished.store(true, Ordering::Relaxed);
        }
        exited
    }

    /// Read the emulated screen. The closure keeps the lock scoped.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let guard = lock(&self.parser);
        f(guard.screen())
    }

    /// The visible text, used by tests and for "copy all".
    pub fn visible_text(&self) -> String {
        self.with_screen(|screen| screen.contents())
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.with_screen(|screen| screen.cursor_position())
    }

    /// Ask the shell to stop, then make sure the process is gone.
    ///
    /// A recording is closed properly on the way out, so the last line is not
    /// the one thing missing from it.
    pub fn shutdown(&mut self) {
        self.stop_recording();
        {
            let mut writer = lock(&self.writer);
            let _ = writer.write_all(b"\x04"); // EOF, the polite way out
            let _ = writer.flush();
        }
        let _ = lock(&self.child).kill();
        let _ = lock(&self.child).wait();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // A shell left running would keep a pty and a process around after its
        // tab is gone.
        self.stop_recording();
        let _ = lock(&self.child).kill();
    }
}

/// The set of open terminals, and which one is on screen.
#[derive(Default)]
pub struct Terminals {
    pub sessions: Vec<PtySession>,
    pub active: usize,
    /// Which sessions are pinned, by index alongside `sessions`.
    ///
    /// A pinned shell is left where it is: it does not follow the panes and
    /// the panes do not follow it. That is what you want for a build running
    /// in one directory while you work in another - without it, coupling the
    /// two means a shell you cannot keep still.
    ///
    /// Kept beside the sessions rather than inside them because it is a
    /// front-end's policy about a session, not a fact about the process, and
    /// [`Terminals::close`] keeps the two in step.
    pub pinned: Vec<bool>,
}

impl Terminals {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn active(&self) -> Option<&PtySession> {
        self.sessions.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut PtySession> {
        self.sessions.get_mut(self.active)
    }

    /// Open a new terminal and switch to it.
    pub fn open(&mut self, program: &str, cwd: &Path, rows: u16, cols: u16) -> std::io::Result<()> {
        let mut session = PtySession::spawn(program, cwd, rows, cols)?;
        // Number the tabs when several run the same shell, as an editor does.
        let same = self
            .sessions
            .iter()
            .filter(|s| s.program == session.program)
            .count();
        if same > 0 {
            session.title = format!("{} ({})", session.title, same + 1);
        }
        self.sessions.push(session);
        self.pinned.push(false);
        self.active = self.sessions.len() - 1;
        Ok(())
    }

    /// Whether the session at `index` is left where it is.
    pub fn is_pinned(&self, index: usize) -> bool {
        self.pinned.get(index).copied().unwrap_or(false)
    }

    /// Pin or unpin a session.
    pub fn set_pinned(&mut self, index: usize, pinned: bool) {
        // Grown rather than indexed blindly: a session opened before this
        // existed, or by a front-end that never pins, has no entry yet.
        while self.pinned.len() < self.sessions.len() {
            self.pinned.push(false);
        }
        if let Some(slot) = self.pinned.get_mut(index) {
            *slot = pinned;
        }
    }

    /// Close one terminal, keeping the selection somewhere sensible.
    pub fn close(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        let mut session = self.sessions.remove(index);
        session.shutdown();
        // In step with the sessions, or every tab after this one would
        // inherit the pin of its neighbour.
        if index < self.pinned.len() {
            self.pinned.remove(index);
        }

        if self.sessions.is_empty() {
            self.active = 0;
        } else if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active = index;
        }
    }

    /// Drop tabs whose shell has exited.
    pub fn reap_finished(&mut self) -> usize {
        let before = self.sessions.len();
        let finished: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.finished())
            .map(|(i, _)| i)
            .collect();
        for index in finished.into_iter().rev() {
            self.close(index);
        }
        before - self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Check a directory the shell reported against the one we know it is in.
    ///
    /// The same directory, but not always the same spelling. A Unix-flavoured
    /// bash on Windows - Git Bash, MSYS, the one in a WSL distribution -
    /// answers in its own namespace, calling
    /// `C:\Users\me\AppData\Local\Temp\x` by the name `/tmp/x`. Neither
    /// spelling converts to the other without asking that shell, and nothing
    /// in the engine asks, so there the last component is all that can
    /// honestly be compared. Where the shell and the OS share a namespace the
    /// whole path is compared, because there it means something.
    ///
    /// Worth knowing before anything acts on `shell_cwd()`: a pane made to
    /// follow an interactive `cd` would be handed `/tmp/x` on Windows and
    /// find nothing there.
    fn reported_as(reported: Option<&Path>, expected: &Path) -> bool {
        let Some(reported) = reported else {
            return false;
        };
        if cfg!(windows) {
            reported.file_name().is_some() && reported.file_name() == expected.file_name()
        } else {
            reported == expected
        }
    }

    /// Wait until `count` commands have been reported, then hand them over.
    fn wait_for_commands(
        session: &PtySession,
        count: usize,
        seconds: u64,
    ) -> Vec<crate::shellhook::Ran> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut collected = Vec::new();
        while Instant::now() < deadline {
            collected.extend(session.take_commands());
            if collected.len() >= count {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        collected
    }

    /// Type at a shell the way a person does, and see what it says it ran.
    ///
    /// The keystrokes are deliberately the ones that defeat reading the
    /// screen: a line edited before Enter, a line recalled from history, and
    /// a program printing something shaped exactly like a prompt.
    fn what_it_says_it_ran(program: &str, work: &Path) -> Vec<crate::shellhook::Ran> {
        let mut session = PtySession::spawn(program, work, 24, 100).expect("a shell");
        assert!(session.journals(), "{program} should have been hooked");
        // The first prompt means the startup files have finished.
        assert!(
            wait_for(&session, "$", 20) || wait_for(&session, "#", 20),
            "no prompt from {program}"
        );

        let mut type_it = |bytes: &[u8], pause: u64| {
            session.write(bytes);
            std::thread::sleep(Duration::from_millis(pause));
        };

        // 1. Plain.
        type_it(b"echo alpha", 250);
        type_it(b"\r", 500);
        // 2. Edited mid-line: four lefts, then an insertion. Runs
        //    `echo the beta`, and looks like `echo the` on screen.
        type_it(b"echo beta", 250);
        type_it(b"\x1b[D\x1b[D\x1b[D\x1b[D", 250);
        type_it(b"the ", 250);
        type_it(b"\r", 500);
        // 3. Recalled from history, which repaints the line.
        type_it(b"\x1b[A", 350);
        type_it(b"\r", 500);
        // 4. A program printing a prompt and a command that never ran.
        type_it(b"printf 'root@vm:/tmp# rm -rf /\\n'", 250);
        type_it(b"\r", 500);
        // 5. One that fails, for the status.
        type_it(b"false", 250);
        type_it(b"\r", 500);

        let ran = wait_for_commands(&session, 5, 20);
        session.shutdown();
        ran
    }

    /// Wait for the screen to contain `needle`, so tests do not sleep blindly.
    fn wait_for(session: &PtySession, needle: &str, seconds: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if session.visible_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn the_hooks_own_options_come_first() {
        // bash refuses a long option that follows a short one, which is a
        // silent way to end up with a shell that only prints its usage.
        let interactive: Vec<String> = ["-l", "-i"].iter().map(|s| s.to_string()).collect();
        let hook: Vec<String> = ["--rcfile", "/tmp/rc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let without = vec!["-l".to_string(), "--login".to_string()];
        assert_eq!(
            arguments_for(&interactive, &hook, &without),
            vec!["--rcfile", "/tmp/rc", "-i"]
        );
        // With nothing to remove, the interactive flags survive intact.
        assert_eq!(
            arguments_for(&interactive, &[], &[]),
            vec!["-l".to_string(), "-i".to_string()]
        );
    }

    #[test]
    fn bash_reports_what_it_ran_including_the_lines_the_screen_gets_wrong() {
        let Some(bash) = plain::found(&["bash"]) else {
            eprintln!("no bash on this machine - skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ran = what_it_says_it_ran(&bash, dir.path());
        let lines: Vec<&str> = ran.iter().map(|r| r.line.as_str()).collect();

        assert_eq!(
            lines,
            vec![
                "echo alpha",
                // Not `echo the`, which is all the screen ever showed.
                "echo the beta",
                "echo the beta",
                r"printf 'root@vm:/tmp# rm -rf /\n'",
                "false",
            ]
        );
        // And nothing was invented from the output that looked like a prompt.
        assert!(!lines
            .iter()
            .any(|line| line.contains("rm -rf /'") || *line == "rm -rf /"));

        assert!(ran[..4].iter().all(|r| !r.failed()));
        assert_eq!(ran[4].code, 1, "the status the stream never carried");
        let here = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            reported_as(ran[0].cwd.as_deref(), &here),
            "reported {:?}, expected {:?}",
            ran[0].cwd,
            here
        );
    }

    #[test]
    fn zsh_reports_what_it_ran() {
        let Some(zsh) = plain::found(&["zsh"]) else {
            eprintln!("no zsh on this machine - skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ran = what_it_says_it_ran(&zsh, dir.path());
        let lines: Vec<&str> = ran.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            vec![
                "echo alpha",
                "echo the beta",
                "echo the beta",
                r"printf 'root@vm:/tmp# rm -rf /\n'",
                "false",
            ]
        );
        assert_eq!(ran[4].code, 1);
    }

    #[test]
    fn fish_reports_what_it_ran() {
        let Some(fish) = plain::found(&["fish"]) else {
            eprintln!("no fish on this machine - skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ran = what_it_says_it_ran(&fish, dir.path());
        let lines: Vec<&str> = ran.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            vec![
                "echo alpha",
                "echo the beta",
                "echo the beta",
                r"printf 'root@vm:/tmp# rm -rf /\n'",
                "false",
            ]
        );
        assert_eq!(ran[4].code, 1);
    }

    #[test]
    fn a_shell_with_nothing_to_hook_runs_exactly_as_before() {
        let Some(dash) = plain::found(&["dash"]) else {
            eprintln!("no dash on this machine - skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(&dash, dir.path(), 24, 80).unwrap();
        assert!(!session.journals(), "there is no seam in dash");

        // Still a working shell, which is the point of not refusing to run it.
        session.run_line("echo still-works");
        assert!(wait_for(&session, "still-works", 15));
        assert!(session.take_commands().is_empty());
        assert_eq!(session.shell_cwd(), None);
        session.shutdown();
    }

    #[test]
    fn the_shell_says_where_it_went() {
        // `cd` inside the shell is invisible to a file manager that only
        // knows where it started the shell. Reported by the same hook as the
        // commands are, so this needs one of the shells that has a seam.
        let Some(bash) = plain::found(&["bash"]) else {
            eprintln!("no bash on this machine - skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let real = std::fs::canonicalize(&inner).unwrap();

        let mut session = PtySession::spawn(&bash, dir.path(), 24, 80).unwrap();
        assert!(wait_for(&session, "$", 20) || wait_for(&session, "#", 20));
        session.run_line("cd inner");
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline && !reported_as(session.shell_cwd().as_deref(), &real) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            reported_as(session.shell_cwd().as_deref(), &real),
            "reported {:?}, expected {:?}",
            session.shell_cwd(),
            real
        );
        session.shutdown();
    }

    #[test]
    fn the_users_own_startup_still_runs() {
        // The hook is added on top of the shell's own files, never instead of
        // them: an alias defined in .bashrc has to survive, or the terminal
        // panel is not the user's shell any more.
        let Some(bash) = plain::found(&["bash"]) else {
            eprintln!("no bash on this machine - skipped");
            return;
        };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bashrc"),
            "export RCMD_TEST_MARKER=from-the-users-bashrc\n",
        )
        .unwrap();

        let previous = std::env::var("HOME").ok();
        // SAFETY: single-threaded at this point in the test, and put back
        // before anything else looks.
        unsafe { std::env::set_var("HOME", home.path()) };
        let mut session = PtySession::spawn(&bash, home.path(), 24, 80).unwrap();
        assert!(wait_for(&session, "$", 20) || wait_for(&session, "#", 20));
        session.run_line("echo \"marker is $RCMD_TEST_MARKER\"");
        let found = wait_for(&session, "marker is from-the-users-bashrc", 15);
        session.shutdown();
        match previous {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert!(found, "the user's own .bashrc was not sourced");
    }

    #[test]
    fn posix_shells_are_started_interactive_and_others_are_left_alone() {
        use crate::mount::Platform;

        // Without -i a POSIX shell on a pty is still non-interactive: no
        // prompt, no rc file, none of the user's aliases or PATH.
        assert_eq!(interactive_args("/bin/bash", Platform::Linux), ["-i"]);
        assert_eq!(interactive_args("/bin/zsh", Platform::Linux), ["-i"]);

        // macOS assembles PATH in login-only files.
        assert_eq!(interactive_args("/bin/zsh", Platform::MacOs), ["-l", "-i"]);

        // fish and nu are interactive by default and reject these flags.
        assert!(interactive_args("/usr/bin/fish", Platform::Linux).is_empty());
        assert!(interactive_args("/usr/bin/nu", Platform::Linux).is_empty());
        assert!(interactive_args("powershell.exe", Platform::Windows).is_empty());
        assert!(interactive_args(r"C:\Windows\System32\cmd.exe", Platform::Windows).is_empty());
    }

    #[test]
    fn a_shell_started_here_shows_a_prompt() {
        // The visible proof that it is interactive.
        let dir = tempfile::tempdir().unwrap();
        let session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while Instant::now() < deadline && !seen {
            seen = !session.visible_text().trim().is_empty();
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(seen, "an interactive shell prints a prompt");
    }

    #[test]
    fn a_session_runs_a_real_shell_and_shows_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.run_line("echo hello-from-a-real-shell");
        assert!(
            wait_for(&session, "hello-from-a-real-shell", 10),
            "screen was:\n{}",
            session.visible_text()
        );
    }

    #[test]
    fn state_persists_between_commands() {
        // This is the whole point of a session: a fresh `sh -c` per command
        // could not do this.
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        // The pty buffers input, so the shell reads these in order.
        let [set, read_back] = plain::set_then_echo("MYVAR", "persisted", "value-is-");
        session.run_line(&set);
        session.run_line(&read_back);

        assert!(
            wait_for(&session, "value-is-persisted", 10),
            "screen was:\n{}",
            session.visible_text()
        );
    }

    #[test]
    fn cd_persists_too_because_the_shell_owns_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("inner")).unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.run_line("cd inner");
        session.run_line(plain::print_cwd());

        assert!(
            wait_for(&session, "inner", 10),
            "screen was:\n{}",
            session.visible_text()
        );
    }

    #[test]
    fn the_shell_starts_in_the_directory_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker-file.txt"), "x").unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.run_line(plain::list());
        assert!(
            wait_for(&session, "marker-file.txt", 10),
            "screen was:\n{}",
            session.visible_text()
        );
    }

    #[test]
    fn resizing_updates_the_emulator_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();
        assert_eq!(session.size(), (24, 80));

        session.resize(40, 120);
        assert_eq!(session.size(), (40, 120));
        assert_eq!(session.with_screen(|s| s.size()), (40, 120));

        // A repeat of the same size is a no-op rather than a needless SIGWINCH.
        session.resize(40, 120);
        assert_eq!(session.size(), (40, 120));

        // Degenerate sizes are clamped rather than passed on.
        session.resize(0, 0);
        assert_eq!(session.size(), (1, 1));
    }

    #[test]
    fn output_that_scrolls_off_the_screen_is_still_reachable() {
        // The point of the scrollback: a long build's early output must not be
        // gone just because it left a ten-row panel.
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();

        let (command, lines) = plain::numbered("line", 60);
        let (first, last) = (&lines[0], lines.last().unwrap());
        session.run_line(&command);
        assert!(
            wait_for(&session, last, 15),
            "screen was:\n{}",
            session.visible_text()
        );
        assert!(
            !session.visible_text().contains(first.as_str()),
            "the first line should have left a ten-row screen"
        );

        let reached = session.scroll_to(usize::MAX);
        assert!(reached > 0, "there should be history to scroll into");
        assert!(
            session.visible_text().contains(first.as_str()),
            "scrolled to the top, screen was:\n{}",
            session.visible_text()
        );

        session.scroll_to_bottom();
        assert_eq!(session.scrollback_offset(), 0);
        assert!(session.visible_text().contains(last.as_str()));
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();
        let (command, rows) = plain::numbered("row", 40);
        session.run_line(&command);
        assert!(wait_for(&session, rows.last().unwrap(), 15));
        // Settled first: the prompt after the last row is one more line of
        // scrollback, and it must land before anything measures how far back
        // the view can go.
        settle_screen(
            &session,
            Duration::from_millis(300),
            Duration::from_secs(10),
        );

        // Past the oldest line is the oldest line, not an error.
        let top = session.scroll_to(usize::MAX);
        assert_eq!(session.scroll_by(500), top);
        // And past the newest is the live screen.
        assert_eq!(session.scroll_by(-5_000), 0);
    }

    #[test]
    fn typing_snaps_the_view_back_to_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();
        let (command, rows) = plain::numbered("row", 40);
        session.run_line(&command);
        assert!(wait_for(&session, rows.last().unwrap(), 15));

        assert!(session.scroll_to(usize::MAX) > 0);
        // Anything typed brings the prompt back into view, or the echo would
        // land somewhere off screen.
        session.write_str("echo back");
        assert_eq!(session.scrollback_offset(), 0);
    }

    #[test]
    fn the_transcript_holds_every_line_once_and_in_order() {
        // A ten-row screen and sixty lines: the walk has to stitch six windows
        // together without dropping or repeating a row.
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();
        let (command, lines) = plain::numbered("line", 60);
        session.run_line(&command);
        assert!(wait_for(&session, lines.last().unwrap(), 15));

        let text = session.transcript();
        for needle in &lines {
            assert_eq!(
                text.matches(needle.as_str()).count(),
                1,
                "{needle} should appear exactly once in:\n{text}"
            );
        }

        // In order, and after the command that produced them.
        let first = text.find(lines[0].as_str()).unwrap();
        let last = text.find(lines.last().unwrap().as_str()).unwrap();
        assert!(first < last);

        // The command's own echo comes before anything it printed. Searched
        // with the line breaks taken out, because cmd's prompt is the whole
        // working directory: a temporary path plus this command is more than
        // eighty columns, so the echo really is wrapped, and the transcript is
        // right to keep the break the screen had.
        let unwrapped: String = text.chars().filter(|c| !"\r\n".contains(*c)).collect();
        let echoed = unwrapped
            .find(&command)
            .unwrap_or_else(|| panic!("the command was never echoed, transcript:\n{text}"));
        assert!(echoed < unwrapped.find(lines[0].as_str()).unwrap());
    }

    #[test]
    fn taking_the_transcript_leaves_the_view_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();
        let (command, rows) = plain::numbered("row", 40);
        session.run_line(&command);
        assert!(wait_for(&session, rows.last().unwrap(), 15));
        // Same reason as above: this compares an exact offset before and
        // after, and a prompt arriving in between would move it.
        settle_screen(
            &session,
            Duration::from_millis(300),
            Duration::from_secs(10),
        );

        // Scrolled up: reading the transcript must not yank the user's view.
        session.scroll_by(12);
        let before = session.scrollback_offset();
        assert!(before > 0);
        let _ = session.transcript();
        assert_eq!(session.scrollback_offset(), before);

        // And from the live screen it stays at the live screen.
        session.scroll_to_bottom();
        let _ = session.transcript();
        assert_eq!(session.scrollback_offset(), 0);
    }

    #[test]
    fn a_transcript_is_what_was_read_not_what_was_sent() {
        // A progress bar rewrites its own line with carriage returns. The user
        // saw one line, so the transcript holds one line - in its final state.
        //
        // The carriage returns are bytes in a file that is copied out
        // verbatim, not something the shell is asked to produce: the command
        // echoed at the prompt is then just `cat bar.txt`, which cannot
        // contain the stages and pass this by accident.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bar.txt"),
            "stage 10%\rstage 50%\rstage done\n",
        )
        .unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).unwrap();
        session.run_line(&plain::dump("bar.txt"));
        assert!(wait_for(&session, "stage done", 15));

        let text = session.transcript();
        assert_eq!(text.matches("stage").count(), 1, "got:\n{text}");
        assert!(text.contains("stage done"));
        assert!(!text.contains("stage 10"), "overwritten, got:\n{text}");
    }

    #[test]
    fn a_recording_keeps_what_the_scrollback_could_not() {
        // The point of recording rather than saving: a ten-row screen with a
        // scrollback of five holds fifteen lines at most, but the file gets
        // every one of the two hundred.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("session.log");
        let mut session =
            PtySession::spawn_with_scrollback(plain::program(), dir.path(), 10, 80, 5)
                .expect("a shell");

        session.start_recording(&log).unwrap();
        assert_eq!(session.recording().as_deref(), Some(log.as_path()));

        let (command, printed) = plain::numbered("l", 200);
        session.run_line(&command);
        assert!(wait_for(&session, printed.last().unwrap(), 20));

        let (path, lines) = session.stop_recording().expect("was recording");
        assert_eq!(path, log);
        assert!(lines >= 200, "wrote {lines} lines");
        assert!(session.recording().is_none());

        let text = std::fs::read_to_string(&log).unwrap();
        for index in [0, 56, 122, 199] {
            let needle = &printed[index];
            assert!(
                text.contains(needle.as_str()),
                "{needle} missing from the recording"
            );
        }
        // And the emulator really could not have supplied it.
        assert!(!session.transcript().contains(printed[0].as_str()));
    }

    #[test]
    fn a_full_screen_program_makes_the_tab_busy() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 10, 80).expect("a shell");

        // A prompt is free.
        session.run_line("echo ready");
        assert!(wait_for(&session, "ready", 10));
        assert!(!session.is_busy());

        // The escape that switches to the alternate screen is what vim, less
        // and top all send. Copying it out of a file keeps the test off any
        // particular editor being installed - and off typing an escape at a
        // prompt, which the Windows line editor would eat as "clear the line".
        std::fs::write(dir.path().join("alt.bin"), "\x1b[?1049h").unwrap();
        session.run_line(&plain::dump("alt.bin"));
        let busy = {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if session.is_busy() {
                    break true;
                }
                if Instant::now() > deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        };
        assert!(busy, "the alternate screen did not register");
    }

    /// Wait for a file to contain `needle`. Recording tests watch the file
    /// rather than the screen, since the file is what they are about.
    fn wait_for_file(path: &Path, needle: &str, seconds: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if std::fs::read_to_string(path)
                .map(|text| text.contains(needle))
                .unwrap_or(false)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// Wait until the screen stops changing, so the shell has finished talking.
    ///
    /// The same trap as [`settle_file`], one layer up. `wait_for` returns the
    /// instant a string appears, and a shell is not done when its last line of
    /// output lands - it still has a prompt to write, and that prompt pushes
    /// one more line into the scrollback. A test that measured how far back it
    /// could scroll in that gap got an answer that was one line stale by the
    /// time it compared it.
    fn settle_screen(session: &PtySession, quiet_for: Duration, give_up_after: Duration) {
        let deadline = Instant::now() + give_up_after;
        let mut last = String::new();
        let mut unchanged_since = Instant::now();
        while Instant::now() < deadline {
            let now = session.visible_text();
            if now != last {
                last = now;
                unchanged_since = Instant::now();
            } else if unchanged_since.elapsed() >= quiet_for {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until a recording stops growing, so the shell has finished talking.
    ///
    /// [`wait_for_file`] returns the instant a string appears, which is sooner
    /// than the shell being done - and on a shell that echoes what you typed,
    /// the first appearance is the echo of the command rather than its output.
    /// A test that switched recordings at that moment closed the first file
    /// before the command's own output arrived, and that output then landed in
    /// the second one. Which is a race the test lost about one run in ten,
    /// under load, on Windows only - because `/bin/sh` does not echo and the
    /// string therefore appears exactly once there.
    fn settle_file(path: &Path, quiet_for: Duration, give_up_after: Duration) {
        let deadline = Instant::now() + give_up_after;
        let mut last = u64::MAX;
        let mut unchanged_since = Instant::now();
        while Instant::now() < deadline {
            let now = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if now != last {
                last = now;
                unchanged_since = Instant::now();
            } else if unchanged_since.elapsed() >= quiet_for {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn recording_stops_and_starts_and_will_not_double_up() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.start_recording(&first).unwrap();
        // A second recording over a running one would silently lose the first.
        assert!(session.start_recording(&second).is_err());

        session.run_line("echo in-the-first");
        assert!(wait_for_file(&first, "in-the-first", 15));
        // Settled before switching, or the rest of what this command has to say
        // - its output, and the prompt after it - arrives once the next
        // recording is already open, and lands in that file instead.
        settle_file(&first, Duration::from_millis(400), Duration::from_secs(10));
        session.stop_recording();

        // A new recording is a new file; nothing appends to the old one.
        session.start_recording(&second).unwrap();
        session.run_line("echo in-the-second");
        assert!(wait_for_file(&second, "in-the-second", 15));
        settle_file(&second, Duration::from_millis(400), Duration::from_secs(10));
        session.stop_recording();

        let one = std::fs::read_to_string(&first).unwrap();
        let two = std::fs::read_to_string(&second).unwrap();
        assert!(one.contains("in-the-first"));
        assert!(!one.contains("in-the-second"), "the first file was closed");
        assert!(two.contains("in-the-second"));
        assert!(!two.contains("in-the-first"), "the second file is fresh");
    }

    #[test]
    fn a_recording_catches_a_child_that_is_not_the_shell() {
        // "whatever else prints there": a background process writing to the
        // same terminal is in the file too, because the tee is at the pty.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("child.log");
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.start_recording(&log).unwrap();
        session.run_line(&plain::background_echo("from-a-background-child"));
        assert!(wait_for_file(&log, "from-a-background-child", 15));
        session.stop_recording();

        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("from-a-background-child"), "got:\n{text}");
    }

    #[test]
    fn shutting_down_closes_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("cut-short.log");
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();

        session.start_recording(&log).unwrap();
        session.run_line("echo before-the-end");
        assert!(wait_for_file(&log, "before-the-end", 15));

        session.shutdown();
        // Buffered writes are flushed rather than lost with the process.
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("before-the-end"), "got:\n{text}");
    }

    #[test]
    fn a_saved_transcript_is_named_after_its_shell() {
        assert_eq!(
            transcript_name("bash", "20260725-115233"),
            "bash-20260725-115233.log"
        );
        // Repeat tabs carry a "(2)", which is not a file name.
        assert_eq!(
            transcript_name("bash (2)", "20260725-115233"),
            "bash-2-20260725-115233.log"
        );
        assert_eq!(
            transcript_name("powershell.exe", "20260725-115233"),
            "powershell-exe-20260725-115233.log"
        );
        // Nothing usable in the title still gives a usable name.
        assert_eq!(transcript_name("///", "stamp"), "shell-stamp.log");
    }

    #[test]
    fn a_session_notices_when_its_shell_exits() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = PtySession::spawn(plain::program(), dir.path(), 24, 80).unwrap();
        assert!(!session.finished());

        session.run_line("exit");

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !session.finished() {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(session.finished(), "the tab should be reapable");
    }

    #[test]
    fn opening_several_terminals_numbers_the_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let mut terminals = Terminals::default();

        terminals
            .open(plain::program(), dir.path(), 24, 80)
            .unwrap();
        terminals
            .open(plain::program(), dir.path(), 24, 80)
            .unwrap();
        terminals
            .open(plain::program(), dir.path(), 24, 80)
            .unwrap();

        let titles: Vec<String> = terminals.sessions.iter().map(|s| s.title.clone()).collect();
        let name = plain::title();
        assert_eq!(
            titles,
            [
                name.to_string(),
                format!("{name} (2)"),
                format!("{name} (3)")
            ]
        );
        // A new terminal takes focus, as it does in an editor.
        assert_eq!(terminals.active, 2);
    }

    #[test]
    fn closing_keeps_the_selection_somewhere_sensible() {
        let dir = tempfile::tempdir().unwrap();
        let mut terminals = Terminals::default();
        for _ in 0..3 {
            terminals
                .open(plain::program(), dir.path(), 24, 80)
                .unwrap();
        }

        // Closing the active last tab steps back.
        terminals.close(2);
        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals.active, 1);

        // Closing one *before* the active keeps the same tab selected.
        terminals.select(1);
        terminals.close(0);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals.active, 0);

        terminals.close(0);
        assert!(terminals.is_empty());
        assert_eq!(terminals.active, 0);
        // Closing something that is not there is harmless.
        terminals.close(5);
    }

    #[test]
    fn finished_sessions_are_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let mut terminals = Terminals::default();
        terminals
            .open(plain::program(), dir.path(), 24, 80)
            .unwrap();
        terminals
            .open(plain::program(), dir.path(), 24, 80)
            .unwrap();

        terminals.sessions[0].run_line("exit");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reaped = 0;
        while Instant::now() < deadline && reaped == 0 {
            reaped = terminals.reap_finished();
            std::thread::sleep(Duration::from_millis(25));
        }

        assert_eq!(reaped, 1);
        assert_eq!(terminals.len(), 1);
    }
}
