// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Turning a path into one a person would write down.
//!
//! Not [`std::fs::canonicalize`], and the reason is worth stating once here
//! rather than being rediscovered by each front-end. Canonicalising needs the
//! path to exist, which an address bar's contents may not yet; it follows
//! symbolic links, which is not what a file manager's `..` means; and on
//! Windows it hands back a *verbatim* path - `\\?\C:\src` rather than
//! `C:\src`.
//!
//! That last one is the visible bug. The `\\?\` prefix is how Windows is told
//! to skip its own parsing and the 260-character limit with it, so it is the
//! right thing to *pass to the operating system* and the wrong thing to show
//! anybody. A file manager that displayed it would be showing its own
//! plumbing in the one place the reader is looking to find out where they
//! are.

use std::path::{Component, Path, PathBuf};

/// Fold away `.` and `..` so a path bar shows what a person would write.
///
/// Lexical, so it works on a path that does not exist and leaves symbolic
/// links alone: `..` means the directory above the one shown, not wherever a
/// link happens to point, which is what a file manager's up-arrow does.
pub fn tidied(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                // Something to climb out of.
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // Above the root is the root; Windows says the same of `C:\..`.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                // Nothing to fold into, so this is a relative path that really
                // does start above where it stands. Dropping it would quietly
                // change which directory was meant.
                _ => out.push(part),
            },
            other => out.push(other),
        }
    }
    out
}

/// Strip the `\\?\` a Windows verbatim path carries, where it is safe to.
///
/// Safe means the path still means the same thing without it, which is not
/// always: a path longer than 260 characters, or one holding a component the
/// Win32 layer would reinterpret, needs the prefix to keep working. Those are
/// left exactly as they are - a path that displays badly beats one that
/// displays nicely and no longer opens.
pub fn undecorated(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();

    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        // `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`.
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        // Only a drive path folds back. `\\?\Volume{...}` names a volume with
        // no letter and has no shorter spelling at all.
        let looks_like_a_drive = {
            let mut chars = rest.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
                && matches!(chars.next(), Some(':'))
        };
        if !looks_like_a_drive {
            return path.to_path_buf();
        }
        rest.to_string()
    } else {
        return path.to_path_buf();
    };

    // The prefix is what lifts the old length limit, so a path that needs the
    // length keeps the prefix. 260 is the limit itself, including the drive
    // and the terminator.
    if plain.chars().count() >= 260 {
        return path.to_path_buf();
    }
    PathBuf::from(plain)
}

/// What to show, and what to hand back to the rest of the program.
///
/// Relative input is resolved against `from` so a pane always knows where it
/// is, and the result is tidy and free of plumbing.
pub fn resolved(path: &Path, from: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        from.join(path)
    };
    undecorated(&tidied(&absolute))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_fold_away() {
        assert_eq!(tidied(Path::new("a/./b/../c")), PathBuf::from("a/c"));
        // A relative path that really does start above where it stands keeps
        // saying so: dropping it would change which directory was meant.
        assert_eq!(tidied(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn climbing_above_the_root_stays_at_the_root() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let up = Path::new(root).join("..").join("..");
        assert_eq!(tidied(&up), PathBuf::from(root));
    }

    #[test]
    fn a_verbatim_windows_path_is_shown_the_way_it_is_written() {
        // The bug this exists for: `std::fs::canonicalize` hands back the
        // first of these, and it is what the pane header showed.
        assert_eq!(
            undecorated(Path::new(r"\\?\C:\src\lost-commander")),
            PathBuf::from(r"C:\src\lost-commander")
        );
        assert_eq!(
            undecorated(Path::new(r"\\?\UNC\server\share\file")),
            PathBuf::from(r"\\server\share\file")
        );
    }

    #[test]
    fn a_prefix_that_is_load_bearing_is_left_alone() {
        // A volume with no drive letter has no shorter spelling, so there is
        // nothing to strip to.
        let volume = r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\data";
        assert_eq!(undecorated(Path::new(volume)), PathBuf::from(volume));

        // And a path only legal *because* of the prefix keeps it. Displaying
        // it nicely is not worth handing back one that no longer opens.
        let long = format!(r"\\?\C:\{}", "x".repeat(300));
        assert_eq!(undecorated(Path::new(&long)), PathBuf::from(&long));
    }

    #[test]
    fn an_ordinary_path_is_untouched() {
        for path in ["/home/you/src", r"C:\src", "relative/bit"] {
            assert_eq!(undecorated(Path::new(path)), PathBuf::from(path));
        }
    }

    #[test]
    fn resolving_makes_a_relative_path_absolute_without_plumbing() {
        let from = if cfg!(windows) {
            PathBuf::from(r"C:\src\lost-commander")
        } else {
            PathBuf::from("/src/lost-commander")
        };
        let resolved = resolved(Path::new("core/../egui"), &from);
        assert_eq!(resolved, from.join("egui"));
        assert!(!resolved.to_string_lossy().contains(r"\\?\"));
    }
}
