// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Running something with administrator privileges.
//!
//! Every platform's answer is a *request*, never a grant: this asks the system
//! to authorise, and the system asks the user. There is no path through here
//! that gains privilege quietly, and there should not be.
//!
//! # What this is not for
//!
//! "Open as administrator" reads like the answer to a permission problem, and
//! for a whole graphical application it usually is not:
//!
//! * A root GUI application writes root-owned files into the user's own
//!   configuration directory, which then breaks that application for the
//!   ordinary user afterwards. `sudo gedit` once is a classic way to have to
//!   `chown` your home directory back.
//! * On Wayland a root process generally cannot reach the display at all, and
//!   on macOS a `.app` bundle run as root is unsupported rather than merely
//!   discouraged.
//! * The whole program gets root - every plugin it loads and every URL it
//!   opens - when the actual need was to write one file.
//!
//! So the two routes that are *right* are first-class here, and the blunt one
//! is available with its edges named. [`edit_as_root`] is the specific
//! answer to the specific case: `sudoedit` copies the file out, runs the
//! editor as **you**, and writes the result back as root, so nothing but the
//! write is privileged. And a root shell in the panel's directory
//! ([`root_shell`]) is what most "as administrator" clicks were reaching for.

use std::path::Path;

use crate::mount::Platform;
use crate::open::Launch;
use crate::preview::program_exists;
use crate::shell::quote;

/// How a privileged command gets started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevation {
    /// Spawn this. The system puts up its own authorisation dialog - UAC, the
    /// macOS authentication panel, PolicyKit's prompt.
    Command(Launch),
    /// Type this at a shell tab, where the password prompt has a terminal to
    /// appear on. `sudo` with nowhere to ask is `sudo` that fails.
    Shell(String),
    /// The platform does not do this, and the reason is worth saying.
    Refused(String),
}

/// Graphical helpers that ask for authorisation on Linux, best first.
///
/// `pkexec` is PolicyKit's and is what a desktop with a session bus has.
/// `gksu` is deliberately absent: it was removed from Debian and Ubuntu years
/// ago precisely because it encouraged what the module note above warns about.
pub const LINUX_ASKERS: &[&str] = &["pkexec", "kdesu", "lxqt-sudo"];

/// Wrap `launch` so it runs with administrator privileges.
pub fn elevate(
    platform: Platform,
    launch: &Launch,
    display: Option<(&str, &str)>,
    exists: &dyn Fn(&Path) -> bool,
) -> Elevation {
    match platform {
        Platform::Windows => Elevation::Command(windows_elevated(launch)),
        Platform::MacOs => Elevation::Command(macos_elevated(launch)),
        Platform::Linux => match LINUX_ASKERS.iter().find(|a| program_exists(a, exists)) {
            Some(asker) => Elevation::Command(linux_elevated(asker, launch, display)),
            // No graphical helper installed, so the prompt needs a terminal -
            // and there are terminals in this window.
            None => Elevation::Shell(sudo_line(launch, Platform::Linux)),
        },
    }
}

/// `Start-Process -Verb RunAs`, which is what raises UAC.
///
/// The script is handed over **base64-encoded UTF-16** rather than as text.
/// PowerShell re-joins and re-parses the arguments it is given for `-Command`,
/// on top of the C runtime quoting Rust already applied, and a file name with
/// a quote or a `$` in it does not survive both. `-EncodedCommand` has no
/// parsing left to go wrong.
fn windows_elevated(launch: &Launch) -> Launch {
    let mut script = format!(
        "Start-Process -FilePath {}",
        powershell_quote(&launch.program)
    );
    if !launch.args.is_empty() {
        let args: Vec<String> = launch.args.iter().map(|a| powershell_quote(a)).collect();
        script.push_str(&format!(" -ArgumentList {}", args.join(",")));
    }
    script.push_str(" -Verb RunAs");
    powershell_command(&script)
}

/// `do shell script ... with administrator privileges`, which raises the
/// macOS authentication panel.
fn macos_elevated(launch: &Launch) -> Launch {
    let script = format!(
        "do shell script {} with administrator privileges",
        applescript_string(&sudo_free_line(launch, Platform::MacOs))
    );
    Launch {
        program: "osascript".into(),
        args: vec!["-e".into(), script],
    }
}

/// `pkexec` and friends, which raise PolicyKit's prompt.
fn linux_elevated(asker: &str, launch: &Launch, display: Option<(&str, &str)>) -> Launch {
    let mut args: Vec<String> = Vec::new();

    // pkexec deliberately starts the program with a minimal environment, so a
    // graphical one loses the display it was going to draw on and dies with a
    // "cannot open display". Passing the two variables back through `env` is
    // what every desktop's own launcher does.
    if asker == "pkexec" {
        if let Some((display_value, xauthority)) = display {
            args.push("env".into());
            args.push(format!("DISPLAY={display_value}"));
            if !xauthority.is_empty() {
                args.push(format!("XAUTHORITY={xauthority}"));
            }
        }
    }
    args.push(launch.program.clone());
    args.extend(launch.args.iter().cloned());

    Launch {
        program: asker.to_string(),
        args,
    }
}

/// The command as one shell line, without `sudo`.
fn sudo_free_line(launch: &Launch, platform: Platform) -> String {
    let mut line = quote(&launch.program, platform);
    for arg in &launch.args {
        line.push(' ');
        line.push_str(&quote(arg, platform));
    }
    line
}

/// The command as a `sudo` line to type at a shell.
pub fn sudo_line(launch: &Launch, platform: Platform) -> String {
    format!("sudo {}", sudo_free_line(launch, platform))
}

/// Edit a file you do not own, the way that does not need root to hold the
/// editor.
///
/// `sudoedit` copies the file to a temporary one, runs **your** editor as
/// **you**, and installs the result back as root when you are done. Only the
/// write is privileged: no editor plugin runs as root, no root-owned files
/// appear in your home directory, and a graphical editor still talks to your
/// own display because it is still your process.
///
/// Windows has no equivalent, so there the editor itself is elevated - which
/// is the blunt instrument, but it is the only one on offer.
pub fn edit_as_root(platform: Platform, editor: &str, path: &Path) -> Elevation {
    let target = path.display().to_string();
    match platform {
        Platform::Windows => Elevation::Command(windows_elevated(&Launch {
            program: editor.to_string(),
            args: vec![target],
        })),
        _ => Elevation::Shell(format!(
            "SUDO_EDITOR={} sudoedit {}",
            quote(editor, platform),
            quote(&target, platform)
        )),
    }
}

/// A shell running as administrator, in `cwd`.
///
/// The answer most "as administrator" reaches for: somewhere privileged to
/// work, rather than one privileged application. It goes to a shell tab
/// because that is what it is.
pub fn root_shell(platform: Platform, cwd: &Path) -> Elevation {
    let target = cwd.display().to_string();
    match platform {
        // `-NoExit` so the window stays after the command, and UAC gives it a
        // console of its own - an elevated process cannot inherit ours.
        Platform::Windows => Elevation::Command(powershell_command(&format!(
            "Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoExit','-Command','Set-Location -LiteralPath {}') -Verb RunAs",
            target.replace('\'', "''")
        ))),
        // `-i` for a login shell, so root's own PATH and profile apply rather
        // than a half-inherited version of the user's.
        _ => Elevation::Shell(format!("cd {} && sudo -i", quote(&target, platform))),
    }
}

/// A PowerShell command, handed over base64-encoded UTF-16.
///
/// Shared with [`crate::trash`], which needs the same escape hatch for the
/// same reason: PowerShell re-parses what it is given for `-Command`, on top
/// of the C runtime quoting Rust has already applied, and a path with a quote
/// or a `$` in it survives neither. `-EncodedCommand` has no parsing left to
/// go wrong.
pub fn powershell_command(script: &str) -> Launch {
    Launch {
        program: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            base64_utf16(script),
        ],
    }
}

/// Quote for PowerShell: single quotes, in which the only special character
/// is the quote itself, doubled.
pub fn powershell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Quote for an AppleScript string literal.
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\""))
}

/// Base64 of the text as UTF-16LE, which is what `-EncodedCommand` takes.
pub fn base64_utf16(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();

    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `DISPLAY` and `XAUTHORITY`, for the `pkexec` case.
pub fn display_here() -> Option<(String, String)> {
    let display = std::env::var("DISPLAY").ok()?;
    let xauthority = std::env::var("XAUTHORITY").unwrap_or_default();
    Some((display, xauthority))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always(_: &Path) -> bool {
        true
    }
    fn never(_: &Path) -> bool {
        false
    }
    fn only(name: &'static str) -> impl Fn(&Path) -> bool {
        move |path: &Path| path.file_name().map(|n| n == name).unwrap_or(false)
    }

    fn launch() -> Launch {
        Launch {
            program: "gedit".into(),
            args: vec!["/etc/hosts".into()],
        }
    }

    /// Decode `-EncodedCommand` back to the script, so the tests assert on
    /// what PowerShell will actually run rather than on a blob.
    fn decode(encoded: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = Vec::new();
        for c in encoded.bytes().filter(|c| *c != b'=') {
            let value = ALPHABET.iter().position(|a| *a == c).unwrap() as u32;
            bits.push(value);
        }
        let mut bytes = Vec::new();
        for chunk in bits.chunks(4) {
            let mut triple = 0u32;
            for (index, value) in chunk.iter().enumerate() {
                triple |= value << (18 - 6 * index);
            }
            let taken = chunk.len() * 6 / 8;
            for index in 0..taken {
                bytes.push((triple >> (16 - 8 * index)) as u8);
            }
        }
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], *p.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn base64_of_utf16_round_trips() {
        for text in ["", "a", "ab", "abc", "abcd", "Start-Process 'x'", "héllo ☃"] {
            assert_eq!(decode(&base64_utf16(text)), text, "{text}");
        }
        // The encoding is padded to a multiple of four, as base64 must be.
        assert_eq!(base64_utf16("abc").len() % 4, 0);
    }

    #[test]
    fn windows_asks_uac_through_an_encoded_script() {
        let Elevation::Command(elevated) = elevate(Platform::Windows, &launch(), None, &never)
        else {
            panic!("expected a command")
        };
        assert_eq!(elevated.program, "powershell.exe");
        assert!(elevated.args.contains(&"-EncodedCommand".to_string()));

        let script = decode(elevated.args.last().unwrap());
        assert!(script.contains("-Verb RunAs"), "{script}");
        assert!(script.contains("-FilePath 'gedit'"), "{script}");
        assert!(script.contains("-ArgumentList '/etc/hosts'"), "{script}");
    }

    #[test]
    fn a_windows_argument_with_a_quote_in_it_survives() {
        // Two layers of parsing would otherwise mangle it: the C runtime
        // quoting Rust applies, and PowerShell's own re-parse. The encoded
        // form has neither.
        let awkward = Launch {
            program: "notepad".into(),
            args: vec![r"C:\a b\it's $weird & odd.txt".into()],
        };
        let Elevation::Command(elevated) = elevate(Platform::Windows, &awkward, None, &never)
        else {
            panic!("expected a command")
        };
        let script = decode(elevated.args.last().unwrap());
        // The quote is doubled, which is how PowerShell escapes it, and
        // everything else is inside the literal untouched.
        assert!(
            script.contains(r"'C:\a b\it''s $weird & odd.txt'"),
            "{script}"
        );
    }

    #[test]
    fn macos_asks_through_the_authentication_panel() {
        let Elevation::Command(elevated) = elevate(Platform::MacOs, &launch(), None, &never) else {
            panic!("expected a command")
        };
        assert_eq!(elevated.program, "osascript");
        let script = &elevated.args[1];
        assert!(script.starts_with("do shell script "), "{script}");
        assert!(
            script.ends_with(" with administrator privileges"),
            "{script}"
        );
        assert!(script.contains("gedit"), "{script}");
    }

    #[test]
    fn the_applescript_string_escapes_what_would_end_it() {
        let awkward = Launch {
            program: "cat".into(),
            args: vec![r#"/tmp/a "quoted" \ name"#.into()],
        };
        let Elevation::Command(elevated) = elevate(Platform::MacOs, &awkward, None, &never) else {
            panic!("expected a command")
        };
        let script = &elevated.args[1];
        // Backslash first, then quote - the other order would double-escape
        // the backslashes it had just added.
        assert!(script.contains(r#"\\"#), "{script}");
        assert!(script.contains(r#"\""#), "{script}");
        // Exactly two unescaped quotes: the ones opening and closing it.
        let bare = script.replace(r#"\""#, "");
        assert_eq!(bare.matches('"').count(), 2, "{script}");
    }

    #[test]
    fn linux_prefers_the_graphical_prompt() {
        let Elevation::Command(elevated) = elevate(
            Platform::Linux,
            &launch(),
            Some((":0", "/home/u/.Xauthority")),
            &always,
        ) else {
            panic!("expected a command")
        };
        assert_eq!(elevated.program, "pkexec");
        // pkexec strips the environment, so a graphical program would lose
        // the display it was about to draw on.
        assert_eq!(elevated.args[0], "env");
        assert!(elevated.args.contains(&"DISPLAY=:0".to_string()));
        assert!(elevated
            .args
            .contains(&"XAUTHORITY=/home/u/.Xauthority".to_string()));
        assert!(elevated.args.contains(&"gedit".to_string()));
        assert!(elevated.args.contains(&"/etc/hosts".to_string()));
    }

    #[test]
    fn a_second_choice_asker_is_used_as_it_comes() {
        // kdesu passes the environment itself, so it gets no `env` prefix.
        let Elevation::Command(elevated) =
            elevate(Platform::Linux, &launch(), Some((":0", "")), &only("kdesu"))
        else {
            panic!("expected a command")
        };
        assert_eq!(elevated.program, "kdesu");
        assert_eq!(elevated.args, vec!["gedit", "/etc/hosts"]);
    }

    #[test]
    fn with_no_graphical_prompt_it_goes_to_a_shell() {
        // `sudo` needs somewhere to ask for the password, and there are real
        // terminals in this window.
        let Elevation::Shell(line) = elevate(Platform::Linux, &launch(), None, &never) else {
            panic!("expected a shell line")
        };
        assert_eq!(line, "sudo gedit /etc/hosts");
    }

    #[test]
    fn a_shell_line_quotes_its_arguments() {
        let awkward = Launch {
            program: "vim".into(),
            args: vec!["/tmp/my notes; rm -rf x".into()],
        };
        let Elevation::Shell(line) = elevate(Platform::Linux, &awkward, None, &never) else {
            panic!("expected a shell line")
        };
        // The semicolon is inside the quotes, so it is part of the file name
        // rather than the start of another command.
        assert_eq!(line, "sudo vim '/tmp/my notes; rm -rf x'");
    }

    #[test]
    fn editing_as_root_does_not_run_the_editor_as_root() {
        // sudoedit copies out, runs the editor as you, and writes back as
        // root - so no editor plugin runs privileged and nothing root-owned
        // lands in the user's home directory.
        let Elevation::Shell(line) = edit_as_root(Platform::Linux, "vim", Path::new("/etc/hosts"))
        else {
            panic!("expected a shell line")
        };
        assert!(line.contains("sudoedit"), "{line}");
        assert!(!line.contains("sudo vim"), "{line}");
        assert!(line.contains("/etc/hosts"), "{line}");
        assert!(line.contains("SUDO_EDITOR=vim"), "{line}");
    }

    #[test]
    fn windows_has_no_sudoedit_so_it_elevates_the_editor() {
        let Elevation::Command(elevated) =
            edit_as_root(Platform::Windows, "notepad", Path::new(r"C:\Windows\hosts"))
        else {
            panic!("expected a command")
        };
        let script = decode(elevated.args.last().unwrap());
        assert!(script.contains("-Verb RunAs"), "{script}");
        assert!(script.contains("notepad"), "{script}");
    }

    #[test]
    fn a_root_shell_starts_where_the_panel_is() {
        let Elevation::Shell(line) = root_shell(Platform::Linux, Path::new("/etc/apt")) else {
            panic!("expected a shell line")
        };
        assert!(line.contains("cd /etc/apt"), "{line}");
        assert!(line.contains("sudo -i"), "{line}");

        let Elevation::Command(elevated) = root_shell(Platform::Windows, Path::new(r"C:\Windows"))
        else {
            panic!("expected a command")
        };
        let script = decode(elevated.args.last().unwrap());
        assert!(script.contains("-Verb RunAs"), "{script}");
        assert!(
            script.contains(r"Set-Location -LiteralPath C:\Windows"),
            "{script}"
        );
    }

    #[test]
    fn a_directory_with_a_quote_in_it_survives_the_root_shell() {
        let Elevation::Shell(line) = root_shell(Platform::Linux, Path::new("/tmp/it's here"))
        else {
            panic!("expected a shell line")
        };
        assert!(line.contains(r"'/tmp/it'\''s here'"), "{line}");
    }
}
