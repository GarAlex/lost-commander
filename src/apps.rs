//! Which applications could open a file - the list behind "Open with...".
//!
//! [`open`](crate::open) answers "what does the desktop do with this by
//! default". This answers the other half: what *else* could, and how to start
//! the one that was picked.
//!
//! Where the operating system already has a chooser of its own, this defers to
//! it rather than shipping a worse copy - see [`chooser_command`]. Elsewhere
//! the list is assembled from what the system publishes about its
//! applications, which on Linux is a directory of `.desktop` files and on
//! macOS is a directory of bundles.

use std::path::{Path, PathBuf};

use crate::mount::Platform;
use crate::open::Launch;
use crate::preview::{mime_for, program_exists};

/// An application offered in the chooser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    /// What the chooser shows.
    pub name: String,
    /// How to start it: a desktop entry's `Exec` line on Linux, a bundle name
    /// on macOS, or whatever was typed into the chooser's own box.
    pub exec: String,
    /// It says it handles this file's type, so it is offered first.
    pub handles: bool,
    /// It wants a terminal (`Terminal=true`), so it goes to a shell tab
    /// rather than being started with its output thrown away.
    pub terminal: bool,
}

/// A parsed `.desktop` file, before it becomes a chooser entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    pub exec: String,
    pub try_exec: Option<String>,
    pub mime_types: Vec<String>,
    pub terminal: bool,
}

/// Parse a desktop entry. `None` if it is not an application worth offering.
///
/// Only the plain `Name=` is read: `Name[de]=` and friends are the same field
/// in other languages, and picking one at random would be worse than the one
/// the file itself calls canonical.
pub fn parse_desktop_entry(text: &str) -> Option<DesktopEntry> {
    let mut in_section = false;
    let (mut name, mut exec, mut try_exec) = (None, None, None);
    let mut mime_types = Vec::new();
    let (mut terminal, mut hidden, mut is_application) = (false, false, false);

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_section || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "TryExec" => try_exec = Some(value.to_string()),
            "Type" => is_application = value == "Application",
            "Terminal" => terminal = value == "true",
            // Either one means "do not show this in a menu", and a chooser is
            // a menu: these are the entries that exist to be launched by
            // something else, not picked by a person.
            "NoDisplay" | "Hidden" => hidden |= value == "true",
            "MimeType" => {
                mime_types = value
                    .split(';')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    if hidden || !is_application {
        return None;
    }
    Some(DesktopEntry {
        name: name?,
        exec: exec?,
        try_exec,
        mime_types,
        terminal,
    })
}

/// Split an `Exec` line into arguments.
///
/// The desktop entry spec gives `Exec` its own quoting: double quotes group,
/// and inside them a backslash escapes the next character. Splitting on
/// whitespace instead would break every application installed under a path
/// with a space in it, which on macOS is most of them.
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars();

    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                '"' => quoted = false,
                _ => current.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                started = true;
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// Field codes that stand for the file, and are replaced by it.
const FILE_CODES: &[&str] = &["%f", "%F", "%u", "%U"];

/// Field codes a launcher is required to drop: the icon and name it would
/// pass to a menu, and the deprecated ones that mean nothing now.
const DROPPED_CODES: &[&str] = &["%i", "%c", "%k", "%d", "%D", "%n", "%N", "%v", "%m"];

/// Turn an `Exec` line into a command that opens `path`.
///
/// An entry with no field code still gets the file appended. Plenty of them
/// omit it, and a chooser that started the application on nothing would look
/// like it had failed.
pub fn exec_command(exec: &str, path: &Path) -> Option<Launch> {
    let target = path.display().to_string();
    let mut tokens = split_exec(exec).into_iter();
    let program = tokens.next()?;
    if program.is_empty() {
        return None;
    }

    let mut args = Vec::new();
    let mut took_file = false;
    for token in tokens {
        if DROPPED_CODES.contains(&token.as_str()) {
            continue;
        }
        match FILE_CODES.iter().find(|code| token.contains(**code)) {
            Some(code) => {
                args.push(token.replace(code, &target));
                took_file = true;
            }
            None => args.push(token.replace("%%", "%")),
        }
    }
    if !took_file {
        args.push(target);
    }
    Some(Launch { program, args })
}

/// The applications to offer for a file, best first.
///
/// The ones that claim its type come first and are flagged, then everything
/// else by name. A chooser that showed *only* the registered handlers would
/// be missing the case it exists for - the file whose type nothing claims, or
/// claims wrongly.
pub fn rank(
    entries: Vec<DesktopEntry>,
    mime: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Application> {
    let mut apps: Vec<Application> = entries
        .into_iter()
        // An entry may name a binary that was never installed - the same
        // split-package case the thumbnailers have.
        .filter(|entry| match &entry.try_exec {
            Some(program) => program_exists(program, exists),
            None => true,
        })
        .map(|entry| {
            let handles = mime
                .map(|mime| entry.mime_types.iter().any(|m| m == mime))
                .unwrap_or(false);
            Application {
                name: entry.name,
                exec: entry.exec,
                handles,
                terminal: entry.terminal,
            }
        })
        .collect();

    apps.sort_by(|a, b| {
        b.handles
            .cmp(&a.handles)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps.dedup_by(|a, b| a.name == b.name && a.exec == b.exec);
    apps
}

/// The applications whose name contains `typed`, in the order they were in.
///
/// An empty box matches everything, so the list starts complete rather than
/// starting empty and daring you to guess.
pub fn matching<'a>(applications: &'a [Application], typed: &str) -> Vec<&'a Application> {
    let needle = typed.trim().to_lowercase();
    applications
        .iter()
        .filter(|app| needle.is_empty() || app.name.to_lowercase().contains(&needle))
        .collect()
}

/// What the chooser will do, given what has been typed and where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen<'a> {
    /// Start this application.
    App(&'a Application),
    /// Nothing in the list matched, so what was typed is a command.
    Command(&'a str),
}

/// Resolve the chooser to one thing to do.
///
/// One box does both jobs: it narrows the list while anything matches, and
/// becomes a command line when nothing does. That is a rule you can discover
/// by typing rather than a mode you have to know about first - and the
/// "run something not on the list" case is exactly the one a chooser built
/// only from installed applications cannot otherwise reach.
pub fn choice<'a>(
    applications: &'a [Application],
    typed: &'a str,
    cursor: usize,
) -> Option<Chosen<'a>> {
    let matches = matching(applications, typed);
    match matches.len() {
        0 => {
            let typed = typed.trim();
            (!typed.is_empty()).then_some(Chosen::Command(typed))
        }
        len => Some(Chosen::App(matches[cursor.min(len - 1)])),
    }
}

/// Where `.desktop` files live, most specific first.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("applications"));
    }
    if let Ok(data_dirs) = std::env::var("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&data_dirs) {
            dirs.push(dir.join("applications"));
        }
    }
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Read every desktop entry in `dirs`.
pub fn load_desktop_entries(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(parsed) = parse_desktop_entry(&text) {
                found.push(parsed);
            }
        }
    }
    found
}

/// Where macOS keeps applications.
pub fn mac_application_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// The `.app` bundles in `dirs`, as chooser entries.
///
/// macOS publishes which application *handles* a type through LaunchServices,
/// which is a C API rather than anything on disk - so this offers the bundles
/// plainly, by name, and lets `open -a` do the rest. The default handler is
/// already one keystroke away on `Enter`; this list is for the other one.
pub fn load_mac_applications(dirs: &[PathBuf]) -> Vec<Application> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let Some(name) = path.file_stem().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            found.push(Application {
                exec: name.clone(),
                name,
                handles: false,
                terminal: false,
            });
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found.dedup_by(|a, b| a.name == b.name);
    found
}

/// Everything that could open this file, best first. Empty where the system
/// has a chooser of its own - see [`chooser_command`].
pub fn applications_for(path: &Path) -> Vec<Application> {
    match Platform::current() {
        Platform::MacOs => load_mac_applications(&mac_application_dirs()),
        Platform::Windows => Vec::new(),
        Platform::Linux => rank(
            load_desktop_entries(&application_dirs()),
            mime_for(path),
            &crate::preview::on_disk,
        ),
    }
}

/// The system's own "Open with" chooser, where there is one.
///
/// Windows has had this dialog since the nineties and it does more than a
/// list of names can: it offers "always use this", it knows about the Store,
/// and it is the dialog the user already recognises. Reaching it is one call,
/// so the chooser on Windows *is* that dialog.
pub fn chooser_command(platform: Platform, path: &Path) -> Option<Launch> {
    match platform {
        Platform::Windows => Some(Launch {
            program: "rundll32.exe".into(),
            args: vec![
                "shell32.dll,OpenAs_RunDLL".into(),
                path.display().to_string(),
            ],
        }),
        _ => None,
    }
}

/// The command that starts `app` on `path`.
pub fn open_with_command(platform: Platform, app: &Application, path: &Path) -> Option<Launch> {
    match platform {
        // `open -a` takes the application by name and hands it the file,
        // which is what a double-click in Finder resolves to anyway.
        Platform::MacOs => Some(Launch {
            program: "open".into(),
            args: vec!["-a".into(), app.exec.clone(), path.display().to_string()],
        }),
        _ => exec_command(&app.exec, path),
    }
}

/// The shell line for an application that wants a terminal.
///
/// These get a shell tab rather than being started with their output thrown
/// away - the same route `F4` takes to `$EDITOR`, and for the same reason:
/// there are real terminals in this window, so a terminal application has
/// somewhere to go.
pub fn terminal_line(app: &Application, path: &Path) -> String {
    let mut line = String::new();
    for (index, token) in exec_tokens(&app.exec, path).into_iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(&crate::shell::quote_here(&token));
    }
    line
}

/// Program and arguments as one list, for the terminal case.
fn exec_tokens(exec: &str, path: &Path) -> Vec<String> {
    match exec_command(exec, path) {
        Some(launch) => {
            let mut tokens = vec![launch.program];
            tokens.extend(launch.args);
            tokens
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always(_: &Path) -> bool {
        true
    }

    #[test]
    fn a_desktop_entry_becomes_a_chooser_item() {
        let entry = parse_desktop_entry(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=GNU Image Manipulation Program\n\
             Name[de]=GNU-Bildbearbeitungsprogramm\n\
             Exec=gimp-2.10 %U\n\
             TryExec=gimp-2.10\n\
             Terminal=false\n\
             MimeType=image/png;image/jpeg;\n",
        )
        .unwrap();

        assert_eq!(entry.name, "GNU Image Manipulation Program");
        assert_eq!(entry.exec, "gimp-2.10 %U");
        assert_eq!(entry.try_exec.as_deref(), Some("gimp-2.10"));
        assert_eq!(entry.mime_types, vec!["image/png", "image/jpeg"]);
        assert!(!entry.terminal);
    }

    #[test]
    fn entries_not_meant_for_a_menu_are_left_out() {
        // NoDisplay and Hidden both mean "not for people to pick".
        for line in ["NoDisplay=true", "Hidden=true"] {
            let text = format!("[Desktop Entry]\nType=Application\nName=X\nExec=x %f\n{line}\n");
            assert!(parse_desktop_entry(&text).is_none(), "{line}");
        }
        // A link or a directory entry is not something to open a file with.
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Link\nName=X\nExec=x\n").is_none());
        // ...and neither is one with nothing to run.
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName=X\n").is_none());
    }

    #[test]
    fn keys_outside_the_desktop_entry_section_are_ignored() {
        let entry = parse_desktop_entry(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Files\n\
             Exec=nautilus %U\n\
             [Desktop Action Window]\n\
             Name=New Window\n\
             Exec=nautilus --new-window\n",
        )
        .unwrap();
        // The action's Name and Exec must not overwrite the entry's own.
        assert_eq!(entry.name, "Files");
        assert_eq!(entry.exec, "nautilus %U");
    }

    #[test]
    fn exec_quoting_survives_a_path_with_spaces() {
        let tokens = split_exec(r#""/opt/My App/bin/run" --flag "a b" plain"#);
        assert_eq!(
            tokens,
            vec!["/opt/My App/bin/run", "--flag", "a b", "plain"]
        );
        // A backslash inside quotes escapes the next character.
        assert_eq!(split_exec(r#""a\"b" c"#), vec![r#"a"b"#, "c"]);
    }

    #[test]
    fn the_field_code_becomes_the_file() {
        for code in ["%f", "%F", "%u", "%U"] {
            let launch = exec_command(&format!("viewer {code}"), Path::new("/tmp/a.png")).unwrap();
            assert_eq!(launch.program, "viewer");
            assert_eq!(launch.args, vec!["/tmp/a.png".to_string()], "{code}");
        }
        // Embedded in an argument, not only standing alone.
        let launch = exec_command("app --file=%f --gui", Path::new("/tmp/a.png")).unwrap();
        assert_eq!(launch.args, vec!["--file=/tmp/a.png", "--gui"]);
    }

    #[test]
    fn the_codes_a_launcher_must_drop_are_dropped() {
        let launch = exec_command("app %i %c %k %d %v %f", Path::new("/tmp/a.png")).unwrap();
        assert_eq!(launch.args, vec!["/tmp/a.png".to_string()]);
        // A literal percent survives as one.
        let launch = exec_command("app 100%% %f", Path::new("/tmp/a.png")).unwrap();
        assert_eq!(launch.args, vec!["100%".to_string(), "/tmp/a.png".into()]);
    }

    #[test]
    fn an_exec_line_with_no_field_code_still_gets_the_file() {
        // Plenty of entries omit it, and starting the application on nothing
        // would look like the chooser had failed.
        let launch = exec_command("mousepad", Path::new("/tmp/a.txt")).unwrap();
        assert_eq!(launch.program, "mousepad");
        assert_eq!(launch.args, vec!["/tmp/a.txt".to_string()]);
    }

    #[test]
    fn an_empty_exec_line_is_not_a_command() {
        assert!(exec_command("", Path::new("/tmp/a.txt")).is_none());
        assert!(exec_command("   ", Path::new("/tmp/a.txt")).is_none());
    }

    fn entry(name: &str, mimes: &[&str]) -> DesktopEntry {
        DesktopEntry {
            name: name.into(),
            exec: format!("{} %f", name.to_lowercase()),
            try_exec: None,
            mime_types: mimes.iter().map(|m| (*m).to_string()).collect(),
            terminal: false,
        }
    }

    #[test]
    fn the_applications_that_claim_the_type_come_first() {
        let entries = vec![
            entry("Zed", &[]),
            entry("Eye of GNOME", &["image/png"]),
            entry("Archive Manager", &[]),
            entry("GIMP", &["image/png", "image/jpeg"]),
        ];
        let apps = rank(entries, Some("image/png"), &always);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();

        // Handlers first, each group alphabetical.
        assert_eq!(
            names,
            vec!["Eye of GNOME", "GIMP", "Archive Manager", "Zed"]
        );
        assert!(apps[0].handles && apps[1].handles);
        assert!(!apps[2].handles && !apps[3].handles);
    }

    #[test]
    fn a_text_file_finds_its_handlers() {
        // The commonest case there is, and the one that was silently broken:
        // `mime_for` had no entry for `.txt`, so nothing claimed it and the
        // chooser offered an alphabetical list with Archive Manager on top.
        let entries = vec![
            entry("Archive Manager", &["application/zip"]),
            entry("Text Editor", &["text/plain"]),
        ];
        let apps = rank(entries, mime_for(Path::new("notes.txt")), &always);
        assert_eq!(apps[0].name, "Text Editor");
        assert!(apps[0].handles);
        assert!(!apps[1].handles);
    }

    #[test]
    fn everything_is_still_offered_when_nothing_claims_the_type() {
        // The case the chooser exists for: a file whose type nothing handles.
        let entries = vec![entry("Zed", &[]), entry("GIMP", &["image/png"])];
        let apps = rank(entries, None, &always);
        assert_eq!(apps.len(), 2);
        assert!(apps.iter().all(|a| !a.handles));
    }

    #[test]
    fn an_entry_whose_binary_was_never_installed_is_left_out() {
        // The same split-package case the thumbnailers have: the `.desktop`
        // file ships in one package and the program in another.
        let mut installed = entry("Present", &[]);
        installed.try_exec = Some("present".into());
        let mut missing = entry("Absent", &[]);
        missing.try_exec = Some("absent".into());

        let exists = |path: &Path| path.file_name().unwrap().to_string_lossy() == "present";
        let apps = rank(vec![installed, missing], None, &exists);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Present");
    }

    #[test]
    fn the_same_application_from_two_directories_is_listed_once() {
        // ~/.local/share/applications shadowing /usr/share/applications is
        // the normal case, not an error.
        let apps = rank(vec![entry("GIMP", &[]), entry("GIMP", &[])], None, &always);
        assert_eq!(apps.len(), 1);
    }

    fn listed() -> Vec<Application> {
        rank(
            vec![
                entry("Eye of GNOME", &["image/png"]),
                entry("GIMP", &["image/png"]),
                entry("Text Editor", &[]),
            ],
            Some("image/png"),
            &always,
        )
    }

    #[test]
    fn the_box_narrows_the_list_as_you_type() {
        let apps = listed();
        assert_eq!(matching(&apps, "").len(), 3);
        // Case-insensitive, and anywhere in the name rather than only at the
        // start - "gnome" should find "Eye of GNOME".
        let found = matching(&apps, "gnome");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Eye of GNOME");
        assert_eq!(matching(&apps, "  gimp  ").len(), 1);
    }

    #[test]
    fn enter_takes_the_application_under_the_cursor() {
        let apps = listed();
        // Empty box: the first one, which is a handler for the type.
        assert_eq!(choice(&apps, "", 0), Some(Chosen::App(&apps[0])));
        assert_eq!(choice(&apps, "", 2), Some(Chosen::App(&apps[2])));
        // A cursor left beyond a narrowed list still lands on something.
        assert_eq!(choice(&apps, "gimp", 7), Some(Chosen::App(&apps[1])));
    }

    #[test]
    fn text_that_matches_nothing_is_a_command_to_run() {
        // The case a list built from installed applications cannot otherwise
        // reach: something not on it.
        let apps = listed();
        assert_eq!(
            choice(&apps, "hexdump -C", 0),
            Some(Chosen::Command("hexdump -C"))
        );
        // ...and an empty box with no applications at all does nothing.
        assert_eq!(choice(&[], "", 0), None);
        assert_eq!(choice(&[], "  ", 0), None);
        assert_eq!(choice(&[], "vim", 0), Some(Chosen::Command("vim")));
    }

    #[test]
    fn windows_uses_its_own_open_with_dialog() {
        let launch = chooser_command(Platform::Windows, Path::new(r"C:\a b\r&d.txt")).unwrap();
        assert_eq!(launch.program, "rundll32.exe");
        assert_eq!(launch.args[0], "shell32.dll,OpenAs_RunDLL");
        assert_eq!(launch.args[1], r"C:\a b\r&d.txt");
        // Everywhere else the chooser is ours.
        assert!(chooser_command(Platform::Linux, Path::new("/tmp/a")).is_none());
        assert!(chooser_command(Platform::MacOs, Path::new("/tmp/a")).is_none());
    }

    #[test]
    fn macos_starts_the_bundle_by_name() {
        let app = Application {
            name: "Preview".into(),
            exec: "Preview".into(),
            handles: false,
            terminal: false,
        };
        let launch = open_with_command(Platform::MacOs, &app, Path::new("/tmp/a b.png")).unwrap();
        assert_eq!(launch.program, "open");
        assert_eq!(launch.args, vec!["-a", "Preview", "/tmp/a b.png"]);
    }

    #[test]
    fn linux_starts_the_exec_line() {
        let app = Application {
            name: "GIMP".into(),
            exec: "gimp-2.10 %U".into(),
            handles: true,
            terminal: false,
        };
        let launch = open_with_command(Platform::Linux, &app, Path::new("/tmp/a.png")).unwrap();
        assert_eq!(launch.program, "gimp-2.10");
        assert_eq!(launch.args, vec!["/tmp/a.png".to_string()]);
    }

    #[test]
    fn a_terminal_application_becomes_a_shell_line() {
        let app = Application {
            name: "Vim".into(),
            exec: "vim %F".into(),
            handles: true,
            terminal: true,
        };
        // Quoted for the shell, since this one is typed at a prompt rather
        // than spawned with an argument list.
        let line = terminal_line(&app, Path::new("/tmp/my notes.txt"));
        assert!(line.starts_with("vim "), "{line}");
        assert!(line.contains("my notes.txt"), "{line}");
        assert_ne!(line, "vim /tmp/my notes.txt", "the space was not quoted");
    }

    #[test]
    fn desktop_entries_are_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("good.desktop"),
            "[Desktop Entry]\nType=Application\nName=Good\nExec=good %f\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=x\nNoDisplay=true\n",
        )
        .unwrap();
        // Not a desktop file at all, and must not stop the walk.
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();

        let found = load_desktop_entries(&[dir.path().to_path_buf(), "/no/such/dir".into()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Good");
    }

    #[test]
    fn mac_bundles_are_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Preview.app")).unwrap();
        std::fs::create_dir(dir.path().join("Xcode.app")).unwrap();
        std::fs::write(dir.path().join("README"), "hello").unwrap();

        let found = load_mac_applications(&[dir.path().to_path_buf(), "/no/such/dir".into()]);
        let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Preview", "Xcode"]);
        assert_eq!(found[0].exec, "Preview");
    }
}
