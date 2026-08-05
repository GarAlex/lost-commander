// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Deleting to the system's trash, so a wrong keystroke is recoverable.
//!
//! Every platform already has one, with its own rules about where it lives and
//! what has to be recorded for a file to be put back. None of them is "move it
//! to a folder called Trash", and getting it wrong produces files the desktop
//! shows but cannot restore.
//!
//! * **Linux** follows the freedesktop.org trash specification: a `files/` and
//!   an `info/` directory, a `.trashinfo` written *before* the file is moved,
//!   and a per-volume trash for anything not on the same filesystem as home.
//! * **macOS** asks Finder, which is what owns the "Put Back" record.
//! * **Windows** goes through the shell's own recycle-bin call.
//!
//! The one rule that matters more than any of it: if trashing fails, nothing
//! is deleted. A trash that silently falls back to `rm` is worse than no trash
//! at all, because it is the fallback the user was relying on not to happen.

use std::io;
use std::path::{Path, PathBuf};

use crate::mount::Platform;
use crate::open::Launch;

/// Where the freedesktop trash keeps the files and their records.
pub const FILES: &str = "files";
pub const INFO: &str = "info";

/// Percent-encode a path for a `.trashinfo` `Path=` line.
///
/// The spec stores the original location as a URI without its scheme, so
/// everything outside the unreserved set is encoded - `/` excepted, since it
/// is the separator rather than data.
pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The body of a `.trashinfo` file.
///
/// `deleted_at` is a parameter rather than read from the clock, so the format
/// is testable and the caller decides what "now" means.
pub fn trash_info(original: &Path, deleted_at: &str) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        url_encode(&original.display().to_string()),
        deleted_at
    )
}

/// The inverse of [`url_encode`], for reading a trashinfo back.
pub fn url_decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let pair: String = chars.by_ref().take(2).collect();
        match u8::from_str_radix(&pair, 16) {
            Ok(byte) => out.push(byte as char),
            // Not an escape after all: kept as written, because a name with
            // a stray percent in it is still a name.
            Err(_) => {
                out.push('%');
                out.push_str(&pair);
            }
        }
    }
    out
}

/// One thing in the trash: what it was, where it came from, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    pub name: String,
    pub original: PathBuf,
    pub deleted_at: String,
    /// How to reach it where it sits - the name inside `files/` on the XDG
    /// side, the item's own path in the bin on Windows.
    pub token: String,
}

/// What an XDG trash directory holds, newest first by deletion date.
pub fn list_at(trash_dir: &Path) -> Vec<TrashedItem> {
    let Ok(entries) = std::fs::read_dir(trash_dir.join("info")) else {
        return Vec::new();
    };
    let mut items: Vec<TrashedItem> = entries
        .flatten()
        .filter_map(|entry| {
            let info_name = entry.file_name().to_string_lossy().to_string();
            let token = info_name.strip_suffix(".trashinfo")?.to_string();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let mut original = None;
            let mut deleted_at = String::new();
            for line in text.lines() {
                if let Some(path) = line.strip_prefix("Path=") {
                    original = Some(PathBuf::from(url_decode(path)));
                }
                if let Some(when) = line.strip_prefix("DeletionDate=") {
                    deleted_at = when.to_string();
                }
            }
            Some(TrashedItem {
                name: token.clone(),
                original: original?,
                deleted_at,
                token,
            })
        })
        .collect();
    items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    items
}

/// Put one thing back where it came from.
///
/// Refuses when the original seat is taken: silently replacing a file that
/// exists now would turn a restore into an overwrite nobody asked for. The
/// parent directories are remade if they have gone - the file's home coming
/// back with it is what "restore" means.
pub fn restore_at(trash_dir: &Path, item: &TrashedItem) -> io::Result<()> {
    if item.original.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "something else is where it came from",
        ));
    }
    if let Some(parent) = item.original.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(trash_dir.join("files").join(&item.token), &item.original)?;
    let _ = std::fs::remove_file(
        trash_dir
            .join("info")
            .join(format!("{}.trashinfo", item.token)),
    );
    Ok(())
}

/// Remove one thing from the trash for good.
pub fn purge_at(trash_dir: &Path, item: &TrashedItem) -> io::Result<()> {
    let path = trash_dir.join("files").join(&item.token);
    if path.is_dir() {
        std::fs::remove_dir_all(&path)?;
    } else {
        std::fs::remove_file(&path)?;
    }
    let _ = std::fs::remove_file(
        trash_dir
            .join("info")
            .join(format!("{}.trashinfo", item.token)),
    );
    Ok(())
}

/// The script that lists the Windows Recycle Bin, one item per line as
/// `path-in-bin|original-full-path|date`.
///
/// Column 1 of the bin's details is the original location and column 2 the
/// deletion date - indexes, not names, so this does not depend on the
/// display language.
pub fn list_bin_script() -> String {
    "$shell = New-Object -ComObject Shell.Application; \
     $bin = $shell.Namespace(10); \
     foreach ($item in $bin.Items()) { \
       $where = $bin.GetDetailsOf($item, 1); \
       $when = $bin.GetDetailsOf($item, 2); \
       Write-Output ($item.Path + '|' + (Join-Path $where $item.Name) + '|' + $when) \
     }"
    .to_string()
}

/// Parse what [`list_bin_script`] printed.
pub fn parse_bin_listing(stdout: &str) -> Vec<TrashedItem> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(3, '|');
            let token = parts.next()?.trim().to_string();
            let original = PathBuf::from(parts.next()?.trim());
            let deleted_at = parts.next().unwrap_or("").trim().to_string();
            if token.is_empty() || original.as_os_str().is_empty() {
                return None;
            }
            let name = original
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| token.clone());
            Some(TrashedItem {
                name,
                original,
                deleted_at,
                token,
            })
        })
        .collect()
}

/// The script that puts one bin item back, refusing a taken seat.
pub fn restore_bin_script(item: &TrashedItem) -> String {
    let token = crate::elevate::powershell_quote(&item.token);
    let original = crate::elevate::powershell_quote(&item.original.display().to_string());
    format!(
        "if (Test-Path -LiteralPath {original}) {{ \
           Write-Error 'something else is where it came from' \
         }} else {{ \
           $parent = Split-Path -Parent {original}; \
           if ($parent) {{ New-Item -ItemType Directory -Force $parent | Out-Null }}; \
           Move-Item -LiteralPath {token} -Destination {original} -ErrorAction Stop \
         }}"
    )
}

/// The script that removes one bin item for good.
pub fn purge_bin_script(item: &TrashedItem) -> String {
    let token = crate::elevate::powershell_quote(&item.token);
    format!("Remove-Item -LiteralPath {token} -Recurse -Force -ErrorAction Stop")
}

/// Everything in the trash, wherever this platform keeps it.
pub fn list() -> Vec<TrashedItem> {
    if cfg!(windows) {
        let launch = crate::elevate::powershell_command(&list_bin_script());
        let Ok(output) = std::process::Command::new(&launch.program)
            .args(&launch.args)
            .output()
        else {
            return Vec::new();
        };
        parse_bin_listing(&String::from_utf8_lossy(&output.stdout))
    } else {
        home_trash().map(|dir| list_at(&dir)).unwrap_or_default()
    }
}

fn run_bin_script(script: &str) -> io::Result<()> {
    let launch = crate::elevate::powershell_command(script);
    let output = std::process::Command::new(&launch.program)
        .args(&launch.args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("the shell refused")
                .to_string(),
        ))
    }
}

/// Put one thing back, wherever this platform keeps its trash.
pub fn restore(item: &TrashedItem) -> io::Result<()> {
    if cfg!(windows) {
        run_bin_script(&restore_bin_script(item))
    } else {
        let dir = home_trash().ok_or_else(|| io::Error::other("no trash directory"))?;
        restore_at(&dir, item)
    }
}

/// Remove one thing for good, wherever this platform keeps its trash.
pub fn purge(item: &TrashedItem) -> io::Result<()> {
    if cfg!(windows) {
        run_bin_script(&purge_bin_script(item))
    } else {
        let dir = home_trash().ok_or_else(|| io::Error::other("no trash directory"))?;
        purge_at(&dir, item)
    }
}

/// Local time in the form the spec asks for.
pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// A name not already taken in the trash.
///
/// Two files called `notes.txt` deleted from different directories both want
/// the same slot, so the second gets a number - and it goes before the
/// extension, which is what the desktops do and what keeps the file openable
/// after it is restored.
pub fn unique_name(name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    for number in 2..10_000 {
        let candidate = format!("{stem}.{number}{extension}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    // Ten thousand collisions is not a case worth a better answer than "put
    // it somewhere, do not lose it".
    format!("{stem}.{}{extension}", std::process::id())
}

/// The home trash directory, per `$XDG_DATA_HOME`.
pub fn home_trash() -> Option<PathBuf> {
    dirs::data_dir().map(|data| data.join("Trash"))
}

/// The mount point `path` is on.
///
/// Walks up until the device number changes, which is what a mount point *is*.
/// Needed because a file on another filesystem cannot be renamed into the home
/// trash, and the spec puts it in a trash on its own volume instead.
#[cfg(unix)]
pub fn top_dir(path: &Path) -> PathBuf {
    use std::os::unix::fs::MetadataExt;

    let Ok(start) = path.metadata() else {
        return PathBuf::from("/");
    };
    let device = start.dev();
    let mut best = path.to_path_buf();
    let mut current = path;
    while let Some(parent) = current.parent() {
        match parent.metadata() {
            Ok(meta) if meta.dev() == device => {
                best = parent.to_path_buf();
                current = parent;
            }
            _ => break,
        }
    }
    best
}

#[cfg(not(unix))]
pub fn top_dir(path: &Path) -> PathBuf {
    path.ancestors().last().unwrap_or(path).to_path_buf()
}

/// Our user id, for the per-volume trash directory name.
#[cfg(unix)]
fn uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    dirs::home_dir()
        .and_then(|home| home.metadata().ok())
        .map(|meta| meta.uid())
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

/// The trash to use for something on `topdir`, creating it if need be.
///
/// `$topdir/.Trash/$uid` is only used when the administrator has set one up
/// and made it sticky - the spec is careful about this, because an
/// unprotected shared `.Trash` lets one user delete another's files out of
/// it. Otherwise each user gets `$topdir/.Trash-$uid`, which is theirs.
#[cfg(unix)]
pub fn volume_trash(topdir: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let shared = topdir.join(".Trash");
    if let Ok(meta) = shared.symlink_metadata() {
        let sticky = meta.permissions().mode() & 0o1000 != 0;
        if meta.is_dir() && !meta.file_type().is_symlink() && sticky {
            let mine = shared.join(uid().to_string());
            std::fs::create_dir_all(mine.join(FILES))?;
            std::fs::create_dir_all(mine.join(INFO))?;
            return Ok(mine);
        }
    }
    let mine = topdir.join(format!(".Trash-{}", uid()));
    std::fs::create_dir_all(mine.join(FILES))?;
    std::fs::create_dir_all(mine.join(INFO))?;
    Ok(mine)
}

#[cfg(not(unix))]
pub fn volume_trash(topdir: &Path) -> io::Result<PathBuf> {
    let mine = topdir.join(format!(".Trash-{}", uid()));
    std::fs::create_dir_all(mine.join(FILES))?;
    std::fs::create_dir_all(mine.join(INFO))?;
    Ok(mine)
}

/// Move `path` into the freedesktop trash directory `trash`.
///
/// The `.trashinfo` is written **first**, and with `create_new`, which is what
/// makes the name reservation atomic: two of these racing cannot both think
/// they own the same slot. A record with no file is tidy-uppable; a file with
/// no record is an orphan the desktop will not restore.
///
/// `relative_to` is the volume's top directory when this is a volume trash,
/// in which case the recorded path is relative to it - so the trash still
/// makes sense after the volume is mounted somewhere else.
pub fn move_into_trash(
    path: &Path,
    trash: &Path,
    relative_to: Option<&Path>,
    stamp: &str,
) -> io::Result<PathBuf> {
    let files = trash.join(FILES);
    let info = trash.join(INFO);
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;

    let original = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;

    let recorded = match relative_to {
        Some(top) => path.strip_prefix(top).unwrap_or(path).to_path_buf(),
        None => path.to_path_buf(),
    };

    // Claim a name by creating its record, and only then move the file.
    let mut name = original.clone();
    loop {
        let taken = |candidate: &str| {
            files.join(candidate).symlink_metadata().is_ok()
                || info.join(format!("{candidate}.trashinfo")).exists()
        };
        name = unique_name(&name, &taken);
        let record = info.join(format!("{name}.trashinfo"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&record)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(trash_info(&recorded, stamp).as_bytes())?;
                break;
            }
            // Somebody claimed it between the check and the create; go round.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    let destination = files.join(&name);
    match std::fs::rename(path, &destination) {
        Ok(()) => Ok(destination),
        Err(e) => {
            // Leave no record pointing at a file that is still where it was.
            let _ = std::fs::remove_file(info.join(format!("{name}.trashinfo")));
            Err(e)
        }
    }
}

/// Ask Finder to trash these, which is what owns the "Put Back" record.
///
/// Moving the file into `~/.Trash` by hand would put it in the right place
/// with none of the metadata, so the desktop would show it and refuse to
/// restore it.
pub fn macos_command(paths: &[PathBuf]) -> Launch {
    let items = paths
        .iter()
        .map(|path| {
            format!(
                "POSIX file \"{}\"",
                path.display()
                    .to_string()
                    .replace('\\', r"\\")
                    .replace('"', "\\\"")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Launch {
        program: "osascript".into(),
        args: vec![
            "-e".into(),
            format!("tell application \"Finder\" to delete {{{items}}}"),
        ],
    }
}

/// What each line of the script's report starts with.
///
/// The script says what became of every path it was given, because one exit
/// status for a whole batch cannot say *which* file would not go - and "some
/// of these failed" is the kind of answer that reads like an answer while
/// telling you nothing.
const REPORT: &str = "lostc-trash";

/// How long the generated script may get, in characters.
///
/// `-EncodedCommand` carries it as base64 of UTF-16, which is about 2.7
/// characters of command line for every character here, and Windows refuses a
/// command line over 32,767. Eight thousand is a third of what would fit,
/// which leaves room for the arguments around it and for paths that count for
/// more than their length.
const MAX_SCRIPT: usize = 8_000;

/// Roughly what the script costs before any path is put in it.
const SCRIPT_FIXED: usize = 700;

/// The shell's own recycle-bin call, reached through PowerShell.
///
/// `Microsoft.VisualBasic.FileIO.FileSystem` is the one route to it that needs
/// no COM bindings: `Remove-Item` deletes outright, and there is no PowerShell
/// cmdlet that recycles.
///
/// The paths go in as an array and the work is done by a loop, rather than the
/// statements being written out one per path. That is not tidiness: it makes
/// the cost of a path the length of the path, so a batch holds ten times as
/// many, and it gives every path the same `try` - which is what lets the
/// report below name the one that failed.
pub fn windows_command(paths: &[PathBuf]) -> Launch {
    let list = paths
        .iter()
        .map(|path| crate::elevate::powershell_quote(&path.display().to_string()))
        .collect::<Vec<_>>()
        .join(",");
    let mark = REPORT;
    // The message has its newlines squeezed out before it is printed: a
    // report is one line, and an exception that spanned two would otherwise
    // look like a report about some other path.
    let script = format!(
        "Add-Type -AssemblyName Microsoft.VisualBasic; \
         $rcmd = @({list}); \
         for ($i = 0; $i -lt $rcmd.Count; $i++) {{ \
           $p = $rcmd[$i]; \
           try {{ \
             if (Test-Path -LiteralPath $p -PathType Container) {{ \
               [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($p, \
                 'OnlyErrorDialogs', 'SendToRecycleBin') }} else {{ \
               [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p, \
                 'OnlyErrorDialogs', 'SendToRecycleBin') }}; \
             Write-Output \"{mark} $i ok\" \
           }} catch {{ \
             Write-Output (\"{mark} $i err \" + ($_.Exception.Message -replace '\\s+', ' ')) \
           }} \
         }}"
    );
    crate::elevate::powershell_command(&script)
}

/// How many of `paths` can go into one call to the system.
///
/// One, except on Windows. There, each call pays PowerShell's startup - about
/// half a second, measured - so deleting eight files one at a time took four
/// and a half seconds when the work itself is instant. The batch is bounded by
/// how long a command line Windows will accept, not by a round number.
///
/// macOS keeps one call per path deliberately. `osascript` starts in a
/// fraction of the time, so there is far less to win, and telling Finder to
/// delete a list gives back no way to say which item it refused - which would
/// trade a real answer for a fast one.
///
/// Never zero for a non-empty list: a path longer than the whole budget still
/// goes, alone, because refusing to delete a file for being long-named would
/// be worse than a command line the system may yet accept.
pub fn batch_len(paths: &[PathBuf], platform: Platform) -> usize {
    if paths.is_empty() {
        return 0;
    }
    if !matches!(platform, Platform::Windows) {
        return 1;
    }

    let mut used = SCRIPT_FIXED;
    let mut taken = 0;
    for path in paths {
        // Bytes, where the limit is really in UTF-16 units; for anything
        // outside ASCII that overestimates, and overestimating is the safe
        // direction.
        let cost = crate::elevate::powershell_quote(&path.display().to_string()).len() + 1;
        if taken > 0 && used + cost > MAX_SCRIPT {
            break;
        }
        used += cost;
        taken += 1;
    }
    taken
}

/// Read the script's report back: what became of each path, in order.
///
/// A path the report says nothing about is an error, never a success. The
/// script may have died half way through, and a file still sitting on disk
/// must not be reported as trashed - the panel would re-read the directory,
/// find it, and show a delete that plainly did not happen.
fn reports(printed: &str, count: usize, fallback: &str) -> Vec<io::Result<()>> {
    let mut out: Vec<io::Result<()>> = (0..count)
        .map(|_| Err(io::Error::other(fallback.to_string())))
        .collect();

    for line in printed.lines() {
        let Some(rest) = line.trim().strip_prefix(REPORT) else {
            continue;
        };
        let Some((index, rest)) = rest.trim_start().split_once(' ') else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };
        if index >= count {
            continue;
        }
        if let Some(message) = rest.strip_prefix("err") {
            let message = message.trim();
            out[index] = Err(io::Error::other(if message.is_empty() {
                "the system would not put it in the recycle bin".to_string()
            } else {
                message.to_string()
            }));
        } else if rest.trim() == "ok" {
            out[index] = Ok(());
        }
    }
    out
}

/// Move a batch of paths to the trash, and say what became of each.
///
/// One result per path, in the order given. Give it no more than
/// [`batch_len`] says will fit.
pub fn trash_batch(paths: &[PathBuf]) -> Vec<io::Result<()>> {
    if paths.is_empty() {
        return Vec::new();
    }
    let platform = Platform::current();
    if !delegates(platform) {
        return paths.iter().map(|path| trash_locally(path)).collect();
    }
    if matches!(platform, Platform::MacOs) {
        return paths
            .iter()
            .map(|path| run_and_wait(&macos_command(std::slice::from_ref(path))))
            .collect();
    }

    let command = windows_command(paths);
    let output = match std::process::Command::new(&command.program)
        .args(&command.args)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => output,
        // The shell would not even start. Nothing was deleted, and every path
        // has to say so rather than be left looking as though it worked.
        Err(e) => {
            let message = e.to_string();
            return paths
                .iter()
                .map(|_| Err(io::Error::new(e.kind(), message.clone())))
                .collect();
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    let fallback = if detail.is_empty() {
        format!("{} said nothing about it", command.program)
    } else {
        detail.to_string()
    };
    let printed = String::from_utf8_lossy(&output.stdout);
    reports(&printed, paths.len(), &fallback)
}

/// Whether this platform trashes by running a command rather than by moving
/// files itself.
pub fn delegates(platform: Platform) -> bool {
    matches!(platform, Platform::MacOs | Platform::Windows)
}

/// Move one path to the trash.
///
/// A batch of one, so that the single file and the many go the same way and
/// the reporting below is exercised either way.
pub fn trash(path: &Path) -> io::Result<()> {
    let one = [path.to_path_buf()];
    trash_batch(&one)
        .pop()
        .unwrap_or_else(|| Err(io::Error::other("nothing was reported")))
}

/// Move one path to the trash by moving it, which is what Linux wants: the
/// trash there is a directory with a written record beside each file, and
/// there is no system call that owns it.
fn trash_locally(path: &Path) -> io::Result<()> {
    let stamp = now_stamp();
    let home = home_trash().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no trash directory for this user")
    })?;

    // The home trash first, since that is where things belong when they can
    // get there; a file on another filesystem cannot be renamed into it.
    match move_into_trash(path, &home, None, &stamp) {
        Ok(_) => Ok(()),
        Err(_) => {
            let top = top_dir(path);
            let volume = volume_trash(&top)?;
            move_into_trash(path, &volume, Some(&top), &stamp).map(|_| ())
        }
    }
}

/// Run a trashing command and report what it said, since unlike opening a
/// file this one has to be waited for: the caller needs to know whether the
/// file is gone before it re-reads the directory.
fn run_and_wait(command: &Launch) -> io::Result<()> {
    let output = std::process::Command::new(&command.program)
        .args(&command.args)
        .stdin(std::process::Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    Err(io::Error::other(if detail.is_empty() {
        format!("{} failed", command.program)
    } else {
        detail.to_string()
    }))
}

#[cfg(test)]
mod reading_tests {
    use super::*;

    fn seeded() -> (tempfile::TempDir, PathBuf, TrashedItem) {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        let original = dir.path().join("home").join("a file.txt");
        std::fs::write(trash.join("files").join("a file.txt"), "x").unwrap();
        std::fs::write(
            trash.join("info").join("a file.txt.trashinfo"),
            trash_info(&original, "2026-08-01T10:00:00"),
        )
        .unwrap();
        let item = list_at(&trash).remove(0);
        (dir, trash, item)
    }

    #[test]
    fn the_listing_reads_back_what_deletion_wrote() {
        let (_dir, trash, item) = seeded();
        assert_eq!(item.name, "a file.txt");
        assert!(item.original.ends_with("a file.txt"));
        assert_eq!(item.deleted_at, "2026-08-01T10:00:00");
        assert_eq!(list_at(&trash).len(), 1);
        // Round trip through the encoding: the space survived.
        assert_eq!(url_decode(&url_encode("a file.txt")), "a file.txt");
    }

    #[test]
    fn restore_puts_it_back_and_refuses_a_taken_seat() {
        let (_dir, trash, item) = seeded();
        restore_at(&trash, &item).unwrap();
        assert!(item.original.exists(), "back home, home remade");
        assert!(list_at(&trash).is_empty(), "and out of the trash");

        // A second copy trashed later cannot land on the restored one.
        std::fs::write(trash.join("files").join("a file.txt"), "y").unwrap();
        std::fs::write(
            trash.join("info").join("a file.txt.trashinfo"),
            trash_info(&item.original, "2026-08-02T10:00:00"),
        )
        .unwrap();
        let again = list_at(&trash).remove(0);
        let refused = restore_at(&trash, &again).unwrap_err();
        assert!(refused.to_string().contains("where it came from"));
    }

    #[test]
    fn purge_is_for_good_and_says_so_by_leaving_nothing() {
        let (_dir, trash, item) = seeded();
        purge_at(&trash, &item).unwrap();
        assert!(list_at(&trash).is_empty());
        assert!(!trash.join("files").join("a file.txt").exists());
    }

    #[test]
    fn the_bin_scripts_speak_for_themselves() {
        let item = TrashedItem {
            name: "a.txt".into(),
            original: PathBuf::from(r"C:\src\a.txt"),
            deleted_at: String::new(),
            token: r"C:\$Recycle.Bin\S-1\x".into(),
        };
        assert!(list_bin_script().contains("Namespace(10)"));
        let restore = restore_bin_script(&item);
        assert!(
            restore.contains("Test-Path"),
            "a taken seat is refused, not replaced"
        );
        assert!(restore.contains("Move-Item"));
        assert!(purge_bin_script(&item).contains("Remove-Item"));
        let parsed = parse_bin_listing(
            "C:\\$Recycle.Bin\\S-1\\x|C:\\src\\a.txt|01.08.2026 10:00\n\nnot-a-line\n",
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "a.txt");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths for `count` real files, made in `dir`.
    fn some_files(dir: &Path, count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| {
                let path = dir.join(format!("batched-{i}.txt"));
                std::fs::write(&path, format!("file {i}")).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn a_batch_reports_on_every_path_and_takes_them_all() {
        // Real files into the real trash, because the whole point of handing
        // these to the system is the metadata only the system writes, and a
        // fake would prove nothing about it.
        let dir = tempfile::tempdir().unwrap();
        let paths = some_files(dir.path(), 3);

        let results = trash_batch(&paths);

        assert_eq!(results.len(), paths.len(), "one result per path, always");
        for (path, result) in paths.iter().zip(&results) {
            assert!(result.is_ok(), "{}: {:?}", path.display(), result);
            assert!(!path.exists(), "{} is still there", path.display());
        }
    }

    #[test]
    fn one_bad_path_in_a_batch_fails_alone() {
        // The reason the batch reports per path instead of once. A single
        // status for the whole call could only have said "something went
        // wrong", which would have condemned the two files that went
        // perfectly well and named neither.
        let dir = tempfile::tempdir().unwrap();
        let good = some_files(dir.path(), 2);
        let missing = dir.path().join("was-never-here.txt");

        let paths = vec![good[0].clone(), missing.clone(), good[1].clone()];
        let results = trash_batch(&paths);

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok(), "{:?}", results[0]);
        assert!(results[1].is_err(), "a path that is not there cannot go");
        assert!(results[2].is_ok(), "{:?}", results[2]);

        assert!(!good[0].exists(), "the first still went");
        assert!(!good[1].exists(), "and so did the last");

        // And the failure says something, so it can be shown to someone.
        let complaint = results[1].as_ref().unwrap_err().to_string();
        assert!(!complaint.trim().is_empty(), "an unexplained failure");
        // It names the file it is about, which is the whole point.
        assert!(
            complaint.contains("was-never-here"),
            "the complaint should say which file: {complaint}"
        );
    }

    #[test]
    fn a_batch_holds_many_paths_on_windows_and_one_everywhere_else() {
        let paths: Vec<PathBuf> = (0..64)
            .map(|i| PathBuf::from(format!(r"C:\Users\u\folder\file-{i}.txt")))
            .collect();

        assert!(
            batch_len(&paths, Platform::Windows) > 1,
            "the whole point is more than one per call"
        );
        // macOS could batch and deliberately does not - Finder gives back no
        // way to say which item it refused.
        assert_eq!(batch_len(&paths, Platform::MacOs), 1);
        // Linux never gets here; it moves the files itself.
        assert_eq!(batch_len(&paths, Platform::Linux), 1);
        assert_eq!(batch_len(&[], Platform::Windows), 0);
    }

    #[test]
    fn a_long_list_is_split_and_nothing_falls_out_of_it() {
        // Windows refuses a command line past 32,767 characters, so a big
        // delete has to become several calls - and every path has to be in
        // exactly one of them.
        let paths: Vec<PathBuf> = (0..2_000)
            .map(|i| {
                PathBuf::from(format!(
                    r"C:\Users\someone\a rather long folder name\file-{i}.txt"
                ))
            })
            .collect();

        let mut batches = 0;
        let mut covered = 0;
        let mut rest = paths.as_slice();
        while !rest.is_empty() {
            let take = batch_len(rest, Platform::Windows);
            assert!(take > 0, "a batch of nothing would never finish");
            covered += take;
            batches += 1;
            rest = &rest[take..];
        }
        assert_eq!(covered, paths.len(), "every path in exactly one batch");
        assert!(batches > 1, "this many should not have fitted in one call");

        // Each batch really does fit, which is the only thing the size is for.
        let take = batch_len(&paths, Platform::Windows);
        let encoded = windows_command(&paths[..take]).args.last().unwrap().len();
        assert!(
            encoded < 32_000,
            "the command line would be refused: {encoded}"
        );
    }

    #[test]
    fn a_path_too_long_for_the_budget_still_goes_by_itself() {
        // Refusing to delete a file because its name is long would be a worse
        // answer than a command line the system may well still accept.
        let huge = PathBuf::from(format!(r"C:\{}\x.txt", "d".repeat(MAX_SCRIPT * 2)));
        assert_eq!(batch_len(&[huge], Platform::Windows), 1);
    }

    #[test]
    fn the_report_says_which_path_and_a_silence_is_not_a_yes() {
        let printed = format!(
            "{REPORT} 0 ok\n\
             {REPORT} 2 err Access to the path is denied.\n\
             something the shell said on its own\n\
             {REPORT} 9 ok\n",
        );
        let out = reports(&printed, 4, "nothing came back");

        assert!(out[0].is_ok());
        // Never reported on, so it is not known to have gone - and a file
        // still on disk reported as trashed is the worse of the two wrongs.
        assert!(out[1].is_err());
        assert_eq!(
            out[1].as_ref().unwrap_err().to_string(),
            "nothing came back"
        );
        assert_eq!(
            out[2].as_ref().unwrap_err().to_string(),
            "Access to the path is denied."
        );
        assert!(out[3].is_err(), "index 9 belongs to no path here");
        assert_eq!(out.len(), 4, "one per path asked about, no more");
    }

    #[test]
    fn a_failure_with_nothing_to_say_still_says_something() {
        let out = reports(&format!("{REPORT} 0 err   "), 1, "unused");
        assert!(out[0].is_err());
        assert!(
            !out[0].as_ref().unwrap_err().to_string().trim().is_empty(),
            "a blank complaint is no complaint"
        );
    }

    #[test]
    fn the_recorded_path_is_percent_encoded() {
        assert_eq!(url_encode("/home/u/notes.txt"), "/home/u/notes.txt");
        // Spaces and anything outside the unreserved set, but not the
        // separator - it is structure rather than data.
        assert_eq!(url_encode("/home/u/my notes.txt"), "/home/u/my%20notes.txt");
        assert_eq!(url_encode("/a/b#c?d"), "/a/b%23c%3Fd");
        assert_eq!(url_encode("/a/100%"), "/a/100%25");
        // Non-ASCII goes byte by byte, as a URI must.
        assert_eq!(url_encode("/a/é"), "/a/%C3%A9");
    }

    #[test]
    fn the_record_is_the_format_the_desktop_reads() {
        let body = trash_info(Path::new("/home/u/my notes.txt"), "2026-07-27T22:30:00");
        assert_eq!(
            body,
            "[Trash Info]\nPath=/home/u/my%20notes.txt\nDeletionDate=2026-07-27T22:30:00\n"
        );
    }

    #[test]
    fn the_stamp_is_the_shape_the_spec_asks_for() {
        let stamp = now_stamp();
        // YYYY-MM-DDThh:mm:ss, local time, no zone.
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-');
        assert_eq!(stamp.as_bytes()[10], b'T');
        assert_eq!(stamp.as_bytes()[13], b':');
    }

    #[test]
    fn a_taken_name_gets_a_number_before_the_extension() {
        let free = |_: &str| false;
        assert_eq!(unique_name("notes.txt", &free), "notes.txt");

        let taken_once = |name: &str| name == "notes.txt";
        assert_eq!(unique_name("notes.txt", &taken_once), "notes.2.txt");

        let taken_twice = |name: &str| matches!(name, "notes.txt" | "notes.2.txt");
        assert_eq!(unique_name("notes.txt", &taken_twice), "notes.3.txt");

        // No extension to go before.
        let taken = |name: &str| name == "README";
        assert_eq!(unique_name("README", &taken), "README.2");
    }

    #[test]
    fn trashing_writes_the_record_and_moves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "content").unwrap();

        let landed = move_into_trash(&file, &trash, None, "2026-07-27T22:30:00").unwrap();

        assert!(!file.exists(), "the original is still there");
        assert_eq!(landed, trash.join("files/notes.txt"));
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "content");

        let record = std::fs::read_to_string(trash.join("info/notes.txt.trashinfo")).unwrap();
        assert!(record.starts_with("[Trash Info]"), "{record}");
        assert!(
            record.contains(&format!("Path={}", url_encode(&file.display().to_string()))),
            "{record}"
        );
        assert!(
            record.contains("DeletionDate=2026-07-27T22:30:00"),
            "{record}"
        );
    }

    #[test]
    fn two_files_of_the_same_name_both_survive() {
        // The case that makes the numbering necessary: same name, different
        // directories, both deleted.
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let mut landed = Vec::new();
        let mut originals = Vec::new();
        for (index, sub) in ["one", "two"].iter().enumerate() {
            let from = dir.path().join(sub);
            std::fs::create_dir_all(&from).unwrap();
            let file = from.join("notes.txt");
            std::fs::write(&file, format!("from {index}")).unwrap();
            landed.push(move_into_trash(&file, &trash, None, "2026-07-27T22:30:00").unwrap());
            originals.push(file);
        }

        assert_ne!(landed[0], landed[1], "the second replaced the first");
        assert_eq!(std::fs::read_to_string(&landed[0]).unwrap(), "from 0");
        assert_eq!(std::fs::read_to_string(&landed[1]).unwrap(), "from 1");
        // Each has its own record, and they point at different originals.
        let first = std::fs::read_to_string(trash.join("info/notes.txt.trashinfo")).unwrap();
        let second = std::fs::read_to_string(trash.join("info/notes.2.txt.trashinfo")).unwrap();
        // Against the encoded whole path, not a `/one/notes.txt` fragment: the
        // separator is a backslash on Windows and arrives percent-encoded, so
        // a hand-spelled fragment would only ever match on Unix.
        assert!(
            first.contains(&url_encode(&originals[0].display().to_string())),
            "{first}"
        );
        assert!(
            second.contains(&url_encode(&originals[1].display().to_string())),
            "{second}"
        );
    }

    #[test]
    fn a_directory_goes_in_whole() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let tree = dir.path().join("project");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/main.rs"), "fn main() {}").unwrap();

        let landed = move_into_trash(&tree, &trash, None, "2026-07-27T22:30:00").unwrap();

        assert!(!tree.exists());
        assert_eq!(
            std::fs::read_to_string(landed.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn a_volume_trash_records_the_path_relative_to_its_volume() {
        // So the trash still makes sense when the volume is mounted somewhere
        // else next time.
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("media/stick");
        let trash = top.join(".Trash-1000");
        let file = top.join("photos/holiday.jpg");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "jpeg").unwrap();

        move_into_trash(&file, &trash, Some(&top), "2026-07-27T22:30:00").unwrap();

        let record = std::fs::read_to_string(trash.join("info/holiday.jpg.trashinfo")).unwrap();
        assert!(record.contains("Path=photos/holiday.jpg"), "{record}");
        assert!(!record.contains(&top.display().to_string()), "{record}");
    }

    #[test]
    fn a_move_that_fails_leaves_no_orphan_record() {
        // A record pointing at a file still in place is worse than nothing:
        // the desktop offers to restore something that was never deleted.
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let missing = dir.path().join("never-existed.txt");

        assert!(move_into_trash(&missing, &trash, None, "2026-07-27T22:30:00").is_err());
        assert!(
            !trash.join("info/never-existed.txt.trashinfo").exists(),
            "the record outlived the failed move"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_top_directory_is_where_the_device_changes() {
        // Everything in one tempdir is on one filesystem, so the answer is a
        // real mount point above it - and never the file itself.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let top = top_dir(&file);
        assert!(file.starts_with(&top), "{top:?} is not above {file:?}");
        assert!(top.is_dir());
    }

    #[test]
    fn macos_asks_finder_rather_than_moving_it_by_hand() {
        let command = macos_command(&[PathBuf::from("/Users/u/a b.txt")]);
        assert_eq!(command.program, "osascript");
        let script = &command.args[1];
        assert!(script.contains("Finder"), "{script}");
        assert!(script.contains("delete"), "{script}");
        assert!(script.contains("/Users/u/a b.txt"), "{script}");

        // A quote in the name must not end the AppleScript string.
        let command = macos_command(&[PathBuf::from(r#"/Users/u/it"s.txt"#)]);
        let script = &command.args[1];
        assert!(script.contains(r#"it\"s.txt"#), "{script}");
    }

    #[test]
    fn windows_recycles_rather_than_deleting() {
        let command = windows_command(&[PathBuf::from(r"C:\Users\u\it's.txt")]);
        assert_eq!(command.program, "powershell.exe");
        assert!(command.args.contains(&"-EncodedCommand".to_string()));

        // Decode it back, so the test is about what will run.
        let script = decode(command.args.last().unwrap());
        assert!(script.contains("SendToRecycleBin"), "{script}");
        assert!(!script.contains("Remove-Item"), "{script}");
        // Both shapes, since a directory takes the other call.
        assert!(script.contains("DeleteFile"), "{script}");
        assert!(script.contains("DeleteDirectory"), "{script}");
        // The quote is doubled, which is how PowerShell escapes it.
        assert!(script.contains("'C:\\Users\\u\\it''s.txt'"), "{script}");
    }

    fn decode(encoded: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let values: Vec<u32> = encoded
            .bytes()
            .filter(|c| *c != b'=')
            .map(|c| ALPHABET.iter().position(|a| *a == c).unwrap() as u32)
            .collect();
        let mut bytes = Vec::new();
        for chunk in values.chunks(4) {
            let mut triple = 0u32;
            for (index, value) in chunk.iter().enumerate() {
                triple |= value << (18 - 6 * index);
            }
            for index in 0..chunk.len() * 6 / 8 {
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
    fn only_the_platforms_with_their_own_call_delegate() {
        assert!(delegates(Platform::MacOs));
        assert!(delegates(Platform::Windows));
        // Linux is the one this module implements itself.
        assert!(!delegates(Platform::Linux));
    }
}
