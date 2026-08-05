// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Running commands in the system shell, the way the original Commander's
//! command line worked.
//!
//! Whatever shell the machine actually has is used - `$SHELL` on Unix,
//! `%ComSpec%` on Windows - rather than assuming bash. As with
//! [`crate::mount`], the platform is a parameter to the pure functions here so
//! the Windows behaviour is testable from any host.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mount::Platform;

/// How much output is kept. A runaway `find /` must not eat all the memory.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// The flag that makes a shell run a command string.
///
/// This follows the *program*, not the platform: PowerShell wants `-Command`
/// even on the same machine where `cmd.exe` wants `/C`, and a Git-Bash
/// `bash.exe` on Windows still wants `-c`.
pub fn command_flag(program: &str) -> String {
    match program_name(program).as_str() {
        "cmd" => "/C".to_string(),
        "powershell" | "pwsh" => "-Command".to_string(),
        _ => "-c".to_string(),
    }
}

/// The bare program name, lowercased and without its extension.
///
/// Both separators are handled by hand rather than through `Path`: on Unix a
/// backslash is an ordinary character, so `Path` reads the whole of
/// `C:\Windows\System32\cmd.exe` as a single component and never finds the
/// name. This has to work on any host, because the Windows behaviour is
/// tested from Linux.
pub fn program_name(program: &str) -> String {
    let file = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_end_matches(['/', '\\']);
    let stem = match file.rsplit_once('.') {
        Some((stem, _extension)) if !stem.is_empty() => stem,
        _ => file,
    };
    stem.to_ascii_lowercase()
}

/// The program and flag used to run a command line.
pub fn shell_program(platform: Platform, configured: Option<&str>) -> (String, String) {
    let fallback = match platform {
        Platform::Windows => "cmd.exe",
        _ => "/bin/sh",
    };
    let program = configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string();
    let flag = command_flag(&program);
    (program, flag)
}

/// Decide which shell to use: the user's explicit choice first, then whatever
/// the environment says, then the platform default.
pub fn resolve_shell(
    platform: Platform,
    preferred: Option<&str>,
    from_environment: Option<&str>,
) -> (String, String) {
    let chosen = preferred
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| from_environment.map(str::trim).filter(|s| !s.is_empty()));
    shell_program(platform, chosen)
}

/// What the environment nominates as the shell.
pub fn environment_shell() -> Option<String> {
    match Platform::current() {
        Platform::Windows => std::env::var("ComSpec").ok(),
        _ => std::env::var("SHELL").ok(),
    }
}

/// The shell to use when the caller has no explicit preference.
pub fn current_shell() -> (String, String) {
    resolve_shell(Platform::current(), None, environment_shell().as_deref())
}

/// Parse `/etc/shells`: one path per line, `#` comments, blanks ignored.
pub fn parse_shell_list(contents: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in contents.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Shells the machine appears to offer.
///
/// `exists` is injected so the Windows candidate list is testable from any
/// host, the same trick used for the mount planner.
pub fn available_shells(
    platform: Platform,
    etc_shells: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |candidate: &str| {
        if !candidate.is_empty()
            && exists(candidate)
            && !found.iter().any(|existing| existing == candidate)
        {
            found.push(candidate.to_string());
        }
    };

    match platform {
        Platform::Windows => {
            for candidate in [
                r"C:\Windows\System32\cmd.exe",
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                r"C:\Program Files\Git\bin\bash.exe",
            ] {
                push(candidate);
            }
        }
        _ => {
            // /etc/shells is the system's own answer to this question.
            if let Some(contents) = etc_shells {
                for candidate in parse_shell_list(contents) {
                    push(&candidate);
                }
            }
            for candidate in [
                "/bin/bash",
                "/bin/zsh",
                "/bin/sh",
                "/usr/bin/fish",
                "/bin/dash",
                "/usr/bin/nu",
            ] {
                push(candidate);
            }
        }
    }
    found
}

/// Collapse entries that are the same shell reached by different paths.
///
/// `/bin` is a symlink to `/usr/bin` on current Linux distributions, so
/// `/etc/shells` lists both `/bin/bash` and `/usr/bin/bash` and a naive picker
/// offers "bash" twice.
///
/// The invocation name is part of the key, not just the target: `rbash` and
/// `sh` are usually symlinks to `bash` and `dash`, but a shell inspects
/// `argv[0]` and behaves differently under those names, so they are genuinely
/// different choices and must both survive.
pub fn dedupe_by_target(paths: Vec<String>, canonical: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let key = (canonical(&path), program_name(&path));
        if !seen.contains(&key) {
            seen.push(key);
            out.push(path);
        }
    }
    out
}

/// Shells on this machine, for the picker.
pub fn discover_shells() -> Vec<String> {
    let platform = Platform::current();
    let etc = std::fs::read_to_string("/etc/shells").ok();
    let mut shells = available_shells(platform, etc.as_deref(), &|path| Path::new(path).exists());
    shells = dedupe_by_target(shells, &|path| {
        std::fs::canonicalize(path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.to_string())
    });

    // Whatever the environment nominates belongs in the list even if it is
    // somewhere unusual, such as a Nix store path or a Homebrew prefix.
    if let Some(from_env) = environment_shell() {
        let trimmed = from_env.trim().to_string();
        if !trimmed.is_empty()
            && Path::new(&trimmed).exists()
            && !shells.iter().any(|s| s == &trimmed)
        {
            shells.insert(0, trimmed);
        }
    }
    shells
}

/// Quote a file name so the shell sees it as one argument.
///
/// Unix wraps in single quotes (and escapes any embedded ones); Windows uses
/// double quotes, which is all `cmd` understands.
pub fn quote(name: &str, platform: Platform) -> String {
    let needs = name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || "\"'\\$`&|;<>()*?[]{}!#~".contains(c));
    if !needs {
        return name.to_string();
    }
    match platform {
        Platform::Windows => format!("\"{}\"", name.replace('"', "\"\"")),
        _ => format!("'{}'", name.replace('\'', r"'\''")),
    }
}

pub fn quote_here(name: &str) -> String {
    quote(name, Platform::current())
}

/// Whether a command line contains something [`expand_placeholders`] would
/// change - the front-ends use this to decide when a preview is worth
/// showing, and showing one for every plain command would be noise.
pub fn has_placeholders(line: &str) -> bool {
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some('f' | 's' | 'd' | '%') = chars.peek() {
                return true;
            }
        }
    }
    false
}

/// Expand `%f`, `%s` and `%d` in a command line, quoting every name.
///
/// `%f` is the file under the cursor; `%s` is the marked names, or the file
/// under the cursor when nothing is marked - the same reading of "the
/// selection" as every F-key in this program; `%d` is the other pane's
/// directory; `%%` is a literal percent. Anything else after a `%` passes
/// through untouched, because `100%done` is a name somebody has.
///
/// Expansion happens only where a person typed the line. Lines this program
/// builds for itself - an editor invocation, an elevation - carry real paths,
/// and a file called `100%f.txt` must not change what they mean.
pub fn expand_placeholders(
    line: &str,
    file: Option<&str>,
    marked: &[String],
    other_dir: &std::path::Path,
    platform: Platform,
) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('f') => {
                chars.next();
                if let Some(name) = file {
                    out.push_str(&quote(name, platform));
                }
            }
            Some('s') => {
                chars.next();
                let names: Vec<String> = if marked.is_empty() {
                    file.iter().map(|name| quote(name, platform)).collect()
                } else {
                    marked.iter().map(|name| quote(name, platform)).collect()
                };
                out.push_str(&names.join(" "));
            }
            Some('d') => {
                chars.next();
                out.push_str(&quote(&other_dir.display().to_string(), platform));
            }
            Some('%') => {
                chars.next();
                out.push('%');
            }
            _ => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn each_placeholder_stands_for_what_the_panels_show() {
        let marked = vec!["a name.txt".to_string(), "plain.rs".to_string()];
        let out = expand_placeholders(
            "tar cf out.tar %s && cp %f %d",
            Some("cursor.rs"),
            &marked,
            Path::new("/backup"),
            Platform::Linux,
        );
        // Quoting is only added where a shell needs it: a plain name stays
        // plain, which keeps the expanded line readable.
        assert_eq!(
            out,
            "tar cf out.tar 'a name.txt' plain.rs && cp cursor.rs /backup"
        );
    }

    #[test]
    fn the_selection_falls_back_to_the_cursor_like_every_f_key() {
        let out = expand_placeholders(
            "wc -l %s",
            Some("only.rs"),
            &[],
            Path::new("/x"),
            Platform::Linux,
        );
        assert_eq!(out, "wc -l only.rs");
    }

    #[test]
    fn quoting_is_the_platform_s_own() {
        let out = expand_placeholders(
            "type %f",
            Some("a name.txt"),
            &[],
            Path::new("C:\\backup dir"),
            Platform::Windows,
        );
        assert_eq!(out, "type \"a name.txt\"");
        let dir = expand_placeholders(
            "cd %d",
            None,
            &[],
            Path::new("C:\\backup dir"),
            Platform::Windows,
        );
        assert_eq!(dir, "cd \"C:\\backup dir\"");
    }

    #[test]
    fn a_percent_that_means_nothing_passes_through() {
        // `100%done` is a name somebody has, and `%%` is how you ask for a
        // literal percent next to a real placeholder letter.
        let out = expand_placeholders(
            "echo 100%x and 100%%f",
            Some("f.rs"),
            &[],
            Path::new("/"),
            Platform::Linux,
        );
        assert_eq!(out, "echo 100%x and 100%f");
        assert!(!has_placeholders("echo 100%x"));
        assert!(has_placeholders("echo %s"));
        assert!(has_placeholders("100%%"));
    }

    #[test]
    fn a_missing_cursor_file_expands_to_nothing_rather_than_a_word() {
        let out = expand_placeholders("edit %f", None, &[], Path::new("/"), Platform::Linux);
        assert_eq!(out, "edit ");
    }
}

/// A `cd` typed at the command line, which has to be handled by the file
/// manager: a subprocess changing its own directory would achieve nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intercepted {
    /// `cd` with no argument, or `cd ~` - go home.
    ChangeToHome,
    ChangeTo(String),
}

/// True when the text contains anything meaning "there is more shell syntax
/// here than a path".
fn has_shell_syntax(text: &str) -> bool {
    text.contains(';')
        || text.contains('&')
        || text.contains('|')
        || text.contains('\n')
        || text.contains('`')
        || text.contains("$(")
        || text.contains('>')
        || text.contains('<')
}

pub fn intercept(line: &str) -> Option<Intercepted> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("cd")?;
    // "cdrom" must not be treated as a cd command.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // A compound line belongs to the shell: "cd /tmp && ls" must list /tmp in
    // a subshell, not be mistaken for a directory named "/tmp && ls".
    if has_shell_syntax(rest) {
        return None;
    }
    let target = rest.trim();
    if target.is_empty() || target == "~" {
        return Some(Intercepted::ChangeToHome);
    }
    // Strip one layer of quoting, which is how a path with spaces arrives.
    let unquoted = target
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| target.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(target);
    Some(Intercepted::ChangeTo(unquoted.to_string()))
}

/// Substitute `$NAME` and `${NAME}` from the environment, as a shell would.
///
/// An unset variable expands to nothing, which is also what a shell does.
/// Command substitution is deliberately not attempted - `$(...)` and backticks
/// keep the line out of the interceptor entirely.
pub fn expand_env(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_alphanumeric() || next == '_' {
                name.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if braced && chars.peek() == Some(&'}') {
            chars.next();
        }
        if name.is_empty() {
            // A bare "$" is just a dollar sign.
            out.push('$');
            if braced {
                out.push('{');
            }
            continue;
        }
        if let Ok(value) = std::env::var(&name) {
            out.push_str(&value);
        }
    }
    out
}

/// Resolve a `cd` target against the directory the panel is showing.
pub fn resolve_cd(target: &Intercepted, cwd: &Path) -> Option<PathBuf> {
    match target {
        Intercepted::ChangeToHome => dirs::home_dir(),
        Intercepted::ChangeTo(raw) => {
            let raw = &expand_env(raw);
            let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                dirs::home_dir()?.join(rest)
            } else {
                let candidate = PathBuf::from(raw);
                if candidate.is_absolute() {
                    candidate
                } else {
                    cwd.join(candidate)
                }
            };
            Some(expanded)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

impl CommandOutput {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

fn truncate(bytes: &[u8]) -> String {
    let slice = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    let mut text = String::from_utf8_lossy(slice).to_string();
    if bytes.len() > MAX_OUTPUT_BYTES {
        text.push_str("\n[output truncated]");
    }
    text
}

/// A command running on a worker thread, so a slow build does not freeze the
/// window.
pub struct ShellJob {
    pub line: String,
    pub cwd: PathBuf,
    /// The shell this command was handed to.
    pub shell: String,
    /// When it was started, so the account can say how long it took.
    began: std::time::Instant,
    result: Arc<Mutex<Option<CommandOutput>>>,
    handle: Option<JoinHandle<()>>,
}

impl ShellJob {
    /// Run `line` with whatever shell the caller chose. The shell is passed in
    /// rather than looked up here, so the choice is always the user's setting
    /// and never a hidden global.
    pub fn spawn_with(line: String, cwd: PathBuf, shell: (String, String)) -> ShellJob {
        let result = Arc::new(Mutex::new(None));
        let worker_result = Arc::clone(&result);
        let worker_line = line.clone();
        let worker_cwd = cwd.clone();
        let (program, flag) = shell;
        let recorded = program.clone();

        let handle = std::thread::spawn(move || {
            let output = Command::new(&program)
                .arg(&flag)
                .arg(&worker_line)
                .current_dir(&worker_cwd)
                .output();

            let value = match output {
                Ok(out) => CommandOutput {
                    stdout: truncate(&out.stdout),
                    stderr: truncate(&out.stderr),
                    code: out.status.code(),
                },
                Err(e) => CommandOutput {
                    stdout: String::new(),
                    stderr: format!("could not run {program}: {e}"),
                    code: None,
                },
            };
            *worker_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        });

        ShellJob {
            line,
            cwd,
            shell: recorded,
            began: std::time::Instant::now(),
            result,
            handle: Some(handle),
        }
    }

    /// How long it has been running, or ran for.
    pub fn took(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }

    /// Convenience for callers with no configured preference.
    pub fn spawn(line: String, cwd: PathBuf) -> ShellJob {
        Self::spawn_with(line, cwd, current_shell())
    }

    pub fn is_finished(&self) -> bool {
        self.result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn take(&mut self) -> Option<CommandOutput> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_uses_sh_dash_c_by_default() {
        let (program, flag) = shell_program(Platform::Linux, None);
        assert_eq!(program, "/bin/sh");
        assert_eq!(flag, "-c");
    }

    #[test]
    fn the_configured_shell_wins_when_there_is_one() {
        let (program, flag) = shell_program(Platform::MacOs, Some("/bin/zsh"));
        assert_eq!(program, "/bin/zsh");
        assert_eq!(flag, "-c");
    }

    #[test]
    fn an_empty_shell_variable_falls_back() {
        let (program, _) = shell_program(Platform::Linux, Some(""));
        assert_eq!(program, "/bin/sh");
    }

    #[test]
    fn windows_uses_comspec_and_slash_c() {
        let (program, flag) = shell_program(Platform::Windows, None);
        assert_eq!(program, "cmd.exe");
        assert_eq!(flag, "/C");

        let (program, _) = shell_program(Platform::Windows, Some(r"C:\Windows\System32\cmd.exe"));
        assert_eq!(program, r"C:\Windows\System32\cmd.exe");
    }

    #[test]
    fn the_command_flag_follows_the_program_not_the_platform() {
        // PowerShell and cmd live on the same machine and disagree.
        assert_eq!(command_flag(r"C:\Windows\System32\cmd.exe"), "/C");
        assert_eq!(command_flag("powershell.exe"), "-Command");
        assert_eq!(
            command_flag(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            "-Command"
        );
        // Git-Bash on Windows still wants -c.
        assert_eq!(command_flag(r"C:\Program Files\Git\bin\bash.exe"), "-c");
        assert_eq!(command_flag("/bin/zsh"), "-c");
        assert_eq!(command_flag("/usr/bin/fish"), "-c");
    }

    #[test]
    fn an_explicit_choice_beats_the_environment() {
        let (program, flag) =
            resolve_shell(Platform::Linux, Some("/usr/bin/fish"), Some("/bin/bash"));
        assert_eq!(program, "/usr/bin/fish");
        assert_eq!(flag, "-c");

        // No choice: follow the environment.
        let (program, _) = resolve_shell(Platform::Linux, None, Some("/bin/zsh"));
        assert_eq!(program, "/bin/zsh");

        // Neither: the platform default.
        let (program, _) = resolve_shell(Platform::Linux, None, None);
        assert_eq!(program, "/bin/sh");
        let (program, flag) = resolve_shell(Platform::Windows, None, None);
        assert_eq!(program, "cmd.exe");
        assert_eq!(flag, "/C");

        // A blank setting is not a choice.
        let (program, _) = resolve_shell(Platform::Linux, Some("   "), Some("/bin/zsh"));
        assert_eq!(program, "/bin/zsh");
    }

    #[test]
    fn etc_shells_is_parsed_the_way_the_system_writes_it() {
        let contents = "\
# /etc/shells: valid login shells
/bin/sh

/bin/bash   # the usual one
/usr/bin/fish
/bin/bash
";
        assert_eq!(
            parse_shell_list(contents),
            vec!["/bin/sh", "/bin/bash", "/usr/bin/fish"],
            "comments, blanks and duplicates are dropped"
        );
    }

    #[test]
    fn available_shells_lists_what_exists_on_unix() {
        let etc = "/bin/sh\n/usr/local/bin/elvish\n";
        let present = |p: &str| p == "/bin/sh" || p == "/bin/zsh" || p == "/usr/local/bin/elvish";

        let found = available_shells(Platform::Linux, Some(etc), &present);
        assert_eq!(found, vec!["/bin/sh", "/usr/local/bin/elvish", "/bin/zsh"]);
        // The absent ones are not offered.
        assert!(!found.iter().any(|s| s == "/usr/bin/fish"));
    }

    #[test]
    fn available_shells_offers_the_windows_candidates_that_exist() {
        let present = |p: &str| p.ends_with("cmd.exe") || p.ends_with("pwsh.exe");
        let found = available_shells(Platform::Windows, None, &present);

        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("cmd.exe"));
        assert!(found[1].ends_with("pwsh.exe"));
    }

    #[test]
    fn the_same_binary_by_two_paths_is_offered_once() {
        // /bin is a symlink to /usr/bin on most current Linux systems.
        let listed = vec![
            "/bin/sh".to_string(),
            "/usr/bin/sh".to_string(),
            "/bin/bash".to_string(),
            "/usr/bin/bash".to_string(),
            "/usr/bin/fish".to_string(),
        ];
        // Must be idempotent, as a real canonicalise is: /usr/bin/sh already
        // *is* the resolved path.
        let canonical = |p: &str| match p.strip_prefix("/bin/") {
            Some(rest) => format!("/usr/bin/{rest}"),
            None => p.to_string(),
        };

        assert_eq!(
            dedupe_by_target(listed, &canonical),
            vec!["/bin/sh", "/bin/bash", "/usr/bin/fish"],
            "the first spelling of each binary wins"
        );
    }

    #[test]
    fn a_restricted_alias_of_the_same_binary_is_kept() {
        // rbash is a symlink to bash, but bash checks argv[0] and runs
        // restricted under that name, so it is a real, separate choice.
        let listed = vec![
            "/bin/bash".to_string(),
            "/usr/bin/bash".to_string(),
            "/bin/rbash".to_string(),
        ];
        let canonical = |_: &str| "/usr/bin/bash".to_string();

        assert_eq!(
            dedupe_by_target(listed, &canonical),
            vec!["/bin/bash", "/bin/rbash"]
        );
    }

    #[test]
    fn genuinely_different_shells_all_survive_deduping() {
        let listed = vec![
            "/bin/bash".to_string(),
            "/opt/homebrew/bin/bash".to_string(),
        ];
        // Different real files, despite the shared name.
        let canonical = |p: &str| p.to_string();
        assert_eq!(dedupe_by_target(listed.clone(), &canonical), listed);
    }

    #[test]
    fn a_command_can_be_given_an_explicit_shell() {
        // The proof is asking the shell to name itself, so this really does
        // run the shell that was named rather than whatever the default is.
        // Every platform spells all three of those differently.
        #[cfg(windows)]
        let (shell, flag, ask, names_itself) = ("cmd.exe", "/C", "echo %COMSPEC%", "cmd.exe");
        #[cfg(not(windows))]
        let (shell, flag, ask, names_itself) = ("/bin/sh", "-c", "echo $0", "/bin/sh");

        let dir = tempfile::tempdir().unwrap();
        let mut job = ShellJob::spawn_with(
            ask.to_string(),
            dir.path().to_path_buf(),
            (shell.to_string(), flag.to_string()),
        );
        job.join();
        let out = job.take().unwrap();

        assert!(out.succeeded(), "{}", out.stderr);
        assert_eq!(job.shell, shell);
        // Lowercased because Windows reports its own path in mixed case.
        assert!(
            out.stdout.to_lowercase().contains(names_itself),
            "{}",
            out.stdout
        );
    }

    #[test]
    fn plain_names_are_not_quoted() {
        assert_eq!(quote("main.rs", Platform::Linux), "main.rs");
        assert_eq!(quote("report-2024.pdf", Platform::Linux), "report-2024.pdf");
    }

    #[test]
    fn names_with_spaces_or_metacharacters_are_quoted() {
        assert_eq!(quote("my holiday.jpg", Platform::Linux), "'my holiday.jpg'");
        assert_eq!(quote("a&b.txt", Platform::Linux), "'a&b.txt'");
        assert_eq!(quote("$HOME.txt", Platform::Linux), "'$HOME.txt'");
        assert_eq!(
            quote("my holiday.jpg", Platform::Windows),
            "\"my holiday.jpg\""
        );
    }

    #[test]
    fn embedded_quotes_are_escaped_per_platform() {
        assert_eq!(quote("it's.txt", Platform::Linux), r"'it'\''s.txt'");
        assert_eq!(
            quote("say \"hi\".txt", Platform::Windows),
            "\"say \"\"hi\"\".txt\""
        );
    }

    #[test]
    fn cd_is_intercepted_in_its_various_forms() {
        assert_eq!(intercept("cd"), Some(Intercepted::ChangeToHome));
        assert_eq!(intercept("  cd  "), Some(Intercepted::ChangeToHome));
        assert_eq!(intercept("cd ~"), Some(Intercepted::ChangeToHome));
        assert_eq!(
            intercept("cd /usr/local"),
            Some(Intercepted::ChangeTo("/usr/local".into()))
        );
        assert_eq!(
            intercept("cd 'my documents'"),
            Some(Intercepted::ChangeTo("my documents".into()))
        );
    }

    #[test]
    fn a_compound_line_is_left_to_the_shell() {
        // "cd /tmp && ls" has to reach the shell, which changes directory in
        // its own subshell and lists /tmp. Swallowing it would send us looking
        // for a directory literally named "/tmp && ls".
        for line in [
            "cd /tmp && ls",
            "cd src; make",
            "cd build || echo no",
            "cd logs | tail",
            "cd $(pwd)",
            "cd `pwd`",
            "cd out > log.txt",
        ] {
            assert!(intercept(line).is_none(), "{line} should go to the shell");
        }
    }

    #[test]
    fn environment_variables_are_expanded_in_cd_targets() {
        std::env::set_var("RCMD_TEST_DIR", "/opt/example");
        let cwd = Path::new("/tmp");

        assert_eq!(
            resolve_cd(&Intercepted::ChangeTo("$RCMD_TEST_DIR".into()), cwd).unwrap(),
            PathBuf::from("/opt/example")
        );
        assert_eq!(
            resolve_cd(&Intercepted::ChangeTo("${RCMD_TEST_DIR}/sub".into()), cwd).unwrap(),
            PathBuf::from("/opt/example/sub")
        );

        std::env::remove_var("RCMD_TEST_DIR");
    }

    #[test]
    fn expand_env_handles_the_awkward_cases_like_a_shell() {
        std::env::set_var("RCMD_X", "value");

        assert_eq!(expand_env("plain/path"), "plain/path");
        assert_eq!(expand_env("$RCMD_X/tail"), "value/tail");
        assert_eq!(expand_env("${RCMD_X}tail"), "valuetail");
        // An unset variable disappears, as in a shell.
        assert_eq!(expand_env("$RCMD_DEFINITELY_UNSET/x"), "/x");
        // A lone dollar stays literal.
        assert_eq!(expand_env("costs $ money"), "costs $ money");
        assert_eq!(expand_env("100$"), "100$");

        std::env::remove_var("RCMD_X");
    }

    #[test]
    fn commands_that_merely_start_with_cd_are_left_alone() {
        assert!(intercept("cdrom").is_none());
        assert!(intercept("cdda2wav").is_none());
        assert!(intercept("ls").is_none());
        assert!(intercept("").is_none());
    }

    #[test]
    fn relative_cd_resolves_against_the_current_panel() {
        let cwd = Path::new("/home/user/projects");
        let resolved = resolve_cd(&Intercepted::ChangeTo("src".into()), cwd).unwrap();
        assert_eq!(resolved, PathBuf::from("/home/user/projects/src"));

        let absolute = resolve_cd(&Intercepted::ChangeTo("/etc".into()), cwd).unwrap();
        assert_eq!(absolute, PathBuf::from("/etc"));
    }

    #[test]
    fn tilde_paths_expand_to_the_home_directory() {
        let cwd = Path::new("/tmp/somewhere/else");
        let home = dirs::home_dir().expect("a home directory");

        let resolved = resolve_cd(&Intercepted::ChangeTo("~/abc".into()), cwd).unwrap();
        assert_eq!(resolved, home.join("abc"));
        // Crucially not treated as a relative path under the current panel.
        assert!(!resolved.starts_with(cwd));

        let deeper = resolve_cd(&Intercepted::ChangeTo("~/abc/def".into()), cwd).unwrap();
        assert_eq!(deeper, home.join("abc").join("def"));

        // A bare "~" is home itself.
        assert_eq!(resolve_cd(&Intercepted::ChangeToHome, cwd).unwrap(), home);
    }

    #[test]
    fn a_command_runs_and_reports_its_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "x").unwrap();

        let mut job = ShellJob::spawn("ls".to_string(), dir.path().to_path_buf());
        job.join();
        let out = job.take().expect("the command should have finished");

        assert!(out.succeeded(), "stderr: {}", out.stderr);
        assert!(out.stdout.contains("hello.txt"), "stdout: {}", out.stdout);
    }

    #[test]
    fn a_failing_command_reports_its_status_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = ShellJob::spawn(
            "ls /definitely/not/here".to_string(),
            dir.path().to_path_buf(),
        );
        job.join();
        let out = job.take().unwrap();

        assert!(!out.succeeded());
        assert!(!out.stderr.is_empty());
    }

    #[test]
    fn commands_run_in_the_directory_the_panel_is_showing() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();

        let mut job = ShellJob::spawn("pwd".to_string(), nested.clone());
        job.join();
        let out = job.take().unwrap();

        // The temp directory may be a symlink (/tmp -> /private/tmp on macOS),
        // so compare the final component rather than the whole path.
        assert!(out.stdout.trim().ends_with("nested"), "{}", out.stdout);
    }

    #[test]
    fn very_long_output_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = ShellJob::spawn(
            format!("head -c {} /dev/zero | tr '\\0' 'x'", MAX_OUTPUT_BYTES * 4),
            dir.path().to_path_buf(),
        );
        job.join();
        let out = job.take().unwrap();

        assert!(out.stdout.len() <= MAX_OUTPUT_BYTES + 32);
        assert!(out.stdout.ends_with("[output truncated]"));
    }
}

/// How to tell this shell to change directory.
///
/// Quoting is not one thing. A POSIX shell and PowerShell both take single
/// quotes and treat what is inside literally, which is what a path wants -
/// but `cmd` has no single quotes at all and reads them as part of the name,
/// so `cd 'C:\src'` is an error rather than a directory change. `cmd` also
/// needs `/d` to cross drives, which is the case a file manager hits at once.
///
/// Here rather than in a front-end because it is a fact about shells, and
/// because a second front-end wanting the same thing should not have to
/// rediscover that `cmd` is different.
pub fn cd_command(program: &str, path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match program_name(program).as_str() {
        "cmd" | "cmd.exe" => format!("cd /d \"{shown}\""),
        // Single quotes are literal in PowerShell as well, so a space or a
        // `$` in a name arrives intact - but the two families escape a quote
        // *inside* one differently, and a name that closed the quote early
        // would turn the rest of the path into commands.
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => {
            format!("cd '{}'", shown.replace('\'', "''"))
        }
        _ => format!("cd '{}'", shown.replace('\'', r"'\''")),
    }
}

#[cfg(test)]
mod cd_tests {
    use super::cd_command;
    use std::path::Path;

    #[test]
    fn cmd_gets_double_quotes_and_a_drive_switch() {
        // `cd 'C:\src'` in cmd is an error: it has no single quotes and
        // reads them as part of the name. Without `/d` it will not cross
        // from one drive to another, which is the first thing a file manager
        // asks it to do.
        assert_eq!(
            cd_command("cmd.exe", Path::new(r"C:\src")),
            r#"cd /d "C:\src""#
        );
    }

    #[test]
    fn everything_else_gets_single_quotes() {
        assert_eq!(cd_command("bash", Path::new("/home/you")), "cd '/home/you'");
        assert_eq!(
            cd_command("powershell.exe", Path::new(r"C:\Program Files")),
            r"cd 'C:\Program Files'"
        );
    }

    #[test]
    fn powershell_doubles_a_quote_and_posix_does_not() {
        assert_eq!(
            cd_command("pwsh", Path::new("/tmp/it's")),
            "cd '/tmp/it''s'"
        );
    }

    #[test]
    fn a_quote_in_a_name_does_not_end_the_quoting() {
        // A directory may be called anything at all, and a name that closed
        // the quote would turn the rest of it into commands.
        assert_eq!(
            cd_command("bash", Path::new("/tmp/it's")),
            r"cd '/tmp/it'\''s'"
        );
    }
}
