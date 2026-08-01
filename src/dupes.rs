//! Finding files that are the same file twice.
//!
//! The answer has to be exact, because what anyone does with it is delete
//! something. So nothing here reports a duplicate it has not read both copies
//! of: hashing is used to *narrow*, never to conclude, and every group is
//! confirmed byte for byte before it is shown. A hash collision is unlikely;
//! deleting the wrong photograph because of one is not a risk worth taking to
//! save a second pass.
//!
//! The work is arranged so that most files are never read at all:
//!
//! 1. Walk the tree collecting names and sizes. No file is opened.
//! 2. Group by size. Two files of different lengths cannot be copies, and in
//!    a real directory nearly every size is unique - those files are done.
//! 3. Only inside a size group is anything read: hash each, group by hash.
//! 4. Confirm each hash group with a byte comparison, and report it.
//!
//! Hard links are left out of it. Two names for one inode are trivially
//! identical and deleting one reclaims nothing, so offering them as
//! duplicates would be offering to do nothing and call it tidying.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// How many groups a scan collects before it calls it enough.
pub const MAX_GROUPS: usize = 5_000;

/// How much is read at a time when hashing.
const CHUNK: usize = 64 * 1024;

/// FNV-1a, which is short, has no dependencies, and is only ever used to
/// decide which files are worth comparing properly.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// What a scan should look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub include_hidden: bool,
    /// Files smaller than this are not worth reporting. Empty files are all
    /// copies of each other, which is true and useless.
    pub smallest: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include_hidden: false,
            smallest: 1,
        }
    }
}

/// One copy, and whether it is one of the ones to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub path: PathBuf,
    /// Ticked by hand. Nothing is ticked to start with.
    pub remove: bool,
}

/// A set of files that are byte for byte the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// What each copy weighs.
    pub size: u64,
    pub copies: Vec<Copy>,
}

impl Group {
    pub fn new(size: u64, paths: Vec<PathBuf>) -> Group {
        Group {
            size,
            copies: paths
                .into_iter()
                .map(|path| Copy {
                    path,
                    remove: false,
                })
                .collect(),
        }
    }

    pub fn keeping(&self) -> usize {
        self.copies.iter().filter(|c| !c.remove).count()
    }

    /// Whether this one can still be ticked.
    ///
    /// The last copy cannot: a duplicate finder that will delete every copy
    /// of a file is not a duplicate finder, it is a delete key with extra
    /// steps.
    pub fn can_remove(&self, index: usize) -> bool {
        match self.copies.get(index) {
            Some(copy) => copy.remove || self.keeping() > 1,
            None => false,
        }
    }

    /// Tick or untick one copy, refusing to tick the last one kept.
    pub fn toggle(&mut self, index: usize) -> bool {
        if !self.can_remove(index) {
            return false;
        }
        if let Some(copy) = self.copies.get_mut(index) {
            copy.remove = !copy.remove;
            return true;
        }
        false
    }

    /// Tick everything but the first, which is the whole point in one press.
    ///
    /// The first is the one the walk found first, and the walk is in name
    /// order - so the copy that is kept is the same one every time rather
    /// than whichever the filesystem happened to hand over first.
    pub fn keep_first(&mut self) {
        for (at, copy) in self.copies.iter_mut().enumerate() {
            copy.remove = at > 0;
        }
    }

    pub fn keep_all(&mut self) {
        for copy in self.copies.iter_mut() {
            copy.remove = false;
        }
    }

    /// What ticking these would give back.
    pub fn reclaimed(&self) -> u64 {
        self.copies.iter().filter(|c| c.remove).count() as u64 * self.size
    }
}

/// Everything ticked across every group.
pub fn to_remove(groups: &[Group]) -> Vec<PathBuf> {
    groups
        .iter()
        .flat_map(|group| {
            group
                .copies
                .iter()
                .filter(|c| c.remove)
                .map(|c| c.path.clone())
        })
        .collect()
}

/// How many copies are ticked, without building the list of them.
///
/// A dialog asks this every frame to label its button; building a vector of
/// paths sixty times a second to count them would be work for nothing.
pub fn ticked(groups: &[Group]) -> usize {
    groups
        .iter()
        .map(|group| group.copies.iter().filter(|c| c.remove).count())
        .sum()
}

/// What the whole selection would give back.
pub fn reclaimed(groups: &[Group]) -> u64 {
    groups.iter().map(Group::reclaimed).sum()
}

/// What could be given back if every group kept one copy.
///
/// The number anyone actually wants: not how much these files weigh, but how
/// much of it is the same thing over again.
pub fn wasted(groups: &[Group]) -> u64 {
    groups
        .iter()
        .map(|group| (group.copies.len() as u64 - 1) * group.size)
        .sum()
}

/// A file the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    /// What the filesystem calls this file, where it says. Two names sharing
    /// one are two names for one file rather than two files.
    pub identity: Option<(u64, u64)>,
}

/// FNV-1a over a file's contents.
pub fn hash(path: &Path) -> io::Result<u64> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; CHUNK];
    let mut sum = FNV_OFFSET;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(sum);
        }
        for byte in &buffer[..read] {
            sum ^= *byte as u64;
            sum = sum.wrapping_mul(FNV_PRIME);
        }
    }
}

/// Drop the names that are only other names for a file already in the list.
///
/// Order is kept, so the first name found is the one that stays. Through a
/// set rather than a list: this runs once per file in the whole tree, and a
/// linear scan here is the difference between a home directory taking a
/// second and taking a minute.
pub fn without_hard_links(files: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut out = Vec::new();
    for file in files {
        match file.identity {
            Some(id) if !seen.insert(id) => {}
            _ => out.push(file),
        }
    }
    out
}

/// Group by size, keeping only the sizes more than one file has.
///
/// The step that means most files are never opened: in a real directory
/// nearly every size belongs to exactly one file.
pub fn by_size(files: Vec<Candidate>) -> Vec<Vec<Candidate>> {
    let mut by: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for file in files {
        by.entry(file.size).or_default().push(file);
    }
    let mut groups: Vec<Vec<Candidate>> = by.into_values().filter(|g| g.len() > 1).collect();
    // Largest first: the biggest waste is what anyone came here for.
    groups.sort_by(|a, b| b[0].size.cmp(&a[0].size).then(a[0].path.cmp(&b[0].path)));
    for group in groups.iter_mut() {
        group.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups
}

/// Split one same-size set into sets that are actually identical.
///
/// Hashed to narrow and then compared byte for byte to be sure - see the note
/// at the top of this module.
pub fn confirm(files: &[Candidate]) -> Vec<Group> {
    // Keyed rather than searched: a size group can be large - a thousand
    // files of exactly 4 KiB is an ordinary thing for a build directory to
    // contain - and scanning the groups so far for each one is quadratic in
    // the size of the very case this is meant to handle.
    let mut by_hash: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut order: Vec<u64> = Vec::new();
    for file in files {
        let Ok(sum) = hash(&file.path) else { continue };
        let paths = by_hash.entry(sum).or_insert_with(|| {
            order.push(sum);
            Vec::new()
        });
        paths.push(file.path.clone());
    }

    let size = files.first().map(|f| f.size).unwrap_or(0);
    let mut out = Vec::new();
    for paths in order.into_iter().filter_map(|sum| by_hash.remove(&sum)) {
        if paths.len() < 2 {
            continue;
        }
        for group in verified(&paths) {
            if group.len() > 1 {
                out.push(Group::new(size, group));
            }
        }
    }
    out
}

/// Split a set of same-hash files into sets that really do match.
///
/// Very nearly always one set: this is here for the collision that has not
/// happened yet rather than for the common case.
fn verified(paths: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let mut sets: Vec<Vec<PathBuf>> = Vec::new();
    for path in paths {
        let mut placed = false;
        for set in sets.iter_mut() {
            if crate::compare::same_content(&set[0], path).unwrap_or(false) {
                set.push(path.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            sets.push(vec![path.clone()]);
        }
    }
    sets
}

/// One line of the list: a group's heading, or one of its copies.
///
/// The lists in both front-ends are one flat column - a heading, then the
/// copies under it, then the next heading - so the cursor has one thing to
/// walk and the flattening is worked out once here rather than twice there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Heading { group: usize },
    Copy { group: usize, copy: usize },
}

pub fn lines(groups: &[Group]) -> Vec<Line> {
    let mut out = Vec::new();
    for (group, set) in groups.iter().enumerate() {
        out.push(Line::Heading { group });
        for copy in 0..set.copies.len() {
            out.push(Line::Copy { group, copy });
        }
    }
    out
}

/// What the space bar does to the line under the cursor.
///
/// On a copy it ticks or unticks that one. On a heading it does the thing
/// anyone opened this window for - keep the first and let the rest go - and
/// pressing it again puts them all back, so the gesture is its own undo.
pub fn toggle_at(groups: &mut [Group], line: Line) {
    match line {
        Line::Copy { group, copy } => {
            if let Some(set) = groups.get_mut(group) {
                set.toggle(copy);
            }
        }
        Line::Heading { group } => {
            if let Some(set) = groups.get_mut(group) {
                if set.keeping() == set.copies.len() {
                    set.keep_first();
                } else {
                    set.keep_all();
                }
            }
        }
    }
}

/// Where a scan reports to, and asks whether to stop.
pub trait Sink {
    /// A set of copies was confirmed. False stops the scan.
    fn group(&mut self, group: Group) -> bool;
    /// Where it has got to, for the line that says it is still going.
    fn looking_at(&mut self, what: &str);
    fn cancelled(&self) -> bool;
}

/// Collect every file under `root`, without opening any of them.
pub fn collect(root: &Path, options: &Options, sink: &mut dyn Sink) -> Vec<Candidate> {
    let mut out = Vec::new();
    walk_into(root, options, sink, &mut out);
    out
}

fn walk_into(dir: &Path, options: &Options, sink: &mut dyn Sink, out: &mut Vec<Candidate>) {
    if sink.cancelled() {
        return;
    }
    sink.looking_at(&dir.display().to_string());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut listing: Vec<_> = entries.flatten().collect();
    listing.sort_by_key(|e| e.file_name());

    for entry in listing {
        if sink.cancelled() {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !options.include_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        if meta.is_dir() {
            // Not through a symlink: a link back up the tree would walk for
            // ever, and a link across it would report a file as a copy of
            // itself.
            if !entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                walk_into(&path, options, sink, out);
            }
            continue;
        }
        if !meta.is_file() || meta.len() < options.smallest {
            continue;
        }
        out.push(Candidate {
            path,
            size: meta.len(),
            identity: identity(&meta),
        });
    }
}

#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Find every set of identical files under `root`.
pub fn walk(root: &Path, options: &Options, sink: &mut dyn Sink) {
    let files = without_hard_links(collect(root, options, sink));
    for candidates in by_size(files) {
        if sink.cancelled() {
            return;
        }
        sink.looking_at(&format!(
            "{} files of {}",
            candidates.len(),
            crate::entry::human_size(candidates[0].size)
        ));
        for group in confirm(&candidates) {
            if !sink.group(group) {
                return;
            }
        }
    }
}

/// A scan in progress, and what it has found.
#[derive(Debug, Clone, Default)]
pub struct Duplicates {
    pub groups: Vec<Group>,
    pub current: String,
    pub finished: bool,
    pub cancelled: bool,
    /// It stopped at [`MAX_GROUPS`] rather than because it ran out of tree.
    pub truncated: bool,
}

struct SharedSink {
    found: Arc<Mutex<Duplicates>>,
    cancel: Arc<AtomicBool>,
}

impl Sink for SharedSink {
    fn group(&mut self, group: Group) -> bool {
        let mut guard = lock(&self.found);
        if guard.groups.len() >= MAX_GROUPS {
            guard.truncated = true;
            return false;
        }
        guard.groups.push(group);
        true
    }

    fn looking_at(&mut self, what: &str) {
        lock(&self.found).current = what.to_string();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn lock<T>(value: &Arc<Mutex<T>>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}

/// A duplicate scan running on its own thread.
pub struct Scan {
    found: Arc<Mutex<Duplicates>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub root: PathBuf,
}

impl Scan {
    pub fn spawn(root: PathBuf, options: Options) -> Scan {
        let found: Arc<Mutex<Duplicates>> = Arc::default();
        let cancel = Arc::new(AtomicBool::new(false));

        let worker_found = Arc::clone(&found);
        let worker_cancel = Arc::clone(&cancel);
        let worker_root = root.clone();

        let handle = std::thread::spawn(move || {
            let mut sink = SharedSink {
                found: Arc::clone(&worker_found),
                cancel: Arc::clone(&worker_cancel),
            };
            walk(&worker_root, &options, &mut sink);

            let mut guard = lock(&worker_found);
            guard.cancelled = worker_cancel.load(Ordering::Relaxed);
            guard.finished = true;
            guard.current.clear();
        });

        Scan {
            found,
            cancel,
            handle: Some(handle),
            root,
        }
    }

    pub fn snapshot(&self) -> Duplicates {
        lock(&self.found).clone()
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.found).finished
    }

    pub fn request_stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Scan {
    /// A window closed mid-scan must not leave a thread reading a disk.
    fn drop(&mut self) {
        self.request_stop();
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, size: u64) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            size,
            identity: None,
        }
    }

    /// A tree with a known set of duplicates in it.
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("deep/deeper");
        std::fs::create_dir_all(&deep).unwrap();

        // Three copies of one thing, in three places.
        for path in [
            root.path().join("a.txt"),
            root.path().join("deep/b.txt"),
            deep.join("c.txt"),
        ] {
            std::fs::write(path, "the very same contents\n").unwrap();
        }
        // Two copies of another.
        for path in [root.path().join("x.bin"), root.path().join("deep/y.bin")] {
            std::fs::write(path, vec![7u8; 4096]).unwrap();
        }
        // The same length as x.bin, and not the same bytes - the case that
        // size alone would get wrong.
        std::fs::write(root.path().join("z.bin"), vec![9u8; 4096]).unwrap();
        // On its own.
        std::fs::write(root.path().join("only.txt"), "unique\n").unwrap();
        // Hidden, and empty.
        std::fs::write(root.path().join(".hidden"), "the very same contents\n").unwrap();
        std::fs::write(root.path().join("empty-one"), "").unwrap();
        std::fs::write(root.path().join("empty-two"), "").unwrap();
        root
    }

    struct Collect(Vec<Group>);
    impl Sink for Collect {
        fn group(&mut self, group: Group) -> bool {
            self.0.push(group);
            true
        }
        fn looking_at(&mut self, _: &str) {}
        fn cancelled(&self) -> bool {
            false
        }
    }

    /// The groups a walk finds, as sorted lists of names.
    fn found(root: &Path, options: &Options) -> Vec<Vec<String>> {
        let mut sink = Collect(Vec::new());
        walk(root, options, &mut sink);
        let mut out: Vec<Vec<String>> = sink
            .0
            .iter()
            .map(|group| {
                let mut names: Vec<String> = group
                    .copies
                    .iter()
                    .map(|c| c.path.file_name().unwrap().to_string_lossy().to_string())
                    .collect();
                names.sort();
                names
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn identical_files_are_found_however_deep_they_are() {
        let root = fixture();
        assert_eq!(
            found(root.path(), &Options::default()),
            [
                vec!["a.txt".to_string(), "b.txt".into(), "c.txt".into()],
                vec!["x.bin".to_string(), "y.bin".into()],
            ]
        );
    }

    #[test]
    fn two_files_of_one_size_are_not_two_copies() {
        // x.bin and z.bin are both 4096 bytes and share not one byte of
        // content. Grouping by size alone would call them copies; reading
        // them is the only thing that answers it.
        let root = fixture();
        let groups = found(root.path(), &Options::default());
        let together = groups
            .iter()
            .any(|g| g.contains(&"z.bin".to_string()) && g.contains(&"x.bin".to_string()));
        assert!(!together, "{groups:?}");
    }

    #[test]
    fn empty_files_are_left_out_unless_asked_for() {
        let root = fixture();
        let default = found(root.path(), &Options::default());
        assert!(
            !default.iter().any(|g| g.contains(&"empty-one".to_string())),
            "every empty file is a copy of every other, which is true and useless"
        );

        let everything = found(
            root.path(),
            &Options {
                smallest: 0,
                ..Options::default()
            },
        );
        assert!(everything
            .iter()
            .any(|g| g.contains(&"empty-one".to_string())));
    }

    #[test]
    fn hidden_files_are_left_out_unless_asked_for() {
        let root = fixture();
        let without = found(root.path(), &Options::default());
        assert!(!without.iter().any(|g| g.contains(&".hidden".to_string())));

        let with = found(
            root.path(),
            &Options {
                include_hidden: true,
                ..Options::default()
            },
        );
        assert!(with.iter().any(|g| g.contains(&".hidden".to_string())));
    }

    #[cfg(unix)]
    #[test]
    fn two_names_for_one_file_are_not_two_files() {
        // Deleting one of them reclaims nothing, so offering them as
        // duplicates would be offering to do nothing and call it tidying.
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original.txt");
        std::fs::write(&original, "shared\n").unwrap();
        std::fs::hard_link(&original, root.path().join("another-name.txt")).unwrap();

        assert!(
            found(root.path(), &Options::default()).is_empty(),
            "a hard link is the same file, not a copy of it"
        );

        // A real copy beside them is still found, and only once.
        std::fs::write(root.path().join("copy.txt"), "shared\n").unwrap();
        let groups = found(root.path(), &Options::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn grouping_by_size_drops_the_sizes_only_one_file_has() {
        let files = vec![
            candidate("/a", 10),
            candidate("/b", 20),
            candidate("/c", 10),
            candidate("/d", 30),
            candidate("/e", 30),
            candidate("/f", 30),
        ];
        let groups = by_size(files);
        assert_eq!(groups.len(), 2, "20 belongs to one file and is dropped");
        assert_eq!(groups[0].len(), 3, "largest first");
        assert_eq!(groups[0][0].size, 30);
        assert_eq!(groups[1][0].size, 10);
    }

    #[test]
    fn a_group_will_not_let_go_of_its_last_copy() {
        let mut group = Group::new(100, vec!["/a".into(), "/b".into()]);
        assert_eq!(group.keeping(), 2);
        assert_eq!(group.reclaimed(), 0, "nothing is ticked to start with");

        assert!(group.toggle(0));
        assert_eq!(group.keeping(), 1);
        assert_eq!(group.reclaimed(), 100);

        assert!(
            !group.toggle(1),
            "the last copy kept cannot be ticked as well"
        );
        assert_eq!(group.keeping(), 1);

        // Ticking one back off is always allowed.
        assert!(group.toggle(0));
        assert_eq!(group.keeping(), 2);
    }

    #[test]
    fn keeping_the_first_ticks_everything_else() {
        let mut group = Group::new(50, vec!["/a".into(), "/b".into(), "/c".into()]);
        group.keep_first();
        assert_eq!(group.keeping(), 1);
        assert!(!group.copies[0].remove);
        assert!(group.copies[1].remove && group.copies[2].remove);
        assert_eq!(group.reclaimed(), 100);

        group.keep_all();
        assert_eq!(group.keeping(), 3);
        assert_eq!(group.reclaimed(), 0);
    }

    #[test]
    fn the_numbers_say_what_is_there_and_what_was_asked_for() {
        let mut groups = vec![
            Group::new(1000, vec!["/a".into(), "/b".into(), "/c".into()]),
            Group::new(500, vec!["/d".into(), "/e".into()]),
        ];
        assert_eq!(wasted(&groups), 2000 + 500, "what is the same thing twice");
        assert_eq!(reclaimed(&groups), 0, "before anything is ticked");
        assert!(to_remove(&groups).is_empty());

        assert_eq!(ticked(&groups), 0);

        groups[0].keep_first();
        assert_eq!(reclaimed(&groups), 2000);
        assert_eq!(to_remove(&groups), [PathBuf::from("/b"), "/c".into()]);
        assert_eq!(ticked(&groups), 2, "counted without building the list");
    }

    #[test]
    fn the_list_is_a_heading_and_then_its_copies() {
        let groups = vec![
            Group::new(10, vec!["/a".into(), "/b".into()]),
            Group::new(20, vec!["/c".into(), "/d".into(), "/e".into()]),
        ];
        assert_eq!(
            lines(&groups),
            [
                Line::Heading { group: 0 },
                Line::Copy { group: 0, copy: 0 },
                Line::Copy { group: 0, copy: 1 },
                Line::Heading { group: 1 },
                Line::Copy { group: 1, copy: 0 },
                Line::Copy { group: 1, copy: 1 },
                Line::Copy { group: 1, copy: 2 },
            ]
        );
        assert!(lines(&[]).is_empty());
    }

    #[test]
    fn the_space_bar_ticks_a_copy_and_thins_a_whole_group() {
        let mut groups = vec![Group::new(10, vec!["/a".into(), "/b".into(), "/c".into()])];

        toggle_at(&mut groups, Line::Copy { group: 0, copy: 1 });
        assert!(groups[0].copies[1].remove);
        toggle_at(&mut groups, Line::Copy { group: 0, copy: 1 });
        assert!(!groups[0].copies[1].remove);

        // On the heading: keep the first, then put them all back.
        toggle_at(&mut groups, Line::Heading { group: 0 });
        assert_eq!(groups[0].keeping(), 1);
        toggle_at(&mut groups, Line::Heading { group: 0 });
        assert_eq!(groups[0].keeping(), 3);

        // An index that is not there is not a panic.
        toggle_at(&mut groups, Line::Copy { group: 9, copy: 9 });
        toggle_at(&mut groups, Line::Heading { group: 9 });
    }

    #[test]
    fn hashing_two_files_the_same_gives_the_same_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        let bytes: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        std::fs::write(&a, &bytes).unwrap();
        std::fs::write(&b, &bytes).unwrap();
        let mut different = bytes.clone();
        different[150_000] ^= 0xff;
        std::fs::write(&c, &different).unwrap();

        assert_eq!(hash(&a).unwrap(), hash(&b).unwrap());
        assert_ne!(
            hash(&a).unwrap(),
            hash(&c).unwrap(),
            "a change past the first chunk still changes the answer"
        );
    }

    #[test]
    fn a_hard_link_check_over_many_files_is_not_quadratic() {
        // Through a set rather than a list. This runs once per file in the
        // whole tree, and a linear scan here is the difference between a home
        // directory taking a second and taking a minute.
        let files: Vec<Candidate> = (0..20_000)
            .map(|n| Candidate {
                path: PathBuf::from(format!("/f{n}")),
                size: n as u64,
                identity: Some((0, n as u64)),
            })
            .collect();
        let start = std::time::Instant::now();
        let kept = without_hard_links(files);
        assert_eq!(kept.len(), 20_000);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "twenty thousand files took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_scan_runs_on_a_thread_and_stops_when_dropped() {
        let root = fixture();
        let mut scan = Scan::spawn(root.path().to_path_buf(), Options::default());
        scan.join();
        let found = scan.snapshot();
        assert!(found.finished);
        assert!(!found.cancelled);
        assert_eq!(found.groups.len(), 2);
        assert_eq!(wasted(&found.groups), 23 * 2 + 4096);
    }
}
