// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! One of the two file panes: what directory it shows, where the cursor is,
//! how entries are sorted, and which of them are marked.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use std::time::SystemTime;

use crate::entry::{Entry, EntryKind};

/// How many entries a directory can have before the cheap check gives up on
/// looking at each of them.
///
/// Reading a directory is one syscall however many entries it has; asking each
/// entry for its size is one syscall *each*. Two thousand of those a second is
/// still nothing on a local disk, and past that the directory's own timestamp
/// has to be enough.
pub const WATCH_DETAIL_LIMIT: usize = 2_000;

/// What a directory looks like from outside, cheaply.
///
/// The directory's own mtime moves whenever an entry is created, removed or
/// renamed - every structural change. It does **not** move when a file already
/// listed grows, which is why the sizes and the newest entry time are folded
/// in as well for directories small enough to look at entry by entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    /// The directory's own modification time.
    pub directory: Option<SystemTime>,
    pub count: usize,
    /// Only filled in below [`WATCH_DETAIL_LIMIT`] entries.
    pub bytes: u64,
    /// Ditto: the most recent entry, which is what catches a file being
    /// written to without anything being added or removed.
    pub newest: Option<SystemTime>,
}

/// Read a directory's signature.
///
/// A directory that cannot be read has a signature too - the default one -
/// so a directory that disappears reads as a change rather than as nothing.
pub fn signature(dir: &Path, detail_limit: usize) -> Signature {
    let mut signature = Signature {
        directory: fs::metadata(dir).and_then(|m| m.modified()).ok(),
        ..Signature::default()
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return signature;
    };

    for entry in entries.flatten() {
        signature.count += 1;
        if signature.count > detail_limit {
            // Past the limit the detail is dropped rather than half-kept, so
            // the value cannot depend on the order the entries came back in.
            signature.bytes = 0;
            signature.newest = None;
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        signature.bytes = signature.bytes.wrapping_add(metadata.len());
        if let Ok(modified) = metadata.modified() {
            signature.newest = Some(match signature.newest {
                Some(newest) if newest > modified => newest,
                _ => modified,
            });
        }
    }
    signature
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Ext,
    Size,
    Time,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Ext => "ext",
            SortBy::Size => "size",
            SortBy::Time => "time",
        }
    }

    /// Which way round this column is worth reading first.
    ///
    /// Names read A to Z, and sizes and dates read biggest and newest first,
    /// because that is what someone sorting by them is nearly always looking
    /// for - the thing filling the disk, or what changed this morning. So
    /// "descending" is not the odd case for half of these columns; it is the
    /// one to start on, and the front-end asks for this when the column is
    /// first picked rather than hard-coding a direction of its own.
    pub fn natural_order(self) -> Order {
        match self {
            SortBy::Name | SortBy::Ext => Order::Ascending,
            SortBy::Size | SortBy::Time => Order::Descending,
        }
    }
}

/// Which way a column is sorted.
///
/// Absolute rather than "natural or reversed", so that a caller asking for
/// descending gets descending whatever the column - and only the *default*,
/// [`SortBy::natural_order`], varies by column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Ascending,
    Descending,
}

impl Order {
    pub fn flip(self) -> Order {
        match self {
            Order::Ascending => Order::Descending,
            Order::Descending => Order::Ascending,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Order::Ascending => "asc",
            Order::Descending => "desc",
        }
    }
}

/// True for entries the user normally does not want to see.
///
/// Unix uses the leading-dot convention; Windows additionally has a real
/// hidden attribute, which we honour when it is available.
pub fn is_hidden(name: &str, metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
        if metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
            return true;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
    }
    name.starts_with('.')
}

/// Read a directory into sorted entries. Unreadable children are skipped
/// rather than failing the whole listing.
/// The permission bits, where the platform has them.
#[cfg(unix)]
fn mode_of(metadata: &fs::Metadata) -> Option<crate::perms::Mode> {
    use std::os::unix::fs::PermissionsExt;
    Some(crate::perms::Mode::from_bits(metadata.permissions().mode()))
}

#[cfg(not(unix))]
fn mode_of(_metadata: &fs::Metadata) -> Option<crate::perms::Mode> {
    None
}

pub fn read_entries(
    dir: &Path,
    show_hidden: bool,
    sort_by: SortBy,
    order: Order,
) -> io::Result<Vec<Entry>> {
    let mut out: Vec<Entry> = Vec::new();

    for item in fs::read_dir(dir)? {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().to_string();

        // `DirEntry::metadata` rather than `symlink_metadata` on the path, and
        // the difference is not stylistic. Both describe the entry itself
        // rather than what a link points at, which is what makes a broken link
        // still list - but the path form has to resolve the path and open the
        // file, while this one answers from what the directory read already
        // returned. On Windows that is the difference between a system call per
        // entry and none: measured on a directory of 32,724, it took the
        // listing from 4.5 seconds to a fraction of that. On Unix the two cost
        // the same, so nothing is lost there.
        let metadata = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if !show_hidden && is_hidden(&name, &metadata) {
            continue;
        }

        let is_symlink = metadata.file_type().is_symlink();
        // For links, classify by the target so entering them works naturally.
        let resolved_is_dir = if is_symlink {
            fs::metadata(item.path())
                .map(|m| m.is_dir())
                .unwrap_or(false)
        } else {
            metadata.is_dir()
        };

        out.push(Entry {
            name,
            path: item.path(),
            kind: if resolved_is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            size: if resolved_is_dir { 0 } else { metadata.len() },
            modified: metadata.modified().ok(),
            is_symlink,
            mode: mode_of(&metadata),
            marked: false,
        });
    }

    sort_entries(&mut out, sort_by, order);

    // ".." always leads the list, above the sorted content.
    if let Some(parent) = Entry::parent_entry(dir) {
        out.insert(0, parent);
    }
    Ok(out)
}

/// Directories before files, then by the chosen criterion. `..` is not
/// included here; it is prepended after sorting.
/// Match a name against a shell-style glob: `*` any run, `?` any one.
///
/// Case-insensitive, because a pattern typed as `*.JPG` is meant to catch
/// `photo.jpg` - the user is naming a kind of file, not a byte sequence. Only
/// these two wildcards: character classes would be a second syntax to explain
/// for a box you type `*.txt` into.
///
/// Iterative with backtracking rather than recursive, so a pathological
/// pattern cannot blow the stack.
pub fn matches_glob(pattern: &str, name: &str) -> bool {
    matches_glob_with(pattern, name, false)
}

/// As [`matches_glob`], with the case rule chosen.
///
/// Selection by pattern always wants the forgiving one; search offers both,
/// since someone looking for `README` among `readme` files means it.
pub fn matches_glob_with(pattern: &str, name: &str, case_sensitive: bool) -> bool {
    let fold = |text: &str| -> Vec<char> {
        if case_sensitive {
            text.chars().collect()
        } else {
            text.to_lowercase().chars().collect()
        }
    };
    let pattern = fold(pattern);
    let name = fold(name);

    let (mut p, mut n) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have eaten too little.
    let (mut star, mut retry) = (None, 0usize);

    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                retry = n;
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(&c) if c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                // Back up: let the last `*` swallow one more character.
                Some(position) => {
                    p = position + 1;
                    retry += 1;
                    n = retry;
                }
                None => return false,
            },
        }
    }
    // Trailing stars can match nothing at all.
    while pattern.get(p) == Some(&'*') {
        p += 1;
    }
    p == pattern.len()
}

pub fn sort_entries(entries: &mut [Entry], sort_by: SortBy, order: Order) {
    entries.sort_by(|a, b| {
        // Directories always cluster above files, whichever way round the
        // column is sorted. Reversing that would send them to the bottom, and
        // nobody reversing a sort by size is asking for the folders to move.
        match (a.is_dir(), b.is_dir()) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        // Each written the way round its own name reads: ascending by name is
        // A to Z, ascending by size is smallest first.
        let ordering = match sort_by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Ext => a
                .extension()
                .cmp(&b.extension())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Time => a.modified.cmp(&b.modified),
        };
        let ordering = match order {
            Order::Ascending => ordering,
            Order::Descending => ordering.reverse(),
        };
        // The tie-break is never reversed: two files of the same size should
        // stay in a settled order rather than swapping places when the column
        // above them is flipped.
        ordering.then_with(|| a.name.cmp(&b.name))
    });
}

/// Where a panel is when it is not simply in a directory.
///
/// The archive is read once on the way in and the index kept here, because a
/// panel reloads on a timer and re-reading a tarball every second would mean
/// decompressing it every second.
#[derive(Debug, Clone)]
pub struct Inside {
    /// The archive file itself, which is a real path on disk.
    pub archive: PathBuf,
    /// Which level inside it is showing. `""` is the top.
    pub at: String,
    pub format: &'static str,
    pub members: Vec<crate::archive::Member>,
    /// Held for this session only, and never written anywhere - the rule the
    /// network locations already follow, for the same reason.
    pub password: Option<String>,
}

impl Inside {
    /// The path shown for a level inside the archive, which is the archive's
    /// own path with the level below it: `/home/me/docs.zip/notes`.
    ///
    /// Not a path anything can open. Nothing tries: while a panel is inside
    /// an archive it reads through [`crate::archive`], and the operations
    /// that would touch the filesystem refuse.
    pub fn shown_path(&self) -> PathBuf {
        match self.at.is_empty() {
            true => self.archive.clone(),
            false => self.archive.join(&self.at),
        }
    }
}

pub struct Panel {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub sort_by: SortBy,
    /// Which way round `sort_by` runs. Set from the column's natural order
    /// when the column changes, and flipped on its own by `flip_order`.
    pub order: Order,
    pub show_hidden: bool,
    /// Set when the directory could not be listed (permissions, removed, ...).
    pub error: Option<String>,
    /// When present the panel shows a directory tree instead of a listing.
    pub tree: Option<crate::tree::Tree>,

    /// Who opened this tab.
    ///
    /// A tab the program opened on the reader's behalf - to show where
    /// something in the account happened - is not one they chose, and finding
    /// three of them later with no idea why is worse than not opening them.
    /// A front-end colours the strip by this rather than writing a reason
    /// into the title, which would cost the width the name needs.
    pub opened: Opened,

    /// Files tagged while walking a tree, by full path.
    ///
    /// `Entry::marked` is a fact about a row, and a row only exists while its
    /// directory is the one being shown - so marks made in one directory used
    /// to vanish on the way to the next. Tagging files across directories and
    /// then acting on the lot is the thing XTree had that nothing else did,
    /// and it needs somewhere to live that outlasts a listing.
    ///
    /// Only while a tree is up. Without one every marked file is on screen,
    /// and marks that survived walking away would be a set the reader could
    /// no longer see to undo.
    pub tagged: std::collections::BTreeSet<PathBuf>,
    /// When present the panel is inside an archive rather than a directory.
    pub inside: Option<Inside>,
    /// What the directory looked like when it was last read, so a change made
    /// by something else can be noticed.
    watching: Signature,
}

/// Who opened a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Opened {
    /// The reader asked for it.
    #[default]
    ByHand,
    /// The program opened it to show where something in the account
    /// happened.
    FromRecord,
}

impl Panel {
    pub fn new(path: PathBuf) -> Self {
        let mut panel = Panel {
            cwd: path,
            opened: Opened::default(),
            entries: Vec::new(),
            tagged: std::collections::BTreeSet::new(),
            cursor: 0,
            sort_by: SortBy::Name,
            order: SortBy::Name.natural_order(),
            show_hidden: false,
            error: None,
            tree: None,
            inside: None,
            watching: Signature::default(),
        };
        panel.reload();
        panel
    }

    /// Fold the marks on the rows in view into the set that outlives them.
    ///
    /// Called after anything that changes a mark. The rows in view are the
    /// authority for their own directory - a file unmarked here is untagged,
    /// not merely left alone - which is what makes unmarking work at all.
    fn remember_marks(&mut self) {
        if self.tree.is_none() {
            return;
        }
        for entry in &self.entries {
            if entry.kind == EntryKind::Parent {
                continue;
            }
            let path = self.cwd.join(&entry.name);
            if entry.marked {
                self.tagged.insert(path);
            } else {
                self.tagged.remove(&path);
            }
        }
    }

    /// Put the marks back on a freshly read listing.
    fn restore_marks(&mut self) {
        for entry in self.entries.iter_mut() {
            if entry.kind == EntryKind::Parent {
                continue;
            }
            entry.marked = self.tagged.contains(&self.cwd.join(&entry.name));
        }
    }

    /// Everything tagged, in a stable order.
    pub fn tagged_paths(&self) -> Vec<PathBuf> {
        self.tagged.iter().cloned().collect()
    }

    /// How many files are tagged, including the ones not on screen.
    pub fn tagged_count(&self) -> usize {
        self.tagged.len()
    }

    pub fn in_tree_mode(&self) -> bool {
        self.tree.is_some()
    }

    /// Open the tree, rooted at the filesystem root and opened down to the
    /// directory the panel is currently showing.
    pub fn enter_tree_mode(&mut self) {
        self.tree = Some(crate::tree::Tree::revealing(&self.cwd, self.show_hidden));
        // Whatever was marked in this directory is the start of the set.
        self.remember_marks();
    }

    pub fn leave_tree_mode(&mut self) {
        self.tree = None;
        // Tags do not outlive the tree that made them reachable. Keeping them
        // would leave the reader holding a selection spread over directories
        // they can no longer see, and no way to find it again.
        self.tagged.clear();
    }

    /// Re-read the current directory, keeping the cursor on the same file
    /// when it still exists.
    pub fn reload(&mut self) {
        if self.inside.is_some() {
            self.reload_inside();
            return;
        }
        let previous = self.selected().map(|e| e.name.clone());
        let marked: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.clone())
            .collect();

        match read_entries(&self.cwd, self.show_hidden, self.sort_by, self.order) {
            Ok(mut entries) => {
                for e in entries.iter_mut() {
                    if marked.contains(&e.name) {
                        e.marked = true;
                    }
                }
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries = Entry::parent_entry(&self.cwd).into_iter().collect();
                self.error = Some(e.to_string());
            }
        }
        // With a tree up the marks come from the set rather than from the
        // names that happened to be marked a moment ago: this reload may be
        // a different directory entirely, and `a.txt` there is not `a.txt`
        // here.
        if self.tree.is_some() {
            self.restore_marks();
        }

        self.cursor = previous
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .unwrap_or(0);
        self.clamp_cursor();
        // Taken after the read, so a change that lands during it is noticed on
        // the next look rather than being recorded as already seen.
        self.watching = signature(&self.cwd, WATCH_DETAIL_LIMIT);
    }

    /// Re-read the directory if something else has changed it.
    ///
    /// Returns whether it did. The cursor and the marks survive, because
    /// [`Panel::reload`] keeps them by name - a listing that refreshed itself
    /// by throwing away where you were would be worse than a stale one.
    ///
    /// A panel showing a tree is left alone: the tree has its own refresh, and
    /// it is a view of a whole filesystem rather than of one directory.
    pub fn poll_changes(&mut self) -> bool {
        if self.tree.is_some() {
            return false;
        }
        // Inside an archive there is no directory to watch. The path shown is
        // the archive's own with a level appended, which nothing can stat, so
        // the check would see a change every second and reload for ever.
        if self.inside.is_some() {
            return false;
        }
        let now = signature(&self.cwd, WATCH_DETAIL_LIMIT);
        if now == self.watching {
            return false;
        }
        self.reload();
        true
    }

    fn clamp_cursor(&mut self) {
        if self.entries.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len() - 1;
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        let next = (self.cursor as isize + delta).clamp(0, last);
        self.cursor = next as usize;
    }

    pub fn cursor_to(&mut self, index: usize) {
        self.cursor = index;
        self.clamp_cursor();
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// Change directory, resetting cursor and marks.
    ///
    /// Leaves any archive: asking for a directory means a directory.
    pub fn chdir(&mut self, path: PathBuf) {
        self.inside = None;
        self.cwd = path;
        self.entries.clear();
        self.cursor = 0;
        self.reload();
    }

    /// Enter the directory under the cursor. Returns false when the cursor
    /// is on a plain file (the caller decides what to do with it).
    pub fn enter(&mut self) -> bool {
        let Some(entry) = self.selected() else {
            return false;
        };
        if !entry.is_dir() {
            return false;
        }

        // Inside an archive there is no directory to change to: the levels
        // are worked out from the index already in hand.
        if let Some(inside) = self.inside.clone() {
            if entry.is_parent() {
                match inside.at.rfind('/') {
                    // Up a level, landing on the one just left.
                    Some(cut) => {
                        let leaving = inside.at[cut + 1..].to_string();
                        self.show_level(inside.at[..cut].to_string(), Some(leaving));
                    }
                    // Already at the top: `..` walks out of the archive.
                    None if inside.at.is_empty() => self.leave_archive(),
                    None => {
                        let leaving = inside.at.clone();
                        self.show_level(String::new(), Some(leaving));
                    }
                }
                return true;
            }
            let Some(member) = self.member_of(entry) else {
                return false;
            };
            self.show_level(member, None);
            return true;
        }

        let target = entry.path.clone();
        let leaving_from = if entry.is_parent() {
            self.cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        } else {
            None
        };

        self.chdir(target);

        // Coming up from a subdirectory, land the cursor on where we were.
        if let Some(name) = leaving_from {
            if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = idx;
            }
        }
        true
    }

    pub fn go_parent(&mut self) {
        if self.inside.is_some() {
            // The same walk `..` does, so the two ways of going up agree.
            let was = self.cursor;
            self.cursor = 0;
            self.enter();
            if self.inside.is_none() && was == 0 {
                // Left the archive; the cursor was placed on it.
            }
            return;
        }
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let from = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.chdir(parent);
            if let Some(name) = from {
                if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
                    self.cursor = idx;
                }
            }
        }
    }

    // ---- archives ------------------------------------------------------------

    /// Whether this panel is looking inside an archive rather than a folder.
    pub fn in_archive(&self) -> bool {
        self.inside.is_some()
    }

    /// Step into an archive, showing its top level.
    ///
    /// The index is read here and kept, so the reloads that follow cost
    /// nothing. A password is taken because some formats encrypt the index
    /// itself and cannot be listed without one.
    pub fn open_archive(&mut self, path: &Path, password: Option<String>) -> io::Result<()> {
        let listing = crate::archive::list_with(path, password.as_deref())?;
        self.inside = Some(Inside {
            archive: path.to_path_buf(),
            at: String::new(),
            format: listing.format,
            members: listing.members,
            password,
        });
        self.cursor = 0;
        self.reload();
        Ok(())
    }

    /// Come back out to the folder the archive sits in, with the cursor on it.
    pub fn leave_archive(&mut self) {
        let Some(inside) = self.inside.take() else {
            return;
        };
        let name = inside
            .archive
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        let folder = inside
            .archive
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        self.chdir(folder);
        if let Some(name) = name {
            if let Some(at) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = at;
            }
        }
    }

    /// Show a level inside the archive already open.
    fn show_level(&mut self, at: String, land_on: Option<String>) {
        let Some(inside) = self.inside.as_mut() else {
            return;
        };
        inside.at = at;
        self.cursor = 0;
        self.reload();
        if let Some(name) = land_on {
            if let Some(index) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = index;
            }
        }
    }

    /// Build the listing for the level being shown inside an archive.
    fn reload_inside(&mut self) {
        let previous = self.selected().map(|e| e.name.clone());
        let marked: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.clone())
            .collect();

        let Some(inside) = self.inside.clone() else {
            return;
        };
        self.cwd = inside.shown_path();

        let mut entries: Vec<Entry> = Vec::new();
        // ".." is always there, and at the top level it walks back out of the
        // archive entirely rather than nowhere.
        entries.push(Entry {
            name: "..".to_string(),
            path: match inside.at.rfind('/') {
                Some(cut) => inside.archive.join(&inside.at[..cut]),
                None => inside.archive.clone(),
            },
            kind: EntryKind::Parent,
            size: 0,
            modified: None,
            is_symlink: false,
            mode: None,
            marked: false,
        });

        for level in crate::archive::at(&inside.members, &inside.at) {
            entries.push(Entry {
                name: level.name.clone(),
                path: inside.archive.join(&level.path),
                kind: match level.is_dir {
                    true => EntryKind::Dir,
                    false => EntryKind::File,
                },
                size: level.size,
                modified: level.modified,
                is_symlink: false,
                mode: level.mode.map(crate::perms::Mode::from_bits),
                marked: marked.contains(&level.name),
            });
        }

        sort_entries(&mut entries[1..], self.sort_by, self.order);
        self.entries = entries;
        self.error = None;
        self.cursor = previous
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .unwrap_or(0);
        self.clamp_cursor();
    }

    /// Where the entry under the cursor sits inside the archive.
    pub fn member_of(&self, entry: &Entry) -> Option<String> {
        let inside = self.inside.as_ref()?;
        let full = entry.path.strip_prefix(&inside.archive).ok()?;
        Some(crate::archive::normalise(&full.to_string_lossy()))
    }

    /// Toggle the mark under the cursor and step down, like Insert in NC.
    pub fn toggle_mark(&mut self) {
        let cursor = self.cursor;
        if let Some(entry) = self.entries.get_mut(cursor) {
            if entry.kind != EntryKind::Parent {
                entry.marked = !entry.marked;
            }
        }
        self.remember_marks();
        self.move_cursor(1);
    }

    pub fn clear_marks(&mut self) {
        for e in self.entries.iter_mut() {
            e.marked = false;
        }
        self.remember_marks();
    }

    /// Mark everything. `..` is never marked - it is not a file.
    pub fn mark_all(&mut self) {
        for e in self.entries.iter_mut() {
            e.marked = e.kind != EntryKind::Parent;
        }
        self.remember_marks();
    }

    pub fn invert_marks(&mut self) {
        for e in self.entries.iter_mut() {
            if e.kind != EntryKind::Parent {
                e.marked = !e.marked;
            }
        }
        self.remember_marks();
    }

    /// Mark every entry between `from` and `to`, whichever way round they are.
    ///
    /// This is shift-click, and it is what makes selecting two hundred files
    /// possible without two hundred clicks. `additive` keeps what was already
    /// marked, which is ctrl-shift-click.
    pub fn mark_range(&mut self, from: usize, to: usize, additive: bool) {
        if !additive {
            self.clear_marks();
        }
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let (low, high) = if from <= to { (from, to) } else { (to, from) };
        for index in low.min(last)..=high.min(last) {
            if let Some(entry) = self.entries.get_mut(index) {
                if entry.kind != EntryKind::Parent {
                    entry.marked = true;
                }
            }
        }
        self.remember_marks();
    }

    /// Mark, or unmark, every name matching a glob.
    ///
    /// The Commander gesture: grey-plus to select `*.jpg`, grey-minus to take
    /// them out again. Returns how many entries changed.
    pub fn mark_matching(&mut self, pattern: &str, marked: bool) -> usize {
        let mut changed = 0;
        for entry in self.entries.iter_mut() {
            if entry.kind == EntryKind::Parent {
                continue;
            }
            if matches_glob(pattern, &entry.name) && entry.marked != marked {
                entry.marked = marked;
                changed += 1;
            }
        }
        self.remember_marks();
        changed
    }

    pub fn marked_count(&self) -> usize {
        self.entries.iter().filter(|e| e.marked).count()
    }

    pub fn marked_size(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.size)
            .sum()
    }

    /// The entries an operation should act on: every marked one, or else
    /// just the one under the cursor. `..` is never a target.
    ///
    /// In the order they are listed, which is also the order the rename
    /// tool's counter runs in - so sorting the panel by date and numbering
    /// the selection are the same gesture.
    pub fn action_entries(&self) -> Vec<&Entry> {
        let marked: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.marked && e.kind != EntryKind::Parent)
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.selected()
            .filter(|e| e.kind != EntryKind::Parent)
            .into_iter()
            .collect()
    }

    /// The paths an operation should act on. See [`Panel::action_entries`].
    pub fn action_targets(&self) -> Vec<PathBuf> {
        self.action_entries()
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }

    /// Sort by a column, starting in the direction that column reads best.
    pub fn set_sort(&mut self, sort_by: SortBy) {
        self.order = sort_by.natural_order();
        self.sort_by = sort_by;
        self.reload();
    }

    /// Turn the current column round.
    pub fn flip_order(&mut self) {
        self.order = self.order.flip();
        self.reload();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("zeta_dir")).unwrap();
        fs::create_dir(root.join("alpha_dir")).unwrap();
        File::create(root.join("b.txt"))
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        File::create(root.join("a.rs"))
            .unwrap()
            .write_all(b"0123456789")
            .unwrap();
        File::create(root.join(".hidden")).unwrap();
        dir
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| e.name.clone()).collect()
    }

    fn marked(panel: &Panel) -> Vec<String> {
        panel
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.clone())
            .collect()
    }

    #[test]
    fn a_glob_matches_the_way_a_typed_pattern_is_meant_to() {
        assert!(matches_glob("*.txt", "notes.txt"));
        assert!(!matches_glob("*.txt", "notes.txt.bak"));
        assert!(matches_glob("*", "anything"));
        assert!(matches_glob("*", ""), "a star may match nothing");
        assert!(matches_glob("a?c", "abc"));
        assert!(!matches_glob("a?c", "ac"), "? is exactly one");

        // Typed as *.JPG, meant to catch photo.jpg: the user is naming a kind
        // of file, not a byte sequence.
        assert!(matches_glob("*.JPG", "photo.jpg"));
        assert!(matches_glob("*.jpg", "PHOTO.JPG"));

        // Several stars, and the backtracking they need.
        assert!(matches_glob("*report*.pdf", "2026-report-final.pdf"));
        assert!(!matches_glob("*report*.pdf", "2026-summary.pdf"));
        assert!(matches_glob("a*b*c", "axxbyyc"));
        assert!(!matches_glob("a*b*c", "axxbyy"));

        // The case that makes a naive matcher loop or give up early: the star
        // has to give back what it took.
        assert!(matches_glob("*aab", "aaab"));
        assert!(matches_glob("*ab*ab", "abab"));

        // A plain name is just a name.
        assert!(matches_glob("Makefile", "makefile"));
        assert!(!matches_glob("Makefile", "Makefile.in"));
    }

    #[test]
    fn selecting_in_bulk_never_takes_the_parent_entry() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());
        assert_eq!(panel.entries[0].name, "..");

        panel.mark_all();
        assert!(!panel.entries[0].marked, ".. is not a file");
        assert_eq!(panel.marked_count(), panel.entries.len() - 1);
        // And it never becomes a target, however it was marked.
        assert!(!panel.action_targets().iter().any(|p| p.ends_with("..")));

        panel.invert_marks();
        assert_eq!(panel.marked_count(), 0);
        panel.invert_marks();
        assert_eq!(panel.marked_count(), panel.entries.len() - 1);

        panel.clear_marks();
        panel.mark_range(0, panel.entries.len() - 1, false);
        assert!(!panel.entries[0].marked);
    }

    #[test]
    fn a_range_is_marked_either_way_round() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());
        let last = panel.entries.len() - 1;

        panel.mark_range(1, 3, false);
        assert_eq!(panel.marked_count(), 3);

        // Dragged upwards: the same range.
        panel.mark_range(3, 1, false);
        assert_eq!(panel.marked_count(), 3);

        // Not additive, so the previous range goes.
        panel.mark_range(last, last, false);
        assert_eq!(panel.marked_count(), 1);

        // Additive keeps it, which is ctrl-shift-click.
        panel.mark_range(1, 2, true);
        assert_eq!(panel.marked_count(), 3);

        // Out of range indices are clamped rather than panicking.
        panel.mark_range(0, 9999, false);
        assert_eq!(panel.marked_count(), panel.entries.len() - 1);
    }

    #[test]
    fn a_pattern_selects_and_deselects_what_it_matches() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());

        assert_eq!(panel.mark_matching("*.txt", true), 1);
        assert_eq!(marked(&panel), ["b.txt"]);

        // Already-marked entries are not counted again.
        assert_eq!(panel.mark_matching("*.txt", true), 0);

        assert_eq!(panel.mark_matching("*_dir", true), 2);
        assert_eq!(panel.marked_count(), 3);

        // Grey-minus: take a subset back out.
        assert_eq!(panel.mark_matching("alpha*", false), 1);
        assert_eq!(panel.marked_count(), 2);

        // A pattern matching nothing changes nothing.
        assert_eq!(panel.mark_matching("*.nope", true), 0);
    }

    #[test]
    fn each_column_starts_the_way_it_reads_best() {
        // Written down because a front-end has to know the same thing to draw
        // the right arrow before it has asked for a listing, and a front-end
        // guessing differently would point the arrow the wrong way.
        assert_eq!(SortBy::Name.natural_order(), Order::Ascending);
        assert_eq!(SortBy::Ext.natural_order(), Order::Ascending);
        assert_eq!(SortBy::Size.natural_order(), Order::Descending);
        assert_eq!(SortBy::Time.natural_order(), Order::Descending);
    }

    #[test]
    fn turning_a_column_over_leaves_the_directories_on_top() {
        // Reversing a sort by size should not send the folders to the bottom:
        // nobody asking for the biggest files first is asking for that.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a-folder")).unwrap();
        std::fs::write(dir.path().join("small.txt"), vec![b'x'; 10]).unwrap();
        std::fs::write(dir.path().join("large.txt"), vec![b'x'; 5000]).unwrap();

        for order in [Order::Ascending, Order::Descending] {
            let entries = read_entries(dir.path(), false, SortBy::Size, order).unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names[0], "..", "{order:?}");
            assert_eq!(names[1], "a-folder", "the folder stays on top: {order:?}");
        }

        // And the files themselves do turn over.
        let big_first = read_entries(dir.path(), false, SortBy::Size, Order::Descending).unwrap();
        let small_first = read_entries(dir.path(), false, SortBy::Size, Order::Ascending).unwrap();
        assert_eq!(big_first.last().unwrap().name, "small.txt");
        assert_eq!(small_first.last().unwrap().name, "large.txt");
    }

    #[test]
    fn hidden_files_are_filtered_unless_requested() {
        let dir = fixture();
        let visible = read_entries(
            dir.path(),
            false,
            SortBy::Name,
            SortBy::Name.natural_order(),
        )
        .unwrap();
        assert!(!names(&visible).contains(&".hidden".to_string()));

        let all =
            read_entries(dir.path(), true, SortBy::Name, SortBy::Name.natural_order()).unwrap();
        assert!(names(&all).contains(&".hidden".to_string()));
    }

    #[test]
    fn parent_entry_is_first_and_dirs_precede_files() {
        let dir = fixture();
        let entries = read_entries(
            dir.path(),
            false,
            SortBy::Name,
            SortBy::Name.natural_order(),
        )
        .unwrap();
        let listed = names(&entries);

        assert_eq!(listed[0], "..");
        assert_eq!(listed[1], "alpha_dir");
        assert_eq!(listed[2], "zeta_dir");
        // Files follow, sorted by name.
        assert_eq!(listed[3], "a.rs");
        assert_eq!(listed[4], "b.txt");
    }

    #[test]
    fn sort_by_size_is_descending_for_files() {
        let dir = fixture();
        let entries = read_entries(
            dir.path(),
            false,
            SortBy::Size,
            SortBy::Size.natural_order(),
        )
        .unwrap();
        let files: Vec<&Entry> = entries.iter().filter(|e| !e.is_dir()).collect();
        // a.rs is 10 bytes, b.txt is 5.
        assert_eq!(files[0].name, "a.rs");
        assert_eq!(files[1].name, "b.txt");
    }

    #[test]
    fn sort_by_ext_groups_extensions() {
        let dir = fixture();
        let entries =
            read_entries(dir.path(), false, SortBy::Ext, SortBy::Ext.natural_order()).unwrap();
        let files: Vec<String> = entries
            .iter()
            .filter(|e| !e.is_dir())
            .map(|e| e.name.clone())
            .collect();
        // ".rs" sorts before ".txt".
        assert_eq!(files, vec!["a.rs", "b.txt"]);
    }

    #[test]
    fn entering_and_leaving_a_directory_restores_the_cursor() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());

        let zeta = panel
            .entries
            .iter()
            .position(|e| e.name == "zeta_dir")
            .unwrap();
        panel.cursor_to(zeta);
        assert!(panel.enter());
        assert_eq!(panel.cwd, dir.path().join("zeta_dir"));

        // ".." is the only entry inside; going back should re-select zeta_dir.
        assert!(panel.enter());
        assert_eq!(panel.cwd, dir.path());
        assert_eq!(panel.selected().unwrap().name, "zeta_dir");
    }

    #[test]
    fn enter_on_a_file_is_a_no_op() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());
        let idx = panel
            .entries
            .iter()
            .position(|e| e.name == "b.txt")
            .unwrap();
        panel.cursor_to(idx);

        assert!(!panel.enter());
        assert_eq!(panel.cwd, dir.path());
    }

    #[test]
    fn cursor_movement_is_clamped() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());
        let len = panel.entries.len();

        panel.move_cursor(-100);
        assert_eq!(panel.cursor, 0);

        panel.move_cursor(1000);
        assert_eq!(panel.cursor, len - 1);

        panel.cursor_home();
        assert_eq!(panel.cursor, 0);

        panel.cursor_end();
        assert_eq!(panel.cursor, len - 1);
    }

    #[test]
    fn marking_skips_parent_and_advances() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());

        // Cursor starts on "..", which must never become marked.
        assert!(panel.selected().unwrap().is_parent());
        panel.toggle_mark();
        assert_eq!(panel.marked_count(), 0);
        assert_eq!(panel.cursor, 1);

        panel.toggle_mark();
        assert_eq!(panel.marked_count(), 1);
        assert_eq!(panel.cursor, 2);
    }

    #[test]
    fn action_targets_prefers_marks_then_cursor() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());

        // No marks: the cursor entry is the target, but never "..".
        panel.cursor_home();
        assert!(panel.action_targets().is_empty());

        let idx = panel.entries.iter().position(|e| e.name == "a.rs").unwrap();
        panel.cursor_to(idx);
        assert_eq!(panel.action_targets(), vec![dir.path().join("a.rs")]);

        // With marks, the cursor is ignored.
        panel.cursor_to(1);
        panel.toggle_mark();
        panel.cursor_to(2);
        panel.toggle_mark();
        assert_eq!(panel.action_targets().len(), 2);
    }

    #[test]
    fn marks_survive_a_reload() {
        let dir = fixture();
        let mut panel = Panel::new(dir.path().to_path_buf());
        let idx = panel.entries.iter().position(|e| e.name == "a.rs").unwrap();
        panel.cursor_to(idx);
        panel.toggle_mark();
        assert_eq!(panel.marked_count(), 1);

        panel.reload();
        assert_eq!(panel.marked_count(), 1);
        assert!(
            panel
                .entries
                .iter()
                .find(|e| e.name == "a.rs")
                .unwrap()
                .marked
        );
    }

    #[test]
    fn unreadable_directory_reports_an_error_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let panel = Panel::new(missing);
        assert!(panel.error.is_some());
    }

    // ---- noticing what something else did --------------------------------

    /// A panel on a directory of its own, freshly read.
    fn watching(dir: &Path) -> Panel {
        let mut panel = Panel::new(dir.to_path_buf());
        panel.reload();
        panel
    }

    #[test]
    fn nothing_changing_is_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        let mut panel = watching(dir.path());

        assert!(!panel.poll_changes());
        assert!(!panel.poll_changes(), "it kept finding a change");
    }

    #[test]
    fn a_file_appearing_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let mut panel = watching(dir.path());
        assert_eq!(panel.entries.len(), 1); // just `..`

        fs::write(dir.path().join("new.txt"), "x").unwrap();
        assert!(panel.poll_changes(), "the new file went unnoticed");
        assert!(panel.entries.iter().any(|e| e.name == "new.txt"));
        // ...and the panel settles rather than reloading for ever.
        assert!(!panel.poll_changes());
    }

    #[test]
    fn a_file_vanishing_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let doomed = dir.path().join("gone.txt");
        fs::write(&doomed, "x").unwrap();
        let mut panel = watching(dir.path());

        fs::remove_file(&doomed).unwrap();
        assert!(panel.poll_changes());
        assert!(!panel.entries.iter().any(|e| e.name == "gone.txt"));
    }

    #[test]
    fn a_file_growing_is_noticed_even_though_the_directory_did_not_change() {
        // The case the directory's own timestamp misses: appending to a file
        // that is already there moves nothing about the directory itself, so
        // the size column would sit there being wrong.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("build.log");
        fs::write(&log, "one line\n").unwrap();
        let mut panel = watching(dir.path());
        let before = panel
            .entries
            .iter()
            .find(|e| e.name == "build.log")
            .unwrap()
            .size;

        fs::write(&log, "one line\nand another\n").unwrap();
        assert!(panel.poll_changes(), "the file grew unnoticed");
        let after = panel
            .entries
            .iter()
            .find(|e| e.name == "build.log")
            .unwrap()
            .size;
        assert!(after > before, "{before} -> {after}");
    }

    #[test]
    fn a_refresh_keeps_the_cursor_and_the_marks() {
        // A listing that refreshed itself by throwing away where you were
        // would be worse than a stale one.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(dir.path().join(name), "x").unwrap();
        }
        let mut panel = watching(dir.path());
        let b = panel
            .entries
            .iter()
            .position(|e| e.name == "b.txt")
            .unwrap();
        panel.cursor_to(b);
        panel.entries[b].marked = true;

        fs::write(dir.path().join("zz-new.txt"), "x").unwrap();
        assert!(panel.poll_changes());

        assert_eq!(panel.selected().map(|e| e.name.as_str()), Some("b.txt"));
        assert!(panel.selected().unwrap().marked, "the mark was lost");
    }

    #[test]
    fn a_directory_that_disappears_reads_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("here");
        fs::create_dir(&inner).unwrap();
        let mut panel = watching(&inner);

        fs::remove_dir(&inner).unwrap();
        assert!(
            panel.poll_changes(),
            "the panel sat on a directory that had gone"
        );
        assert!(panel.error.is_some());
    }

    #[test]
    fn a_tree_is_left_to_its_own_refresh() {
        // A tree is a view of a filesystem rather than of one directory, and
        // it has a refresh of its own.
        let dir = tempfile::tempdir().unwrap();
        let mut panel = watching(dir.path());
        panel.enter_tree_mode();

        fs::write(dir.path().join("new.txt"), "x").unwrap();
        assert!(!panel.poll_changes());
    }

    #[test]
    fn a_big_directory_costs_one_look_rather_than_one_per_entry() {
        // Reading a directory is one syscall however many entries it has;
        // asking each entry for its size is one each. Past the limit the
        // detail is dropped - so the signature is the count and the
        // directory's own time, and it must not depend on the order the
        // entries happened to come back in.
        let dir = tempfile::tempdir().unwrap();
        for n in 0..10 {
            fs::write(dir.path().join(format!("f{n}.txt")), "x").unwrap();
        }
        let small = signature(dir.path(), 100);
        let big = signature(dir.path(), 5);

        assert_eq!(small.count, big.count);
        assert!(small.bytes > 0 && small.newest.is_some());
        assert_eq!(big.bytes, 0, "detail kept past the limit");
        assert_eq!(big.newest, None);
        // Repeating it gives the same answer, whatever the read order.
        assert_eq!(signature(dir.path(), 5), big);
    }

    #[test]
    fn an_unreadable_directory_still_has_a_signature() {
        let missing = Path::new("/no/such/directory/at/all");
        assert_eq!(signature(missing, 100), Signature::default());
    }

    /// Build two directories with a file each, and a panel showing the first.
    fn two_directories() -> (tempfile::TempDir, Panel) {
        let root = tempfile::tempdir().unwrap();
        for dir in ["one", "two"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
            std::fs::write(root.path().join(dir).join("same-name.txt"), b"x").unwrap();
        }
        let panel = Panel::new(root.path().join("one"));
        (root, panel)
    }

    #[test]
    fn a_tag_survives_walking_to_another_directory_and_back() {
        let (root, mut panel) = two_directories();
        panel.enter_tree_mode();

        let at = panel
            .entries
            .iter()
            .position(|e| e.name == "same-name.txt")
            .unwrap();
        panel.cursor_to(at);
        panel.toggle_mark();
        assert_eq!(panel.tagged_count(), 1);

        // Away, and the mark is not on any row here...
        panel.cwd = root.path().join("two");
        panel.reload();
        assert!(
            panel.entries.iter().all(|e| !e.marked),
            "a file of the same name in another directory is a different file"
        );
        assert_eq!(panel.tagged_count(), 1, "and the tag is still held");

        // ...and back, where it is.
        panel.cwd = root.path().join("one");
        panel.reload();
        assert!(
            panel
                .entries
                .iter()
                .any(|e| e.marked && e.name == "same-name.txt"),
            "the tag comes back onto the row it was made on"
        );
    }

    #[test]
    fn untagging_sticks_rather_than_coming_back_on_the_next_reload() {
        // The failure this guards is subtle: if unmarking only cleared the
        // row and not the set, the file would look unmarked until anything
        // re-read the directory and then quietly mark itself again.
        let (_root, mut panel) = two_directories();
        panel.enter_tree_mode();

        let at = panel
            .entries
            .iter()
            .position(|e| e.name == "same-name.txt")
            .unwrap();
        panel.cursor_to(at);
        panel.toggle_mark();
        panel.cursor_to(at);
        panel.toggle_mark();

        assert_eq!(panel.tagged_count(), 0);
        panel.reload();
        assert!(panel.entries.iter().all(|e| !e.marked));
    }

    #[test]
    fn tags_do_not_outlive_the_tree_that_made_them_reachable() {
        let (_root, mut panel) = two_directories();
        panel.enter_tree_mode();
        panel.mark_all();
        assert!(panel.tagged_count() > 0);

        panel.leave_tree_mode();
        // Without a tree every marked file is on screen. A set spread over
        // directories the reader can no longer reach is one they cannot undo.
        assert_eq!(panel.tagged_count(), 0);
    }

    #[test]
    fn without_a_tree_nothing_is_tagged_at_all() {
        // The whole point of gating on the tree: an ordinary two-pane listing
        // gains no lasting set, so nothing about it changes.
        let (root, mut panel) = two_directories();
        panel.mark_all();
        assert_eq!(panel.tagged_count(), 0, "no tree, no lasting set");

        // Not asserted here: whether the *rows* keep their marks. `reload`
        // has always restored them by name, and both directories hold a
        // `same-name.txt` - which is exactly the confusion the tagged set
        // exists to avoid, and is pre-existing behaviour either way.
        panel.cwd = root.path().join("two");
        panel.reload();
        assert_eq!(panel.tagged_count(), 0);
    }

    #[test]
    fn the_parent_row_is_never_tagged() {
        let (_root, mut panel) = two_directories();
        panel.enter_tree_mode();
        panel.mark_all();
        panel.invert_marks();
        panel.mark_range(0, panel.entries.len(), true);

        // `..` is not a file, and a job that tried to copy it would be acting
        // on the directory the reader is standing in.
        assert!(panel.tagged_paths().iter().all(|p| !p.ends_with("..")));
    }
}

#[cfg(test)]
mod inside_an_archive {
    use super::*;

    /// A real zip, made by the system's own `zip`, with two levels in it.
    fn with_a_zip() -> Option<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("loose.txt"), "beside it\n").unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir_all(build.join("docs/deep")).unwrap();
        std::fs::write(build.join("readme.txt"), "at the top\n").unwrap();
        std::fs::write(build.join("docs/notes.txt"), "in a folder\n").unwrap();
        std::fs::write(build.join("docs/deep/buried.txt"), "two down\n").unwrap();

        let made = std::process::Command::new("sh")
            .arg("-c")
            .arg("zip -qr ../papers.zip readme.txt docs")
            .current_dir(&build)
            .status();
        let archive = dir.path().join("papers.zip");
        match matches!(made, Ok(s) if s.success()) && archive.exists() {
            true => Some((dir, archive)),
            false => None,
        }
    }

    fn names(panel: &Panel) -> Vec<String> {
        panel.entries.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn walking_in_and_around_and_back_out() {
        let Some((dir, archive)) = with_a_zip() else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        let mut panel = Panel::new(dir.path().to_path_buf());
        assert!(!panel.in_archive());

        // In.
        panel.open_archive(&archive, None).expect("opens");
        assert!(panel.in_archive());
        assert_eq!(names(&panel), vec!["..", "docs", "readme.txt"]);
        // The path shown is the archive itself, which is what a breadcrumb
        // and a title want.
        assert_eq!(panel.cwd, archive);

        // Down a level.
        panel.cursor = names(&panel).iter().position(|n| n == "docs").unwrap();
        assert!(panel.enter());
        assert_eq!(names(&panel), vec!["..", "deep", "notes.txt"]);
        assert_eq!(panel.cwd, archive.join("docs"));

        // And another.
        panel.cursor = names(&panel).iter().position(|n| n == "deep").unwrap();
        assert!(panel.enter());
        assert_eq!(names(&panel), vec!["..", "buried.txt"]);

        // Back up, landing on where we came from.
        panel.cursor = 0;
        assert!(panel.enter());
        assert_eq!(panel.selected().unwrap().name, "deep", "landed on it");
        assert!(panel.in_archive());

        // Up again: still inside, at the top.
        panel.cursor = 0;
        assert!(panel.enter());
        assert_eq!(panel.selected().unwrap().name, "docs");

        // And out, landing on the archive in the folder that holds it.
        panel.cursor = 0;
        assert!(panel.enter());
        assert!(!panel.in_archive(), "left the archive");
        assert_eq!(panel.cwd, dir.path());
        assert_eq!(panel.selected().unwrap().name, "papers.zip");
    }

    #[test]
    fn a_member_keeps_its_size_and_a_directory_has_none() {
        let Some((dir, archive)) = with_a_zip() else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        let mut panel = Panel::new(dir.path().to_path_buf());
        panel.open_archive(&archive, None).unwrap();

        let readme = panel
            .entries
            .iter()
            .find(|e| e.name == "readme.txt")
            .expect("readme");
        assert_eq!(readme.size, 11);
        assert_eq!(readme.kind, EntryKind::File);
        assert!(readme.modified.is_some(), "a zip carries dates");

        let docs = panel.entries.iter().find(|e| e.name == "docs").unwrap();
        assert!(docs.is_dir());
    }

    #[test]
    fn the_member_path_comes_back_for_reading_it() {
        let Some((dir, archive)) = with_a_zip() else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        let mut panel = Panel::new(dir.path().to_path_buf());
        panel.open_archive(&archive, None).unwrap();
        panel.cursor = names(&panel).iter().position(|n| n == "docs").unwrap();
        panel.enter();

        let notes = panel
            .entries
            .iter()
            .find(|e| e.name == "notes.txt")
            .unwrap()
            .clone();
        assert_eq!(panel.member_of(&notes).as_deref(), Some("docs/notes.txt"));
        // And it really reads.
        let bytes = crate::archive::read(&archive, "docs/notes.txt").unwrap();
        assert_eq!(bytes, b"in a folder\n".to_vec());
    }

    #[test]
    fn a_reload_keeps_the_level_being_looked_at() {
        // Panels reload on a timer and after every operation. One that forgot
        // where it was would throw the user back to the archive's top level
        // in the middle of working two levels down.
        let Some((dir, archive)) = with_a_zip() else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        let mut panel = Panel::new(dir.path().to_path_buf());
        panel.open_archive(&archive, None).unwrap();
        panel.cursor = names(&panel).iter().position(|n| n == "docs").unwrap();
        panel.enter();
        assert_eq!(panel.inside.as_ref().unwrap().at, "docs");

        panel.reload();
        assert_eq!(panel.inside.as_ref().unwrap().at, "docs", "after a reload");
        assert!(names(&panel).contains(&"notes.txt".to_string()));

        // And the timer's own check does not walk it out either.
        panel.poll_changes();
        assert_eq!(panel.inside.as_ref().unwrap().at, "docs", "after a poll");
        assert!(panel.in_archive());
    }

    #[test]
    fn asking_for_a_directory_leaves_the_archive() {
        // chdir means a directory. A panel left half-inside would list an
        // archive's members under a folder's path.
        let Some((dir, archive)) = with_a_zip() else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        let mut panel = Panel::new(dir.path().to_path_buf());
        panel.open_archive(&archive, None).unwrap();
        assert!(panel.in_archive());

        panel.chdir(dir.path().to_path_buf());
        assert!(!panel.in_archive());
        assert!(names(&panel).contains(&"loose.txt".to_string()));
    }

    #[test]
    fn something_that_is_not_an_archive_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "just text\n").unwrap();
        let mut panel = Panel::new(dir.path().to_path_buf());
        assert!(panel.open_archive(&plain, None).is_err());
        assert!(!panel.in_archive(), "and the panel is left where it was");
    }
}
