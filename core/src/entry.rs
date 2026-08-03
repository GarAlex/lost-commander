// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The model for a single directory entry, plus display formatting.
//!
//! Nothing in here touches the terminal, so it is all directly unit-testable.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The synthetic ".." entry that walks to the parent directory.
    Parent,
    Dir,
    File,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_symlink: bool,
    /// The Unix permission bits, where there are any.
    ///
    /// Read while the directory is being listed - the `stat` has already been
    /// done for the size and the date, so this is free - and `None` on a
    /// platform that has no such thing.
    pub mode: Option<crate::perms::Mode>,
    /// User selection (Insert / Space), as in Norton Commander.
    pub marked: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Parent)
    }

    pub fn is_parent(&self) -> bool {
        self.kind == EntryKind::Parent
    }

    /// Lowercased extension, empty for directories and extension-less files.
    pub fn extension(&self) -> String {
        if self.is_dir() {
            return String::new();
        }
        Path::new(&self.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }

    pub fn parent_entry(current: &Path) -> Option<Entry> {
        let parent = current.parent()?;
        Some(Entry {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            kind: EntryKind::Parent,
            size: 0,
            modified: None,
            is_symlink: false,
            mode: None,
            marked: false,
        })
    }
}

/// Human-readable byte count, kept narrow enough for the size column.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return format!("{bytes}");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.2}{}", UNITS[unit])
    }
}

/// A size as prose rather than as a column.
///
/// The size column can leave a small number bare, because the heading above
/// it says what the number is. A sentence cannot: "2 copies of 14 each" reads
/// like nonsense where "2 copies of 14 bytes each" does not, and past a
/// kilobyte the unit is already there.
pub fn size_in_words(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        human_size(bytes)
    }
}

/// What the size column shows for an entry.
pub fn size_cell(entry: &Entry) -> String {
    match entry.kind {
        EntryKind::Parent => "<UP>".to_string(),
        EntryKind::Dir => "<DIR>".to_string(),
        EntryKind::File => human_size(entry.size),
    }
}

/// `dd.mm.yy hh:mm`, in the machine's local timezone.
pub fn format_time(time: Option<SystemTime>) -> String {
    match time {
        Some(t) => {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%d.%m.%y %H:%M").to_string()
        }
        None => String::new(),
    }
}

/// Truncate to `width` columns, marking cut-off names with a trailing `~`.
pub fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(512), "512");
        assert_eq!(human_size(1023), "1023");
        assert_eq!(human_size(1024), "1.00K");
        assert_eq!(human_size(1536), "1.50K");
        assert_eq!(human_size(10 * 1024), "10.0K");
        assert_eq!(human_size(100 * 1024), "100K");
        assert_eq!(human_size(1024 * 1024), "1.00M");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.00G");
    }

    #[test]
    fn a_size_in_a_sentence_carries_its_unit() {
        assert_eq!(size_in_words(0), "0 bytes");
        assert_eq!(size_in_words(14), "14 bytes");
        assert_eq!(size_in_words(1023), "1023 bytes");
        assert_eq!(size_in_words(1024), "1.00K", "past which the unit is there");
        assert_eq!(size_in_words(40_000), "39.1K");
    }

    #[test]
    fn size_cell_marks_directories() {
        let dir = Entry {
            name: "src".into(),
            path: PathBuf::from("/tmp/src"),
            kind: EntryKind::Dir,
            size: 4096,
            modified: None,
            is_symlink: false,
            mode: None,
            marked: false,
        };
        assert_eq!(size_cell(&dir), "<DIR>");

        let parent = Entry {
            kind: EntryKind::Parent,
            ..dir.clone()
        };
        assert_eq!(size_cell(&parent), "<UP>");

        let file = Entry {
            kind: EntryKind::File,
            size: 2048,
            ..dir
        };
        assert_eq!(size_cell(&file), "2.00K");
    }

    #[test]
    fn extension_is_lowercased_and_empty_for_dirs() {
        let mut e = Entry {
            name: "Photo.JPG".into(),
            path: PathBuf::from("/tmp/Photo.JPG"),
            kind: EntryKind::File,
            size: 1,
            modified: None,
            is_symlink: false,
            mode: None,
            marked: false,
        };
        assert_eq!(e.extension(), "jpg");

        e.name = "README".into();
        assert_eq!(e.extension(), "");

        e.kind = EntryKind::Dir;
        e.name = "assets.d".into();
        assert_eq!(e.extension(), "");
    }

    #[test]
    fn fit_truncates_with_marker() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("exactfit!!", 10), "exactfit!!");
        assert_eq!(fit("a-very-long-filename", 10), "a-very-lo~");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn parent_entry_absent_at_root() {
        let root = Path::new("/");
        assert!(Entry::parent_entry(root).is_none());

        let nested = Path::new("/home/user");
        let parent = Entry::parent_entry(nested).expect("should have parent");
        assert_eq!(parent.name, "..");
        assert_eq!(parent.path, PathBuf::from("/home"));
        assert!(parent.is_dir());
        assert!(parent.is_parent());
    }
}
