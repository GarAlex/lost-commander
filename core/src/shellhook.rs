// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Asking the shell what it ran, instead of guessing from what it printed.
//!
//! A terminal panel can see every byte a shell writes and still not know what
//! was run. What arrives is the shell's line editor *painting* - a prompt
//! nobody can parse because `PS1` is arbitrary, a command line rewritten in
//! place by history recall and cursor keys, and program output that is free to
//! look exactly like a prompt. Reconstructing "what did you run" from that
//! produces plausible answers that are sometimes wrong, which for a record of
//! what was done is worse than no answer at all.
//!
//! So this asks instead. The shell we start is given a small piece of its own
//! startup file, which prints an escape sequence when a command begins and
//! another when it ends, carrying the line and the exit status. The terminal
//! never shows them - an emulator drops operating-system commands it does not
//! recognise - and nothing else about the session changes.
//!
//! # The sequences
//!
//! ```text
//! ESC ] 133 ; C ; <nonce> ; <index> ; <line> BEL     a command line began
//! ESC ] 133 ; E ; <nonce> ; <word>           BEL     and this is what is running
//! ESC ] 133 ; D ; <nonce> ; <code>           BEL     and this is how it ended
//! ESC ] 7 ; file://<host><path>              BEL     where the shell is now
//! ```
//!
//! `133` is the semantic-prompt convention iTerm2 and VS Code use, so a shell
//! that already emits it is doing the same thing for the same reason. The
//! `nonce` is what tells the two apart: it is made fresh for each session and
//! never leaves this program, so marks from the user's own shell integration -
//! or from a program that prints an escape sequence on purpose - are ignored
//! rather than recorded as commands nobody ran.
//!
//! `index` and the `E` mark exist for bash alone, and only to settle one
//! question: whether a line that was not added to history is the same line run
//! again or a different one kept out of history on purpose. Both are ordinary
//! settings - `HISTCONTROL=ignoredups:ignorespace` is the default on most
//! distributions - and telling them apart is the difference between recording
//! a repeat and recording a line that never ran. [`Pairing`] does the
//! deciding; the shell only reports.
//!
//! # What it does not cover
//!
//! `sh`, `dash`, `ksh` and the rest of the POSIX family have no seam: no
//! preexec, no `PROMPT_COMMAND`, no `DEBUG` trap, and no prompt re-expansion
//! to smuggle one into. A session in one of those is simply not recorded, and
//! [`journals`] is how the rest of the program knows to say so.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A shell family, in terms of how its startup is hooked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// `DEBUG` trap for the line, `PROMPT_COMMAND` for the status.
    Bash,
    /// `preexec` and `precmd`, which is what they are for.
    Zsh,
    /// `fish_preexec` and `fish_postexec` events.
    Fish,
    /// The `prompt` function, which PowerShell calls before every prompt.
    ///
    /// Not a preexec: PowerShell has no hook that fires *before* a command,
    /// so the line and its result are both reported after the fact, from the
    /// history. That is enough for the account - what ran, where, how it
    /// ended and how long it took - and it is what `prompt` can honestly see.
    PowerShell,
    /// No seam to hook: `sh`, `dash`, `ksh`, `nu`, `cmd`.
    Unhooked,
}

/// Which family a shell binary belongs to, by the name it is invoked under.
///
/// The name and not the target: `sh` is usually a symlink to `dash` or `bash`,
/// but a shell inspects `argv[0]` and a `bash` running as `sh` is in POSIX
/// mode with no `PROMPT_COMMAND` worth having. `rbash` is left out for the
/// same reason - a restricted shell refuses much of what the hook does.
pub fn family(program: &str) -> Family {
    match crate::shell::program_name(program).as_str() {
        "bash" => Family::Bash,
        "zsh" => Family::Zsh,
        "fish" => Family::Fish,
        // Both of them: `powershell` is the one shipped with Windows and
        // `pwsh` the cross-platform one, and they hook identically.
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => Family::PowerShell,
        _ => Family::Unhooked,
    }
}

/// Whether commands run in this shell can be recorded.
pub fn journals(program: &str) -> bool {
    family(program) != Family::Unhooked
}

/// Why a shell cannot be recorded, in the words the account will use.
///
/// The shell's own name is left out because the line it goes on already
/// carries it, in the column that would otherwise say "Shell" and tell nobody
/// anything.
pub fn why_not() -> &'static str {
    "no way to report what it runs - commands in this session are not recorded"
}

/// One thing a mark said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// A command line began. The index is bash's history number, or 0 when
    /// the shell reports the line directly and no index is needed.
    Started { index: u64, line: String },
    /// The first word of what is actually about to run. bash only, and only
    /// useful when the history number did not move - see [`Pairing`].
    Executing(String),
    /// The command that began finished, with this status.
    ///
    /// The duration is the shell's own, when it knows one. A shell that
    /// reports only after the fact - PowerShell, whose seam is the prompt
    /// function and not a preexec - emits the start and the end together, so
    /// timing between the two marks would say every command took no time at
    /// all. When it is absent the pairing times the marks itself, which is
    /// right for every shell that reports as it goes.
    Finished { code: i32, ms: Option<u64> },
    /// The shell is now in this directory.
    Cwd(PathBuf),
}

/// A command that ran, from the moment it started to the moment it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub line: String,
    /// Where the shell was standing when it started - which is not always
    /// where the panel is looking, once someone has typed `cd`.
    pub cwd: Option<PathBuf>,
    pub code: i32,
    /// How long it took, in milliseconds - from the mark that says a command
    /// began to the one that says it ended.
    pub ms: u64,
}

impl Ran {
    pub fn failed(&self) -> bool {
        self.code != 0
    }
}

/// The longest mark worth reading.
///
/// A command line can be long, but nothing sane is longer than this, and a
/// program emitting a malformed sequence must not be able to make this grow
/// without limit while it waits for a terminator that never comes.
const MAX_BODY: usize = 16 * 1024;

/// Pulls marks out of a terminal byte stream.
///
/// Only operating-system commands are of interest, but the other string
/// sequences have to be tracked all the same: a `ESC ]` inside a device
/// control string is part of that string, not the start of a mark.
#[derive(Debug)]
pub struct Marks {
    nonce: String,
    state: State,
    body: Vec<u8>,
    /// Set once the body has outgrown [`MAX_BODY`], so the rest of it is
    /// dropped rather than collected.
    flooded: bool,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum State {
    #[default]
    Text,
    Escape,
    Osc,
    OscEscape,
    /// DCS, SOS, PM, APC: run until a string terminator, and are not ours.
    String,
    StringEscape,
}

impl Marks {
    pub fn new(nonce: impl Into<String>) -> Marks {
        Marks {
            nonce: nonce.into(),
            state: State::Text,
            body: Vec::new(),
            flooded: false,
        }
    }

    /// Feed a slice of the stream; get back whatever marks it completed.
    ///
    /// Reads land wherever the kernel puts them, so a sequence split across
    /// two of them has to survive - the state and the half-collected body
    /// live here between calls for exactly that reason.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Mark> {
        let mut out = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Text => {
                    if byte == 0x1b {
                        self.state = State::Escape;
                    }
                }
                State::Escape => {
                    self.state = match byte {
                        b']' => {
                            self.body.clear();
                            self.flooded = false;
                            State::Osc
                        }
                        b'P' | b'X' | b'^' | b'_' => State::String,
                        // A CSI ends at its final byte and cannot contain an
                        // escape, so there is nothing to track.
                        _ => State::Text,
                    };
                }
                State::Osc => match byte {
                    0x07 => {
                        self.finish(&mut out);
                        self.state = State::Text;
                    }
                    0x1b => self.state = State::OscEscape,
                    _ => self.collect(byte),
                },
                State::OscEscape => {
                    if byte == b'\\' {
                        self.finish(&mut out);
                        self.state = State::Text;
                    } else {
                        // Not the terminator after all - the escape was part
                        // of the string.
                        self.collect(0x1b);
                        self.collect(byte);
                        self.state = State::Osc;
                    }
                }
                State::String => {
                    if byte == 0x1b {
                        self.state = State::StringEscape;
                    }
                }
                State::StringEscape => {
                    self.state = if byte == b'\\' {
                        State::Text
                    } else {
                        State::String
                    };
                }
            }
        }
        out
    }

    fn collect(&mut self, byte: u8) {
        if self.flooded {
            return;
        }
        if self.body.len() >= MAX_BODY {
            self.flooded = true;
            self.body.clear();
            return;
        }
        self.body.push(byte);
    }

    fn finish(&mut self, out: &mut Vec<Mark>) {
        let body = std::mem::take(&mut self.body);
        let flooded = std::mem::replace(&mut self.flooded, false);
        if flooded {
            return;
        }
        if let Some(mark) = self.parse(&body) {
            out.push(mark);
        }
    }

    fn parse(&self, body: &[u8]) -> Option<Mark> {
        // Lossy on purpose: a command line is bytes, and a readable
        // approximation of an odd one beats discarding it.
        let body = String::from_utf8_lossy(body);

        if let Some(rest) = body.strip_prefix("7;") {
            return cwd_from_url(rest).map(Mark::Cwd);
        }

        let rest = body.strip_prefix("133;")?;
        let (kind, rest) = rest.split_once(';')?;
        match kind {
            "C" => {
                // nonce ; index ; line - and the line last, because it is the
                // one field that may contain a semicolon.
                let mut parts = rest.splitn(3, ';');
                let nonce = parts.next()?;
                if nonce != self.nonce {
                    return None;
                }
                let index = parts.next()?.parse::<u64>().ok()?;
                let line = parts.next().unwrap_or("").to_string();
                Some(Mark::Started { index, line })
            }
            "E" => {
                let (nonce, word) = rest.split_once(';')?;
                if nonce != self.nonce {
                    return None;
                }
                Some(Mark::Executing(word.trim().to_string()))
            }
            "D" => {
                // nonce ; code [ ; milliseconds ]
                let mut parts = rest.splitn(3, ';');
                let nonce = parts.next()?;
                if nonce != self.nonce {
                    return None;
                }
                let code = parts.next()?.trim().parse::<i32>().ok()?;
                let ms = parts.next().and_then(|ms| ms.trim().parse::<u64>().ok());
                Some(Mark::Finished { code, ms })
            }
            _ => None,
        }
    }
}

/// The path out of an `OSC 7` payload: `file://host/path`, percent-encoded.
fn cwd_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // The host runs to the first slash and is not interesting: a shell on
    // another machine is not one whose directories this program can list.
    let path = match rest.find('/') {
        Some(at) => &rest[at..],
        None => return None,
    };
    let decoded = percent_decode(path);
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(drive_letters_lose_the_slash(&decoded)))
}

/// `/C:/src` is `C:/src`.
///
/// A `file://` URI always begins its path with a slash, so a Windows path
/// arrives as `file:///C:/src` and the naive answer is `/C:/src` - which
/// looks like a path, is not one, and fails every test that matters by
/// silently not existing. PowerShell reports exactly this, which is how it
/// was found: the panel refused to follow the shell and said nothing,
/// because a directory that is not there is not a directory to move to.
///
/// Only where a drive letter and a colon actually follow the slash. A Unix
/// path is `/home/you` and must keep its leading slash.
fn drive_letters_lose_the_slash(path: &str) -> &str {
    let mut chars = path.chars();
    if chars.next() != Some('/') {
        return path;
    }
    let letter = chars.next().filter(|c| c.is_ascii_alphabetic());
    if letter.is_some() && chars.next() == Some(':') {
        return &path[1..];
    }
    path
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            let high = (bytes[at + 1] as char).to_digit(16);
            let low = (bytes[at + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// What is known about the command line currently in flight.
#[derive(Debug, Default, PartialEq, Eq)]
enum Pending {
    #[default]
    Nothing,
    /// The line is known and trusted.
    Known {
        line: String,
        cwd: Option<PathBuf>,
        started: Started,
    },
    /// A line was read back out of history that history did not just gain.
    /// It is either the same line run again or a different one that history
    /// was told to ignore, and until something says which, it is not a
    /// record. See [`Pairing::take`].
    Doubted {
        line: String,
        cwd: Option<PathBuf>,
        started: Started,
    },
}

/// When a command began.
///
/// A wrapper rather than a bare `Instant` so that [`Pending`] can still derive
/// the traits the tests want, and so a pairing built in a test can measure
/// nothing without pretending to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started(Option<std::time::Instant>);

impl Started {
    pub fn now() -> Started {
        Started(Some(std::time::Instant::now()))
    }

    /// A start that will measure zero, for tests and for a mark that arrived
    /// with no beginning to measure from.
    pub fn unknown() -> Started {
        Started(None)
    }

    fn elapsed(self) -> u64 {
        self.0
            .map(|at| at.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Turns the stream of marks into finished commands.
///
/// Pairing happens here rather than in the shell because the shell cannot do
/// it: the piece of it that reports the line runs in a subshell and cannot
/// remember anything, and the piece that reports the status also fires for
/// the very first prompt, before anything has been run. A status with no
/// command before it is dropped, which is what makes that harmless.
///
/// The other thing decided here is what to do about bash's history. bash is
/// the only shell that cannot hand over the line directly - `$BASH_COMMAND`
/// holds one *simple* command, so a pipeline would arrive as its first
/// component - so the line is read back out of history instead. That is
/// exact, except when history did not take the line: with
/// `HISTCONTROL=ignoredups` a line repeated is not added, and with
/// `ignorespace` a line starting with a space is not added either, and in
/// both cases reading history returns the line *before*. The two have to be
/// told apart, because recording the first is right and recording the second
/// would be recording a line that never ran. Only the first matches the
/// command actually about to execute, which is what the `Executing` mark
/// carries. Where nothing settles it, nothing is recorded.
#[derive(Debug)]
pub struct Pairing {
    pending: Pending,
    /// The history number of the last line taken, for the bash case.
    last_index: u64,
    cwd: Option<PathBuf>,
    /// Whether starts are timed at all. A test turns it off so that
    /// durations stay out of its assertions.
    timing: bool,
}

impl Default for Pairing {
    fn default() -> Pairing {
        Pairing {
            pending: Pending::Nothing,
            last_index: 0,
            cwd: None,
            timing: true,
        }
    }
}

impl Pairing {
    pub fn new() -> Pairing {
        Pairing::default()
    }

    /// A pairing that measures nothing, so a test can assert on the rest.
    #[cfg(test)]
    fn untimed() -> Pairing {
        Pairing {
            timing: false,
            ..Pairing::default()
        }
    }

    /// The moment a command is timed from.
    fn stamp(&self) -> Started {
        match self.timing {
            true => Started::now(),
            false => Started::unknown(),
        }
    }

    /// Where the shell said it was, last time it said anything.
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn take(&mut self, mark: Mark) -> Option<Ran> {
        match mark {
            Mark::Cwd(path) => {
                self.cwd = Some(path);
                None
            }
            Mark::Started { line, .. } if line.trim().is_empty() => {
                // Nothing was run, or nothing that could be read back. Either
                // way there is no line to record, and a blank entry would be
                // an entry saying nothing happened.
                self.pending = Pending::Nothing;
                None
            }
            Mark::Started { index, line } => {
                let cwd = self.cwd.clone();
                let started = self.stamp();
                // Shells that hand over the line directly send 0, and are
                // never in doubt.
                self.pending = if index != 0 && index == self.last_index {
                    Pending::Doubted { line, cwd, started }
                } else {
                    if index != 0 {
                        self.last_index = index;
                    }
                    Pending::Known { line, cwd, started }
                };
                None
            }
            Mark::Executing(word) => {
                // Only a doubted line has anything to settle, and taking one
                // that has not would throw away a perfectly good record.
                if !matches!(self.pending, Pending::Doubted { .. }) {
                    return None;
                }
                if let Pending::Doubted { line, cwd, started } = std::mem::take(&mut self.pending) {
                    // The same first word means the line really is the one
                    // running; a different one means history is holding
                    // something else and this line did not run at all.
                    self.pending = match first_word(&line) == word {
                        true => Pending::Known { line, cwd, started },
                        false => Pending::Nothing,
                    };
                }
                None
            }
            Mark::Finished { code, ms } => match std::mem::take(&mut self.pending) {
                Pending::Known { line, cwd, started } => Some(Ran {
                    line,
                    cwd,
                    code,
                    // The shell's own figure when it has one; otherwise the
                    // time between the two marks, which is what a shell that
                    // reports as it goes actually took.
                    ms: ms.unwrap_or_else(|| started.elapsed()),
                }),
                // Nothing arrived to settle the doubt - which happens when
                // the line was a subshell, since bash does not run a `DEBUG`
                // trap for one. Silence is not evidence, so it is not a
                // record.
                Pending::Doubted { .. } | Pending::Nothing => None,
            },
        }
    }
}

fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// A shell's startup, prepared on disk, and how to launch it.
///
/// The directory is removed when this is dropped, so a session that ends
/// takes its temporary files with it.
#[derive(Debug)]
pub struct Installed {
    dir: PathBuf,
    /// Extra arguments the shell needs to read what was prepared.
    pub args: Vec<String>,
    /// Environment the shell needs, on top of what it inherits.
    pub env: Vec<(String, String)>,
    /// Arguments that must **not** be passed any more, because the hook
    /// replaces what they would have done.
    pub without: Vec<String>,
    pub nonce: String,
}

impl Installed {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A token that is different every time, to tell our marks from anyone else's.
///
/// Not a secret and not trying to be: it never leaves this machine, and all it
/// has to do is not collide with a sequence some other program prints.
pub fn nonce() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}{:x}{:x}", std::process::id(), at, count)
}

/// Write the startup files a hooked shell needs into a directory of its own.
///
/// Returns `None` for a shell with nothing to hook, which is not a failure -
/// the session runs exactly as it did before, and the account says so.
pub fn install(program: &str, login: bool) -> std::io::Result<Option<Installed>> {
    let family = family(program);
    if family == Family::Unhooked {
        return Ok(None);
    }
    let nonce = nonce();
    let dir = std::env::temp_dir().join(format!("lost-commander-{nonce}"));
    std::fs::create_dir_all(&dir)?;

    let installed = match family {
        Family::Bash => install_bash(dir, &nonce, login)?,
        Family::Zsh => install_zsh(dir, &nonce)?,
        Family::Fish => install_fish(dir, &nonce)?,
        Family::PowerShell => install_powershell(dir, &nonce)?,
        Family::Unhooked => unreachable!("checked above"),
    };
    Ok(Some(installed))
}

fn install_bash(dir: PathBuf, nonce: &str, login: bool) -> std::io::Result<Installed> {
    let rc = dir.join("rc.bash");
    std::fs::write(&rc, bash_script(nonce))?;
    Ok(Installed {
        args: vec!["--rcfile".to_string(), rc.display().to_string()],
        // `--rcfile` is only honoured by an *interactive, non-login* bash, so
        // a login shell has to stop being one and do the login sourcing from
        // inside the script instead. That is the whole reason the flag is
        // handed back to be removed rather than simply added to.
        without: vec!["-l".to_string(), "--login".to_string()],
        env: vec![(
            "RCMD_SHELL_LOGIN".to_string(),
            if login { "1" } else { "" }.to_string(),
        )],
        nonce: nonce.to_string(),
        dir,
    })
}

fn bash_script(nonce: &str) -> String {
    format!(
        r##"# Startup for a shell run inside lost-commander. Everything the shell would
# have read on its own is read first, unchanged; the rest only adds the marks
# that say what was run.

if [ -n "$RCMD_SHELL_LOGIN" ]; then
  # A login shell was asked for, but --rcfile made this one not be one, so
  # the login files are sourced here in the order bash would have used.
  [ -f /etc/profile ] && . /etc/profile
  for __rcmd_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -f "$__rcmd_f" ]; then . "$__rcmd_f"; break; fi
  done
  unset __rcmd_f
else
  [ -f /etc/bash.bashrc ] && . /etc/bash.bashrc
  [ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
unset RCMD_SHELL_LOGIN

__rcmd_nonce={nonce}
__rcmd_seen=

# The line, from the one place the whole of it exists.
#
# $BASH_COMMAND would be simpler, but the DEBUG trap fires once per *simple*
# command, so a pipeline would arrive as its first component and a subshell
# would not arrive at all - bash runs no DEBUG trap for one. PS0 is expanded
# once per command *line*, after it has been read and before it runs,
# whatever shape the line is; history has the line as it was typed.
__rcmd_line() {{
  local entry index line
  entry=$(HISTTIMEFORMAT= builtin history 1)
  entry=${{entry#"${{entry%%[![:space:]]*}}"}}
  index=${{entry%% *}}
  line=${{entry#* }}
  line=${{line#"${{line%%[![:space:]]*}}"}}
  case "$index" in *[!0-9]*|"") index=0 ;; esac
  [ -z "$line" ] && return                  # startup, before anything has run
  printf '\033]133;C;%s;%s;%s\007' "$__rcmd_nonce" "$index" "${{line//$'\n'/ }}"
}}

# What is really about to run, which only matters when the history number did
# not move: it is what tells a line repeated apart from a line history was
# told to ignore. First word only - it is a tie-break, not a record.
__rcmd_executing() {{
  [ -n "$__rcmd_seen" ] && return           # only the first fire of a line
  case "$BASH_COMMAND" in __rcmd_*) return ;; esac   # not our own prompt work
  __rcmd_seen=1
  local word=${{BASH_COMMAND%% *}}
  printf '\033]133;E;%s;%s\007' "$__rcmd_nonce" "$word"
}}

__rcmd_finished() {{
  local code=$?
  printf '\033]133;D;%s;%s\007' "$__rcmd_nonce" "$code"
  printf '\033]7;file://%s%s\007' "${{HOSTNAME:-}}" "$PWD"
  __rcmd_seen=
  return $code
}}

trap '__rcmd_executing' DEBUG
PS0='$(__rcmd_line)'"$PS0"

# PROMPT_COMMAND is an array from bash 5.1, and joining a string onto one
# would quietly turn it back into a single command.
if [ "${{BASH_VERSINFO[0]}}" -gt 5 ] 2>/dev/null || {{ [ "${{BASH_VERSINFO[0]}}" -eq 5 ] && [ "${{BASH_VERSINFO[1]}}" -ge 1 ]; }} 2>/dev/null; then
  if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__rcmd_finished "${{PROMPT_COMMAND[@]}}")
  else
    PROMPT_COMMAND="__rcmd_finished${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}"
  fi
else
  PROMPT_COMMAND="__rcmd_finished${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}"
fi
"##
    )
}

fn install_zsh(dir: PathBuf, nonce: &str) -> std::io::Result<Installed> {
    // zsh reads every one of its startup files from $ZDOTDIR, so pointing it
    // here means providing all of them and passing each one on to the file it
    // displaced. The original directory travels in RCMD_ZDOTDIR, and .zshrc
    // hands ZDOTDIR back at the end so that .zlogin - and anything that reads
    // ZDOTDIR later - sees the real one.
    std::fs::write(
        dir.join(".zshenv"),
        "RCMD_ZDOTDIR=${RCMD_ZDOTDIR:-$HOME}\n\
         [[ -f $RCMD_ZDOTDIR/.zshenv ]] && source $RCMD_ZDOTDIR/.zshenv\n",
    )?;
    std::fs::write(
        dir.join(".zprofile"),
        "RCMD_ZDOTDIR=${RCMD_ZDOTDIR:-$HOME}\n\
         [[ -f $RCMD_ZDOTDIR/.zprofile ]] && source $RCMD_ZDOTDIR/.zprofile\n",
    )?;
    std::fs::write(dir.join(".zshrc"), zsh_script(nonce))?;

    let original = std::env::var("ZDOTDIR").unwrap_or_default();
    Ok(Installed {
        args: Vec::new(),
        without: Vec::new(),
        env: vec![
            ("RCMD_ZDOTDIR".to_string(), original),
            ("ZDOTDIR".to_string(), dir.display().to_string()),
        ],
        nonce: nonce.to_string(),
        dir,
    })
}

fn zsh_script(nonce: &str) -> String {
    format!(
        r##"RCMD_ZDOTDIR=${{RCMD_ZDOTDIR:-$HOME}}
[[ -f $RCMD_ZDOTDIR/.zshrc ]] && source $RCMD_ZDOTDIR/.zshrc

__rcmd_nonce={nonce}

# preexec is handed the line as it was typed, so there is no history to read
# and no index to check - hence the 0.
__rcmd_started() {{
  printf '\033]133;C;%s;0;%s\007' "$__rcmd_nonce" "${{1//$'\n'/ }}"
}}

__rcmd_finished() {{
  local code=$?
  printf '\033]133;D;%s;%s\007' "$__rcmd_nonce" "$code"
  printf '\033]7;file://%s%s\007' "${{HOST:-}}" "$PWD"
  return $code
}}

# add-zsh-hook composes rather than replaces, so anything the user already
# had on preexec or precmd keeps running.
autoload -Uz add-zsh-hook 2>/dev/null
if (( $+functions[add-zsh-hook] )); then
  add-zsh-hook preexec __rcmd_started
  add-zsh-hook precmd __rcmd_finished
else
  preexec_functions+=(__rcmd_started)
  precmd_functions+=(__rcmd_finished)
fi

ZDOTDIR=$RCMD_ZDOTDIR
"##
    )
}

fn install_fish(dir: PathBuf, nonce: &str) -> std::io::Result<Installed> {
    // fish reads vendor_conf.d out of every entry in XDG_DATA_DIRS, which is
    // the one way in that does not touch the user's own config directory.
    let conf = dir.join("fish").join("vendor_conf.d");
    std::fs::create_dir_all(&conf)?;
    std::fs::write(conf.join("lost-commander.fish"), fish_script(nonce))?;

    let existing = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    Ok(Installed {
        args: Vec::new(),
        without: Vec::new(),
        env: vec![(
            "XDG_DATA_DIRS".to_string(),
            format!("{}:{existing}", dir.display()),
        )],
        nonce: nonce.to_string(),
        dir,
    })
}

fn install_powershell(dir: PathBuf, nonce: &str) -> std::io::Result<Installed> {
    let profile = dir.join("lostc-profile.ps1");
    std::fs::write(&profile, powershell_script(nonce))?;
    Ok(Installed {
        // -NoExit keeps the shell after the file runs, which is the whole
        // point: -File without it would run the hook and quit. -File must be
        // last, because everything after it is the script's own arguments.
        args: vec![
            "-NoExit".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            profile.display().to_string(),
        ],
        without: Vec::new(),
        env: Vec::new(),
        nonce: nonce.to_string(),
        dir,
    })
}

/// The `prompt` function, which PowerShell calls before drawing each prompt.
///
/// PowerShell has no preexec. What it does have is a history that records
/// every command with the time it started and ended, and a `prompt` that runs
/// after each one - so the mark for "a command ran" is emitted afterwards,
/// reading the entry that just closed. The account gets the line, the
/// directory, the exit code and the duration, which is everything it shows;
/// what it does not get is a mark at the moment the command *starts*, so a
/// command still running is not yet visible. That is a real difference from
/// bash and zsh, and the honest one to accept rather than fake with a timer.
///
/// The user's own profile is dot-sourced first, unchanged, and their `prompt`
/// is kept and called - a hook that replaced someone's prompt with a bare
/// `PS>` would be a hook nobody would leave switched on.
fn powershell_script(nonce: &str) -> String {
    format!(
        r##"# Startup for a shell run inside lost-commander. The user's own profile is
# read first and unchanged; the rest only adds the marks that say what ran.

foreach ($__rcmd_p in @(
    $PROFILE.AllUsersAllHosts, $PROFILE.AllUsersCurrentHost,
    $PROFILE.CurrentUserAllHosts, $PROFILE.CurrentUserCurrentHost)) {{
    if ($__rcmd_p -and (Test-Path -LiteralPath $__rcmd_p)) {{ . $__rcmd_p }}
}}
Remove-Variable __rcmd_p -ErrorAction SilentlyContinue

$global:__rcmdNonce = '{nonce}'
$global:__rcmdSeen = 0

# [char]27 and not `e. The backtick-e escape arrived in PowerShell 6, and
# Windows still ships 5.1 as powershell.exe - where `e is simply the letter
# e, so every mark this wrote came out as text in the middle of the prompt
# and nothing was ever reported.
$global:__rcmdEsc = [char]27
$global:__rcmdBel = [char]7

# Kept and called, so the prompt someone spent years on still draws.
if (Test-Path Function:\prompt) {{
    $global:__rcmdInner = (Get-Item Function:\prompt).ScriptBlock
}} else {{
    $global:__rcmdInner = {{ "PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) " }}
}}

function global:prompt {{
    # Read before anything else here can disturb them.
    $__ok = $?
    $__last = Get-History -Count 1 -ErrorAction SilentlyContinue

    if ($__last -and $__last.Id -gt $global:__rcmdSeen) {{
        $global:__rcmdSeen = $__last.Id
        $__line = ($__last.CommandLine -replace "`r?`n", ' ')
        $__ms = 0
        if ($__last.EndExecutionTime -and $__last.StartExecutionTime) {{
            $__ms = [int](($__last.EndExecutionTime - $__last.StartExecutionTime).TotalMilliseconds)
        }}
        # A native program's code is in $LASTEXITCODE; a cmdlet only sets $?.
        $__code = 0
        if ($null -ne $global:LASTEXITCODE -and $__last.ExecutionStatus -ne 'Completed') {{
            $__code = $global:LASTEXITCODE
        }} elseif (-not $__ok) {{
            $__code = 1
        }} elseif ($null -ne $global:LASTEXITCODE) {{
            $__code = $global:LASTEXITCODE
        }}

        # The same marks the other shells emit. The directory goes as OSC 7,
        # which is the one every terminal already understands - 133;E is the
        # *executing word*, and sending a path as one would file the
        # directory under the wrong question.
        $__url = 'file://' + [uri]::EscapeDataString($env:COMPUTERNAME) + '/' +
                 ((Get-Location).Path -replace '\\', '/')
        [Console]::Write("$($global:__rcmdEsc)]7;$__url$($global:__rcmdBel)")
        [Console]::Write("$($global:__rcmdEsc)]133;C;$($global:__rcmdNonce);0;$__line$($global:__rcmdBel)")
        # The duration is ours to state: the start and the end are emitted
        # together here, so anything timing between them would read zero.
        [Console]::Write("$($global:__rcmdEsc)]133;D;$($global:__rcmdNonce);$__code;$__ms$($global:__rcmdBel)")
    }}

    & $global:__rcmdInner
}}
"##
    )
}

fn fish_script(nonce: &str) -> String {
    format!(
        r##"# Startup for a shell run inside lost-commander.
set -g __rcmd_nonce {nonce}

function __rcmd_started --on-event fish_preexec
    printf '\033]133;C;%s;0;%s\007' $__rcmd_nonce (string replace -a \n ' ' -- $argv[1])
end

function __rcmd_finished --on-event fish_postexec
    set -l code $status
    printf '\033]133;D;%s;%s\007' $__rcmd_nonce $code
    printf '\033]7;file://%s%s\007' (hostname) "$PWD"
end
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(nonce: &str, chunks: &[&[u8]]) -> Vec<Mark> {
        let mut reader = Marks::new(nonce);
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(reader.feed(chunk));
        }
        out
    }

    #[test]
    fn families_go_by_the_name_the_shell_is_invoked_under() {
        assert_eq!(family("/bin/bash"), Family::Bash);
        assert_eq!(family("/usr/local/bin/zsh"), Family::Zsh);
        assert_eq!(family("/usr/bin/fish"), Family::Fish);
        // sh is usually a symlink to one of the above, but a shell reads its
        // own argv[0] and behaves differently under that name.
        assert_eq!(family("/bin/sh"), Family::Unhooked);
        assert_eq!(family("/bin/dash"), Family::Unhooked);
        assert_eq!(family("/bin/rbash"), Family::Unhooked);
        assert!(journals("/bin/zsh"));
        assert!(!journals("/bin/dash"));
        assert!(why_not().contains("not recorded"));
    }

    #[test]
    fn a_command_and_its_status_come_back() {
        let seen = marks(
            "abc",
            &[b"\x1b]133;C;abc;12;echo hello\x07some output\n\x1b]133;D;abc;0\x07"],
        );
        assert_eq!(
            seen,
            vec![
                Mark::Started {
                    index: 12,
                    line: "echo hello".to_string()
                },
                Mark::Finished { code: 0, ms: None },
            ]
        );
    }

    #[test]
    fn a_line_may_contain_semicolons() {
        // The line is the last field precisely so that this works.
        let seen = marks(
            "abc",
            &[b"\x1b]133;C;abc;3;for i in 1 2; do echo $i; done\x07"],
        );
        assert_eq!(
            seen,
            vec![Mark::Started {
                index: 3,
                line: "for i in 1 2; do echo $i; done".to_string()
            }]
        );
    }

    #[test]
    fn marks_from_anyone_else_are_ignored() {
        // The user's own shell integration, or a program printing an escape
        // sequence on purpose. Neither is a command this session ran.
        let seen = marks(
            "ours",
            &[
                b"\x1b]133;C;theirs;1;rm -rf /\x07",
                b"\x1b]133;D;theirs;0\x07",
                b"\x1b]133;C\x07",
                b"\x1b]133;C;ours;1;ls\x07",
            ],
        );
        assert_eq!(
            seen,
            vec![Mark::Started {
                index: 1,
                line: "ls".to_string()
            }]
        );
    }

    #[test]
    fn a_mark_split_across_reads_still_arrives() {
        // 8 KiB reads land wherever they land.
        let seen = marks(
            "abc",
            &[
                b"\x1b]133;C;a",
                b"bc;7;make all\x07",
                b"\x1b]133;D;abc",
                b";2\x07",
            ],
        );
        assert_eq!(
            seen,
            vec![
                Mark::Started {
                    index: 7,
                    line: "make all".to_string()
                },
                Mark::Finished { code: 2, ms: None },
            ]
        );
    }

    #[test]
    fn the_other_terminator_works_too() {
        let seen = marks("abc", &[b"\x1b]133;C;abc;1;ls\x1b\\"]);
        assert_eq!(
            seen,
            vec![Mark::Started {
                index: 1,
                line: "ls".to_string()
            }]
        );
    }

    #[test]
    fn an_escape_inside_a_string_is_not_a_terminator() {
        // ESC followed by anything but a backslash is part of the string.
        let seen = marks("abc", &[b"\x1b]133;C;abc;1;echo \x1bx\x07"]);
        assert_eq!(
            seen,
            vec![Mark::Started {
                index: 1,
                line: "echo \u{1b}x".to_string()
            }]
        );
    }

    #[test]
    fn a_device_control_string_is_not_mistaken_for_a_mark() {
        // tmux passthrough wraps sequences in DCS. What is inside belongs to
        // the inner terminal, not to us.
        let seen = marks(
            "abc",
            &[b"\x1bP\x1b]133;C;abc;1;not-ours\x07\x1b\\\x1b]133;C;abc;2;ours\x07"],
        );
        assert_eq!(
            seen,
            vec![Mark::Started {
                index: 2,
                line: "ours".to_string()
            }]
        );
    }

    #[test]
    fn a_sequence_that_never_ends_does_not_grow_without_limit() {
        let mut reader = Marks::new("abc");
        let huge = vec![b'x'; MAX_BODY * 3];
        assert!(reader.feed(b"\x1b]133;C;abc;1;").is_empty());
        assert!(reader.feed(&huge).is_empty());
        assert!(reader.feed(b"\x07").is_empty(), "the flood is dropped");
        // And the reader is still usable afterwards.
        assert_eq!(
            reader.feed(b"\x1b]133;C;abc;2;ls\x07"),
            vec![Mark::Started {
                index: 2,
                line: "ls".to_string()
            }]
        );
    }

    #[test]
    fn the_directory_comes_out_of_a_file_url() {
        let seen = marks("abc", &[b"\x1b]7;file://vm/home/me/my%20work\x07"]);
        assert_eq!(seen, vec![Mark::Cwd(PathBuf::from("/home/me/my work"))]);
        // No host is fine; no path at all is not a directory.
        assert_eq!(
            marks("abc", &[b"\x1b]7;file:///tmp\x07"]),
            vec![Mark::Cwd(PathBuf::from("/tmp"))]
        );
        assert!(marks("abc", &[b"\x1b]7;file://vm\x07"]).is_empty());
        assert!(marks("abc", &[b"\x1b]7;not-a-url\x07"]).is_empty());
    }

    #[test]
    fn other_operating_system_commands_are_left_alone() {
        // Window titles are the common one, and are none of our business.
        assert!(marks("abc", &[b"\x1b]0;a title\x07"]).is_empty());
        assert!(marks("abc", &[b"\x1b]133;A\x07\x1b]133;B\x07"]).is_empty());
    }

    #[test]
    fn pairing_turns_two_marks_into_one_command() {
        let mut pairing = Pairing::untimed();
        assert_eq!(pairing.take(Mark::Cwd(PathBuf::from("/work"))), None);
        assert_eq!(
            pairing.take(Mark::Started {
                index: 1,
                line: "make".to_string()
            }),
            None
        );
        assert_eq!(
            pairing.take(Mark::Finished { code: 2, ms: None }),
            Some(Ran {
                line: "make".to_string(),
                cwd: Some(PathBuf::from("/work")),
                code: 2,
                ms: 0,
            })
        );
        assert!(Ran {
            line: String::new(),
            cwd: None,
            code: 2,
            ms: 0,
        }
        .failed());
    }

    #[test]
    fn a_status_with_no_command_before_it_is_dropped() {
        // Which is what the first prompt of every session produces.
        let mut pairing = Pairing::untimed();
        assert_eq!(pairing.take(Mark::Finished { code: 0, ms: None }), None);
        assert_eq!(pairing.take(Mark::Finished { code: 0, ms: None }), None);
    }

    #[test]
    fn the_directory_is_the_one_the_command_started_in() {
        // `cd elsewhere && make` reports the new directory afterwards, but
        // the command began in the old one.
        let mut pairing = Pairing::untimed();
        pairing.take(Mark::Cwd(PathBuf::from("/before")));
        pairing.take(Mark::Started {
            index: 1,
            line: "cd /after".to_string(),
        });
        pairing.take(Mark::Cwd(PathBuf::from("/after")));
        let ran = pairing
            .take(Mark::Finished { code: 0, ms: None })
            .expect("a command");
        assert_eq!(ran.cwd, Some(PathBuf::from("/before")));
        assert_eq!(pairing.cwd(), Some(Path::new("/after")));
    }

    /// Run one line past a pairing the way a shell reports it.
    fn one(
        pairing: &mut Pairing,
        index: u64,
        line: &str,
        executing: Option<&str>,
        code: i32,
    ) -> Option<Ran> {
        pairing.take(Mark::Started {
            index,
            line: line.to_string(),
        });
        if let Some(word) = executing {
            pairing.take(Mark::Executing(word.to_string()));
        }
        pairing.take(Mark::Finished { code, ms: None })
    }

    #[test]
    fn the_same_line_run_again_is_recorded_again() {
        // With HISTCONTROL=ignoredups - the default on most distributions -
        // the second `make` is never added to history, so reading history
        // hands back the same entry with the same number. It really did run,
        // and the command about to execute says so.
        let mut pairing = Pairing::untimed();
        assert!(one(&mut pairing, 40, "make", Some("make"), 0).is_some());
        let again = one(&mut pairing, 40, "make", Some("make"), 0);
        assert_eq!(again.map(|r| r.line), Some("make".to_string()));
    }

    #[test]
    fn a_line_history_was_told_to_ignore_is_not_recorded_as_the_line_before() {
        // With HISTCONTROL=ignorespace, ` secret` is kept out of history, so
        // reading history hands back `make` - which did not run this time.
        // Recording it would be recording something that never happened.
        let mut pairing = Pairing::untimed();
        assert!(one(&mut pairing, 40, "make", Some("make"), 0).is_some());
        assert_eq!(
            one(&mut pairing, 40, "make", Some("secret"), 0),
            None,
            "history is holding a different line from the one that ran"
        );
        // And the next real line is unaffected.
        assert!(one(&mut pairing, 41, "make install", Some("make"), 0).is_some());
    }

    #[test]
    fn a_repeated_line_with_nothing_to_settle_it_is_left_out() {
        // bash runs no DEBUG trap for a subshell, so a repeated `(make)` has
        // nothing to confirm it. Silence is not evidence.
        let mut pairing = Pairing::untimed();
        assert!(one(&mut pairing, 40, "(make)", None, 0).is_some());
        assert_eq!(one(&mut pairing, 40, "(make)", None, 0), None);
    }

    #[test]
    fn a_line_whose_number_moved_needs_nothing_to_settle() {
        // The ordinary case, and the one a subshell takes: history gained the
        // line, so it is the line, and no DEBUG trap has to have fired.
        let mut pairing = Pairing::untimed();
        assert_eq!(
            one(&mut pairing, 40, "(cd build && make)", None, 0).map(|r| r.line),
            Some("(cd build && make)".to_string())
        );
    }

    #[test]
    fn the_tie_break_only_looks_at_the_first_word() {
        // Everything after it is the shell's own expansion of a line that has
        // already been read exactly, so comparing further would only find
        // differences that do not matter.
        let mut pairing = Pairing::untimed();
        one(&mut pairing, 40, "make -j8 all", Some("make"), 0);
        let again = one(&mut pairing, 40, "make -j8 all", Some("make"), 0);
        assert!(again.is_some());
    }

    #[test]
    fn an_executing_mark_on_its_own_does_nothing() {
        // A prompt function of the user's own can fire the trap after a
        // command has already been recorded.
        let mut pairing = Pairing::untimed();
        assert_eq!(pairing.take(Mark::Executing("anything".to_string())), None);
        assert_eq!(pairing.take(Mark::Finished { code: 0, ms: None }), None);
    }

    #[test]
    fn a_shell_that_reports_its_own_line_is_never_held_back() {
        // zsh and fish send 0, because they hand over the line directly and
        // there is no history reading to go wrong - so they never need a
        // tie-break and never send one.
        let mut pairing = Pairing::untimed();
        for _ in 0..3 {
            assert!(one(&mut pairing, 0, "make", None, 0).is_some());
        }
    }

    #[test]
    fn a_blank_line_is_not_a_command() {
        // bash fires once during startup, before anything has been run and
        // with nothing in history to read.
        let mut pairing = Pairing::untimed();
        pairing.take(Mark::Started {
            index: 0,
            line: String::new(),
        });
        assert_eq!(pairing.take(Mark::Finished { code: 0, ms: None }), None);
    }

    #[test]
    fn an_unhooked_shell_installs_nothing() {
        assert!(install("/bin/dash", false).unwrap().is_none());
        assert!(install("/bin/sh", true).unwrap().is_none());
    }

    #[test]
    fn bash_gets_an_rcfile_and_stops_being_a_login_shell() {
        let installed = install("/bin/bash", true).unwrap().expect("hooked");
        assert_eq!(installed.args[0], "--rcfile");
        let rc = std::path::Path::new(&installed.args[1]);
        assert!(rc.exists());
        // The login flag has to come off, because bash ignores --rcfile with
        // it on - the script does the login sourcing instead.
        assert!(installed.without.contains(&"-l".to_string()));
        let script = std::fs::read_to_string(rc).unwrap();
        assert!(script.contains(&installed.nonce));
        assert!(script.contains("/etc/profile"), "login files are sourced");
        assert!(
            script.contains(".bashrc"),
            "and so are the interactive ones"
        );
        // PS0 and not the DEBUG trap for the line itself: the trap misses a
        // subshell entirely and reports a pipeline as its first component.
        assert!(script.contains("PS0="), "the line comes from PS0");
        assert!(script.contains("DEBUG"), "and the tie-break from the trap");

        let dir = installed.dir().to_path_buf();
        drop(installed);
        assert!(!dir.exists(), "the temporary files go with the session");
    }

    #[test]
    fn zsh_gets_every_startup_file_it_would_have_read() {
        let installed = install("/bin/zsh", false).unwrap().expect("hooked");
        for name in [".zshenv", ".zprofile", ".zshrc"] {
            let path = installed.dir().join(name);
            assert!(path.exists(), "{name} is missing");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("RCMD_ZDOTDIR"),
                "{name} does not pass through to the user's own"
            );
        }
        let names: Vec<&str> = installed.env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"ZDOTDIR"));
        assert!(names.contains(&"RCMD_ZDOTDIR"));
    }

    #[test]
    fn fish_gets_a_vendor_config_and_keeps_the_existing_data_dirs() {
        let installed = install("/usr/bin/fish", false).unwrap().expect("hooked");
        assert!(installed
            .dir()
            .join("fish/vendor_conf.d/lost-commander.fish")
            .exists());
        let (name, value) = &installed.env[0];
        assert_eq!(name, "XDG_DATA_DIRS");
        assert!(value.starts_with(&installed.dir().display().to_string()));
        assert!(
            value.contains("/usr/share"),
            "the shell still finds its own completions"
        );
    }

    #[test]
    fn powershell_is_hooked_under_both_of_its_names() {
        for name in ["powershell.exe", "pwsh", "/usr/bin/pwsh", "C:\\pwsh.exe"] {
            assert_eq!(family(name), Family::PowerShell, "{name}");
        }
        // And cmd still is not: it has no prompt function to keep.
        assert_eq!(family("cmd.exe"), Family::Unhooked);
    }

    #[test]
    fn powershell_gets_a_profile_it_is_told_to_run_and_stay() {
        let installed = install("pwsh", false).unwrap().expect("hooked");
        let profile = installed.dir().join("lostc-profile.ps1");
        assert!(profile.exists());

        // -NoExit or the shell runs the hook and quits, which would be a
        // terminal that closes the moment it opens.
        assert!(installed.args.contains(&"-NoExit".to_string()));
        // -File must be last: everything after it is the script's arguments.
        assert_eq!(
            installed.args.last().unwrap(),
            &profile.display().to_string()
        );
        assert_eq!(installed.args[installed.args.len() - 2], "-File");
    }

    #[test]
    fn the_powershell_hook_keeps_the_prompt_someone_already_had() {
        let script = powershell_script("abc123");
        assert!(
            script.contains("Get-Item Function:\\prompt"),
            "the existing prompt should be captured"
        );
        assert!(
            script.contains("& $global:__rcmdInner"),
            "and called, or the hook replaces someone's prompt with nothing"
        );
        // The user's own profiles are read first, unchanged.
        assert!(script.contains("$PROFILE.CurrentUserCurrentHost"));
    }

    #[test]
    fn the_powershell_hook_emits_the_marks_the_reader_understands() {
        let script = powershell_script("abc123");
        // The line and the ending, and the directory as OSC 7. Not 133;E -
        // that mark is the *executing word*, and a path sent as one would be
        // filed under the wrong question entirely.
        for mark in ["]133;C;", "]133;D;", "]7;"] {
            assert!(script.contains(mark), "missing {mark}");
        }
        assert!(
            !script.contains("]133;E;"),
            "the directory belongs in OSC 7, not in the executing-word mark"
        );
        assert!(script.contains("abc123"), "the nonce goes in the marks");
    }

    #[test]
    fn what_powershell_emits_parses_back_into_a_command() {
        // The end-to-end shape: what the script writes must be what the
        // reader understands, in the order the script writes it.
        let nonce = "abc123";
        let mut marks = Marks::new(nonce.to_string());
        let mut pairing = Pairing::new();
        let mut ran = Vec::new();
        for text in [
            "\x1b]7;file://host/C:/Users/someone\x07".to_string(),
            format!("\x1b]133;C;{nonce};0;Get-ChildItem\x07"),
            format!("\x1b]133;D;{nonce};0;42\x07"),
        ] {
            for mark in marks.feed(text.as_bytes()) {
                if let Some(done) = pairing.take(mark) {
                    ran.push(done);
                }
            }
        }
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(ran[0].line, "Get-ChildItem");
        assert_eq!(ran[0].code, 0);
        // The shell's own figure, not the gap between two marks written in
        // the same breath - which is the whole reason D may carry one.
        assert_eq!(ran[0].ms, 42);
    }

    #[test]
    fn every_session_gets_its_own_nonce() {
        let one = nonce();
        let two = nonce();
        assert_ne!(one, two);
        assert!(!one.is_empty());
    }

    #[test]
    fn a_windows_path_loses_the_uri_slash_and_a_unix_one_keeps_it() {
        // Found by driving PowerShell: a `file://` URI always starts its path
        // with a slash, so a Windows directory arrives as `file:///C:/src`
        // and the naive answer is `/C:/src` - which looks like a path, is not
        // one, and fails by silently not existing. The panel then refused to
        // follow the shell and said nothing, because there was nowhere to go.
        assert_eq!(
            cwd_from_url("file:///C:/Users/you/src"),
            Some(PathBuf::from("C:/Users/you/src"))
        );
        assert_eq!(
            cwd_from_url("file:///c:/tmp"),
            Some(PathBuf::from("c:/tmp"))
        );

        // A Unix path is not a drive letter and keeps its leading slash.
        assert_eq!(
            cwd_from_url("file:///home/you/src"),
            Some(PathBuf::from("/home/you/src"))
        );
        assert_eq!(cwd_from_url("file:///"), Some(PathBuf::from("/")));

        // Nor is a single letter followed by anything else.
        assert_eq!(cwd_from_url("file:///c/tmp"), Some(PathBuf::from("/c/tmp")));
    }

    #[test]
    fn the_host_is_skipped_and_escapes_are_undone() {
        assert_eq!(
            cwd_from_url("file://somewhere/home/you/my%20files"),
            Some(PathBuf::from("/home/you/my files"))
        );
    }
}
