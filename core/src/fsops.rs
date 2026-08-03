// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Small, immediate file operations.
//!
//! Anything that can take a noticeable amount of time - copy, move, delete -
//! lives in [`crate::progress`] instead, where it runs on a worker thread with
//! a progress bar and a cancel key. What is left here completes instantly.
//!
//! Everything is written against `std::fs`, so the same code runs on Linux,
//! macOS and Windows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::encoding;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

/// Reject names that would escape the target directory.
pub fn validate_name(name: &str) -> io::Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(invalid("reserved name"));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(invalid("name must not contain path separators"));
    }
    Ok(())
}

/// Rename in place, within the same parent directory.
pub fn rename(src: &Path, new_name: &str) -> io::Result<PathBuf> {
    validate_name(new_name)?;
    let parent = src
        .parent()
        .ok_or_else(|| invalid("cannot rename the filesystem root"))?;
    let dst = parent.join(new_name.trim());
    if dst.exists() {
        return Err(invalid(format!("{new_name} already exists")));
    }
    fs::rename(src, &dst)?;
    Ok(dst)
}

pub fn create_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    validate_name(name)?;
    let path = parent.join(name.trim());
    if path.exists() {
        return Err(invalid(format!("{name} already exists")));
    }
    fs::create_dir(&path)?;
    Ok(path)
}

/// Read a text file for the F3 viewer, capped so huge files cannot exhaust
/// memory. Binary content is shown lossily rather than refused.
///
/// `as_` forces an encoding; `None` reads it as whatever the bytes appear to
/// be. Taking everything for UTF-8 is what turns a Cyrillic or a Windows-made
/// file into a screen of replacement characters, and from the outside that
/// looks exactly like a corrupt file - so the detection comes back with the
/// lines, and the viewer can offer to read it another way.
pub fn read_preview(
    path: &Path,
    max_bytes: usize,
    as_: Option<encoding::Encoding>,
) -> io::Result<(Vec<String>, encoding::Detected)> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut buffer = Vec::new();
    file.take(max_bytes as u64).read_to_end(&mut buffer)?;

    let detected = encoding::sniff(&buffer);
    let text = encoding::decode(&buffer, as_.unwrap_or(detected.encoding));
    let lines = text.lines().map(|l| l.replace('\t', "    ")).collect();
    Ok((lines, detected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn write(path: &Path, contents: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn rename_changes_the_name_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("old.txt");
        write(&file, "x");

        let renamed = rename(&file, "new.txt").unwrap();
        assert_eq!(renamed, dir.path().join("new.txt"));
        assert!(!file.exists());
        assert!(renamed.exists());
    }

    #[test]
    fn rename_refuses_to_clobber_and_rejects_separators() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), "a");
        write(&dir.path().join("b.txt"), "b");

        assert!(rename(&dir.path().join("a.txt"), "b.txt").is_err());
        assert!(rename(&dir.path().join("a.txt"), "sub/c.txt").is_err());
        assert!(rename(&dir.path().join("a.txt"), "").is_err());
        assert!(rename(&dir.path().join("a.txt"), "..").is_err());
    }

    #[test]
    fn create_dir_validates_and_refuses_duplicates() {
        let dir = tempfile::tempdir().unwrap();

        let made = create_dir(dir.path(), "fresh").unwrap();
        assert!(made.is_dir());

        assert!(create_dir(dir.path(), "fresh").is_err());
        assert!(create_dir(dir.path(), "").is_err());
        assert!(create_dir(dir.path(), "a/b").is_err());
    }

    #[test]
    fn preview_is_truncated_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        let body = "line\n".repeat(1000);
        write(&file, &body);

        let (lines, _) = read_preview(&file, 50, None).unwrap();
        // 50 bytes / 5 bytes per line == 10 lines.
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "line");
    }

    #[test]
    fn preview_expands_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tabs.txt");
        write(&file, "a\tb");
        assert_eq!(read_preview(&file, 4096, None).unwrap().0, vec!["a    b"]);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_read_as_what_it_is_rather_than_as_nonsense() {
        // Taking every file for UTF-8 turns a Cyrillic one into a screen of
        // replacement characters, which looks like a corrupt file rather than
        // like the wrong encoding.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cyrillic.txt");
        let bytes = encoding::encode("Привет\n", encoding::Encoding::Cp1251).bytes;
        fs::write(&file, &bytes).unwrap();

        let (lines, detected) = read_preview(&file, 4096, None).unwrap();
        assert_eq!(lines, vec!["Привет"]);
        assert_eq!(detected.encoding, encoding::Encoding::Cp1251);

        // And forcing it the other way gives the wrong answer rather than an
        // error, which is what lets the viewer's key be pressed until the
        // text looks right.
        let (wrong, _) = read_preview(&file, 4096, Some(encoding::Encoding::Cp1252)).unwrap();
        assert_ne!(wrong, vec!["Привет"]);
        assert_eq!(wrong.len(), 1);
    }
}
