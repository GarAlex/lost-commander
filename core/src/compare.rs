//! Telling two directories apart, and making them agree.
//!
//! Two things, one underneath the other. **Compare folders** is the cheap one:
//! line up what each pane is showing, work out which side of each name is
//! newer, and mark them - no dialog, no walk, nothing written. **Synchronize**
//! is the recursive version, which walks both trees, shows every pair with the
//! direction it would go in, and only copies once that has been looked at.
//!
//! The comparison itself is a pure function over facts - a name, a size, a
//! date - so every rule in it is tested without a filesystem. The walk is a
//! synchronous function over a [`Sink`], and [`Scan`] is the thin wrapper that
//! puts it on a thread, exactly as [`crate::find`] is arranged and for the same
//! reason: comparing two large trees takes as long as it takes, and a list that
//! fills while it runs is one you can start reading.
//!
//! Nothing here deletes. A synchronize that removes what the other side does
//! not have is the operation that eats work when a direction is misread, and
//! it wants a design of its own rather than a fourth value in an enum.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crate::mount::Platform;
use crate::progress::SyncTask;

/// How far apart two timestamps can be and still count as the same moment.
///
/// FAT stores modification times to two seconds, so a file copied to a memory
/// stick comes back with a time up to two seconds off the original. Without
/// this every such file reads as "differs" and a comparison of a backup
/// against its source is a wall of false differences.
pub const TOLERANCE: Duration = Duration::from_secs(2);

/// How many pairs a scan collects before it calls it enough.
pub const MAX_PAIRS: usize = 20_000;

/// How much is read at a time when comparing contents.
const CHUNK: usize = 64 * 1024;

/// What is worth knowing about one side of a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
}

impl Facts {
    pub fn file(size: u64, modified: Option<SystemTime>) -> Facts {
        Facts {
            size,
            modified,
            is_dir: false,
        }
    }

    pub fn dir() -> Facts {
        Facts {
            size: 0,
            modified: None,
            is_dir: true,
        }
    }

    pub fn of(path: &Path) -> io::Result<Facts> {
        let meta = path.symlink_metadata()?;
        Ok(Facts {
            size: meta.len(),
            modified: meta.modified().ok(),
            is_dir: meta.is_dir(),
        })
    }
}

/// How the two sides of one name relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Same size, and the same time to within [`TOLERANCE`].
    Same,
    LeftNewer,
    RightNewer,
    /// Different, but neither is newer - same time, different size. Something
    /// is wrong here, and guessing a direction is how the wrong one wins.
    Differ,
    OnlyLeft,
    OnlyRight,
}

impl State {
    /// The arrow the lists draw.
    pub fn mark(&self) -> &'static str {
        match self {
            State::Same => "=",
            State::LeftNewer => "->",
            State::RightNewer => "<-",
            State::Differ => "!=",
            State::OnlyLeft => "->",
            State::OnlyRight => "<-",
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            State::Same => "the same",
            State::LeftNewer => "newer on the left",
            State::RightNewer => "newer on the right",
            State::Differ => "different, neither newer",
            State::OnlyLeft => "only on the left",
            State::OnlyRight => "only on the right",
        }
    }

    pub fn is_same(&self) -> bool {
        *self == State::Same
    }
}

/// Which way a pair would be copied, once someone has looked at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToRight,
    ToLeft,
    /// Leave both alone.
    Skip,
}

impl Direction {
    pub fn mark(&self) -> &'static str {
        match self {
            Direction::ToRight => "->",
            Direction::ToLeft => "<-",
            Direction::Skip => " ",
        }
    }

    /// Cycle, for the one key that sets a row's direction.
    pub fn next(&self) -> Direction {
        match self {
            Direction::ToRight => Direction::ToLeft,
            Direction::ToLeft => Direction::Skip,
            Direction::Skip => Direction::ToRight,
        }
    }
}

/// What a control over the whole list sets its rows to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bulk {
    /// Every row that can go this way, goes this way.
    All(Direction),
    /// Back to what the comparison itself chose.
    Suggested,
}

/// The whole-list controls, in the order they are offered.
///
/// A thousand differences is a thousand presses of the one key that turns a
/// row, which is not an answer. These are: point the lot one way, leave the
/// lot alone, or undo all of it and start again from what the comparison
/// worked out.
pub const BULK: [Bulk; 4] = [
    Bulk::All(Direction::ToRight),
    Bulk::All(Direction::ToLeft),
    Bulk::All(Direction::Skip),
    Bulk::Suggested,
];

impl Bulk {
    pub fn label(&self) -> &'static str {
        match self {
            Bulk::All(Direction::ToRight) => "All ->",
            Bulk::All(Direction::ToLeft) => "All <-",
            Bulk::All(Direction::Skip) => "None",
            Bulk::Suggested => "Reset",
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Bulk::All(Direction::ToRight) => "Every row on screen that can go right, goes right",
            Bulk::All(Direction::ToLeft) => "Every row on screen that can go left, goes left",
            Bulk::All(Direction::Skip) => "Leave every row on screen alone",
            Bulk::Suggested => "Back to what the comparison chose",
        }
    }
}

/// What to do about a pair, before anyone has said otherwise.
///
/// Newer wins, a file only one side has is copied to the other, and anything
/// this cannot answer is skipped. Nothing is ever deleted and nothing older
/// ever lands on something newer without being asked for by hand.
pub fn suggested(state: State) -> Direction {
    match state {
        State::OnlyLeft | State::LeftNewer => Direction::ToRight,
        State::OnlyRight | State::RightNewer => Direction::ToLeft,
        State::Same | State::Differ => Direction::Skip,
    }
}

/// One name, and what each side has under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// Relative to the two roots, so the same string names both sides.
    pub name: String,
    pub left: Option<Facts>,
    pub right: Option<Facts>,
    pub state: State,
    /// What would happen to it. Starts at [`suggested`].
    pub direction: Direction,
}

impl Pair {
    pub fn new(name: impl Into<String>, left: Option<Facts>, right: Option<Facts>) -> Pair {
        Pair::compared(name, left, right, None)
    }

    /// As [`Pair::new`], with the answer to a content comparison where one was
    /// made - `Some(true)` for identical bytes.
    pub fn compared(
        name: impl Into<String>,
        left: Option<Facts>,
        right: Option<Facts>,
        same_bytes: Option<bool>,
    ) -> Pair {
        let state = state_of(left.as_ref(), right.as_ref(), same_bytes);
        Pair {
            name: name.into(),
            left,
            right,
            state,
            direction: suggested(state),
        }
    }

    /// Whether this pair is one a synchronize would touch.
    pub fn is_work(&self) -> bool {
        self.direction != Direction::Skip && self.allows(self.direction)
    }

    /// Whether a direction is one this pair could actually take.
    ///
    /// A copy needs something to copy. Pointing a file only the right has to
    /// the right asks for a left-hand file that is not there, and the run
    /// fails on it - so no control offers that direction in the first place.
    pub fn allows(&self, direction: Direction) -> bool {
        match direction {
            Direction::ToRight => self.left.is_some(),
            Direction::ToLeft => self.right.is_some(),
            Direction::Skip => true,
        }
    }

    /// The next direction the one key can set, passing over the impossible
    /// ones. Terminates because leaving a pair alone is always possible.
    pub fn turn(&mut self) {
        let mut next = self.direction.next();
        while !self.allows(next) {
            next = next.next();
        }
        self.direction = next;
    }
}

/// Point a whole run of rows one way.
///
/// `which` indexes `pairs`, and the front-ends pass the rows on screen - so a
/// list filtered down to the left-hand orphans turns those and leaves the rest
/// of the tree as it was. Rows that cannot go that way keep their direction.
/// Returns how many actually moved, which is what to say afterwards.
pub fn turn_all(pairs: &mut [Pair], which: &[usize], bulk: Bulk) -> usize {
    let mut turned = 0;
    for &index in which {
        let Some(pair) = pairs.get_mut(index) else {
            continue;
        };
        let wanted = match bulk {
            Bulk::All(direction) => direction,
            Bulk::Suggested => suggested(pair.state),
        };
        if pair.direction == wanted || !pair.allows(wanted) {
            continue;
        }
        pair.direction = wanted;
        turned += 1;
    }
    turned
}

/// Whether two times are the same moment, allowing for coarse filesystems.
pub fn same_moment(left: Option<SystemTime>, right: Option<SystemTime>) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => {
            let gap = l.duration_since(r).or_else(|_| r.duration_since(l));
            gap.map(|d| d <= TOLERANCE).unwrap_or(false)
        }
        // A missing date cannot be compared. Two files with no dates at all
        // are as equal as this can tell.
        (None, None) => true,
        _ => false,
    }
}

/// How one name's two sides relate.
///
/// `same_bytes` is the answer to a content comparison where one was made; in
/// quick mode it is `None` and size and date decide.
pub fn state_of(left: Option<&Facts>, right: Option<&Facts>, same_bytes: Option<bool>) -> State {
    let (left, right) = match (left, right) {
        (Some(l), Some(r)) => (l, r),
        (Some(_), None) => return State::OnlyLeft,
        (None, Some(_)) => return State::OnlyRight,
        // Not a pair at all, but the type allows it; nothing to do about it.
        (None, None) => return State::Same,
    };

    // Two directories of the same name are the same entry to a comparison -
    // what is inside them is compared as its own pairs.
    if left.is_dir && right.is_dir {
        return State::Same;
    }

    if let Some(same) = same_bytes {
        if same {
            return State::Same;
        }
    } else if left.size == right.size && same_moment(left.modified, right.modified) {
        return State::Same;
    }

    // Different, one way or another - so which side is newer?
    match (left.modified, right.modified) {
        _ if same_moment(left.modified, right.modified) => State::Differ,
        (Some(l), Some(r)) if l > r => State::LeftNewer,
        (Some(l), Some(r)) if r > l => State::RightNewer,
        _ => State::Differ,
    }
}

/// Whether two files hold the same bytes.
///
/// Size first, because two files of different lengths cannot match and reading
/// them to find that out would be work for nothing.
pub fn same_content(left: &Path, right: &Path) -> io::Result<bool> {
    let (l, r) = (left.metadata()?, right.metadata()?);
    if l.len() != r.len() {
        return Ok(false);
    }
    let mut a = std::fs::File::open(left)?;
    let mut b = std::fs::File::open(right)?;
    let mut left_buf = vec![0u8; CHUNK];
    let mut right_buf = vec![0u8; CHUNK];
    loop {
        let read = read_full(&mut a, &mut left_buf)?;
        let other = read_full(&mut b, &mut right_buf)?;
        if read != other {
            return Ok(false);
        }
        if read == 0 {
            return Ok(true);
        }
        if left_buf[..read] != right_buf[..other] {
            return Ok(false);
        }
    }
}

/// Fill `buf` as far as the file allows, short only at the end.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Whether names are matched exactly, which is a question about the platform.
///
/// `README` and `readme` are one file on Windows and on a stock macOS volume,
/// and two files on Linux. Pairing them the wrong way round either invents a
/// difference or hides one.
pub fn case_sensitive(platform: Platform) -> bool {
    platform == Platform::Linux
}

/// A name as it is matched: itself, or folded where the platform folds.
fn key(name: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        name.to_string()
    } else {
        name.to_lowercase()
    }
}

/// Line two listings up by name.
///
/// In name order rather than either side's order, because a list that reads
/// down both sides at once has to have one order, and neither pane's sort is
/// more right than the other's.
pub fn pair_up(
    left: &[(String, Facts)],
    right: &[(String, Facts)],
    case_sensitive: bool,
) -> Vec<Pair> {
    let mut all: BTreeMap<String, (String, Option<Facts>, Option<Facts>)> = BTreeMap::new();
    for (name, facts) in left {
        all.insert(
            key(name, case_sensitive),
            (name.clone(), Some(*facts), None),
        );
    }
    for (name, facts) in right {
        let slot = all
            .entry(key(name, case_sensitive))
            .or_insert_with(|| (name.clone(), None, None));
        slot.2 = Some(*facts);
    }
    all.into_values()
        .map(|(name, left, right)| Pair::new(name, left, right))
        .collect()
}

/// What one panel is showing, as the comparison wants it.
pub fn facts_of(entries: &[crate::entry::Entry]) -> Vec<(String, Facts)> {
    entries
        .iter()
        .filter(|entry| !entry.is_parent())
        .map(|entry| {
            (
                entry.name.clone(),
                Facts {
                    size: entry.size,
                    modified: entry.modified,
                    is_dir: entry.is_dir(),
                },
            )
        })
        .collect()
}

/// The names each side should mark, comparing the two listings as they stand.
///
/// The Commander gesture, and it is deliberately about *this* pane rather than
/// about the pair: each side marks what it has that the other does not, and
/// what it has a newer copy of. Directories are left alone - whether two
/// directories differ is a question about their contents, and marking one
/// would offer to copy the whole thing over the answer.
pub fn to_mark(
    left: &[(String, Facts)],
    right: &[(String, Facts)],
    case_sensitive: bool,
) -> (Vec<String>, Vec<String>) {
    let mut mark_left = Vec::new();
    let mut mark_right = Vec::new();
    for pair in pair_up(left, right, case_sensitive) {
        let is_dir = pair
            .left
            .as_ref()
            .or(pair.right.as_ref())
            .map(|f| f.is_dir)
            .unwrap_or(false);
        if is_dir {
            continue;
        }
        match pair.state {
            State::OnlyLeft | State::LeftNewer => mark_left.push(pair.name),
            State::OnlyRight | State::RightNewer => mark_right.push(pair.name),
            // Different with no direction: both sides, because both are worth
            // looking at and neither one is the answer.
            State::Differ => {
                mark_left.push(pair.name.clone());
                mark_right.push(pair.name);
            }
            State::Same => {}
        }
    }
    (mark_left, mark_right)
}

/// Mark the differences between what two panels are showing.
///
/// Returns how many were marked on each side. Nothing is read from the disk:
/// this compares the two listings that are already on screen, which is what
/// makes it instant and what makes it stop at the top level.
pub fn mark_differences(
    left: &mut crate::panel::Panel,
    right: &mut crate::panel::Panel,
    case_sensitive: bool,
) -> (usize, usize) {
    let (mark_left, mark_right) = to_mark(
        &facts_of(&left.entries),
        &facts_of(&right.entries),
        case_sensitive,
    );
    apply_marks(left, &mark_left);
    apply_marks(right, &mark_right);
    (mark_left.len(), mark_right.len())
}

fn apply_marks(panel: &mut crate::panel::Panel, names: &[String]) {
    panel.clear_marks();
    for entry in panel.entries.iter_mut() {
        if names.contains(&entry.name) {
            entry.marked = true;
        }
    }
}

/// What a comparison should look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Descend into subdirectories. Off is "compare these two listings".
    pub recursive: bool,
    /// Read both files rather than trusting size and date.
    pub by_content: bool,
    pub include_hidden: bool,
    pub case_sensitive: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            recursive: true,
            by_content: false,
            include_hidden: false,
            case_sensitive: case_sensitive(Platform::current()),
        }
    }
}

/// Where a walk reports to, and asks whether to stop.
pub trait Sink {
    /// A pair was worked out. Returning false stops the walk.
    fn pair(&mut self, pair: Pair) -> bool;
    /// Where the walk has got to, for the "still going" line.
    fn looking_at(&mut self, name: &str);
    fn cancelled(&self) -> bool;
}

/// What one directory holds, as the comparison wants it.
fn listing(dir: &Path, include_hidden: bool) -> Vec<(String, Facts)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        out.push((
            name,
            Facts {
                size: meta.len(),
                modified: meta.modified().ok(),
                is_dir: meta.is_dir(),
            },
        ));
    }
    out
}

/// Compare two trees, reporting each pair as it is worked out.
pub fn walk(left_root: &Path, right_root: &Path, options: &Options, sink: &mut dyn Sink) {
    walk_at(left_root, right_root, "", options, sink);
}

fn walk_at(
    left_root: &Path,
    right_root: &Path,
    prefix: &str,
    options: &Options,
    sink: &mut dyn Sink,
) -> bool {
    if sink.cancelled() {
        return false;
    }
    let left_dir = if prefix.is_empty() {
        left_root.to_path_buf()
    } else {
        left_root.join(prefix)
    };
    let right_dir = if prefix.is_empty() {
        right_root.to_path_buf()
    } else {
        right_root.join(prefix)
    };
    sink.looking_at(if prefix.is_empty() { "." } else { prefix });

    let pairs = pair_up(
        &listing(&left_dir, options.include_hidden),
        &listing(&right_dir, options.include_hidden),
        options.case_sensitive,
    );

    let mut descend = Vec::new();
    for mut pair in pairs {
        if sink.cancelled() {
            return false;
        }
        let both_dirs =
            matches!((&pair.left, &pair.right), (Some(l), Some(r)) if l.is_dir && r.is_dir);
        let relative = if prefix.is_empty() {
            pair.name.clone()
        } else {
            format!("{prefix}/{}", pair.name)
        };

        if both_dirs {
            // A directory present on both sides is not a difference; what is
            // inside it might be.
            if options.recursive {
                descend.push(relative);
            }
            continue;
        }

        // A directory only one side has: report it, and it copies whole.
        let is_dir = pair
            .left
            .as_ref()
            .or(pair.right.as_ref())
            .map(|f| f.is_dir)
            .unwrap_or(false);

        if options.by_content
            && !is_dir
            && matches!(
                pair.state,
                State::Differ | State::LeftNewer | State::RightNewer | State::Same
            )
        {
            let same = same_content(&left_dir.join(&pair.name), &right_dir.join(&pair.name)).ok();
            pair = Pair::compared(&relative, pair.left, pair.right, same);
        } else {
            pair.name = relative;
        }

        if !sink.pair(pair) {
            return false;
        }
    }

    for relative in descend {
        if !walk_at(left_root, right_root, &relative, options, sink) {
            return false;
        }
    }
    true
}

/// The copies a set of chosen directions comes to.
///
/// Directories only one side has are copied whole; the rest are file copies to
/// an exact path, whose parents are made on the way.
pub fn tasks(pairs: &[Pair], left_root: &Path, right_root: &Path) -> Vec<SyncTask> {
    pairs
        .iter()
        .filter(|pair| pair.is_work())
        .map(|pair| match pair.direction {
            Direction::ToRight => SyncTask::Copy {
                from: left_root.join(&pair.name),
                to: right_root.join(&pair.name),
            },
            _ => SyncTask::Copy {
                from: right_root.join(&pair.name),
                to: left_root.join(&pair.name),
            },
        })
        .collect()
}

/// How many pairs fall into each bucket, for the line under the list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub same: usize,
    pub to_right: usize,
    pub to_left: usize,
    pub skipped_differences: usize,
}

pub fn tally(pairs: &[Pair]) -> Tally {
    let mut tally = Tally::default();
    for pair in pairs {
        match pair.direction {
            Direction::ToRight => tally.to_right += 1,
            Direction::ToLeft => tally.to_left += 1,
            Direction::Skip if pair.state.is_same() => tally.same += 1,
            Direction::Skip => tally.skipped_differences += 1,
        }
    }
    tally
}

/// What the list shows, for hiding the rows that are not the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Show {
    pub same: bool,
    pub differences: bool,
    pub only_left: bool,
    pub only_right: bool,
}

impl Show {
    /// Everything but the files that already agree, which is what you opened
    /// the window to see.
    pub fn differences_only() -> Show {
        Show {
            same: false,
            differences: true,
            only_left: true,
            only_right: true,
        }
    }

    pub fn allows(&self, state: State) -> bool {
        match state {
            State::Same => self.same,
            State::OnlyLeft => self.only_left,
            State::OnlyRight => self.only_right,
            _ => self.differences,
        }
    }
}

/// A comparison in progress, and what it has found.
#[derive(Debug, Clone, Default)]
pub struct Compared {
    pub pairs: Vec<Pair>,
    pub current: String,
    pub finished: bool,
    pub cancelled: bool,
    /// It stopped at [`MAX_PAIRS`] rather than because it ran out of tree.
    pub truncated: bool,
}

struct SharedSink {
    compared: Arc<Mutex<Compared>>,
    cancel: Arc<AtomicBool>,
}

impl Sink for SharedSink {
    fn pair(&mut self, pair: Pair) -> bool {
        let mut guard = lock(&self.compared);
        if guard.pairs.len() >= MAX_PAIRS {
            guard.truncated = true;
            return false;
        }
        guard.pairs.push(pair);
        true
    }

    fn looking_at(&mut self, name: &str) {
        lock(&self.compared).current = name.to_string();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn lock<T>(value: &Arc<Mutex<T>>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}

/// A comparison running on its own thread.
pub struct Scan {
    compared: Arc<Mutex<Compared>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub left: PathBuf,
    pub right: PathBuf,
    pub options: Options,
}

impl Scan {
    pub fn spawn(left: PathBuf, right: PathBuf, options: Options) -> Scan {
        let compared: Arc<Mutex<Compared>> = Arc::default();
        let cancel = Arc::new(AtomicBool::new(false));

        let worker_compared = Arc::clone(&compared);
        let worker_cancel = Arc::clone(&cancel);
        let (worker_left, worker_right) = (left.clone(), right.clone());
        let worker_options = options.clone();

        let handle = std::thread::spawn(move || {
            let mut sink = SharedSink {
                compared: Arc::clone(&worker_compared),
                cancel: Arc::clone(&worker_cancel),
            };
            walk(&worker_left, &worker_right, &worker_options, &mut sink);

            let mut guard = lock(&worker_compared);
            guard.cancelled = worker_cancel.load(Ordering::Relaxed);
            guard.finished = true;
            guard.current.clear();
        });

        Scan {
            compared,
            cancel,
            handle: Some(handle),
            left,
            right,
            options,
        }
    }

    pub fn snapshot(&self) -> Compared {
        lock(&self.compared).clone()
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.compared).finished
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
    /// A window closed mid-comparison must not leave a thread walking a disk.
    fn drop(&mut self) {
        self.request_stop();
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
    }

    fn file(size: u64, seconds: u64) -> Facts {
        Facts::file(size, at(seconds))
    }

    #[test]
    fn two_seconds_apart_is_the_same_moment() {
        // FAT rounds to two seconds, so a file copied to a memory stick and
        // back is not a different file.
        assert!(same_moment(at(1_000), at(1_000)));
        assert!(same_moment(at(1_000), at(1_002)));
        assert!(same_moment(at(1_002), at(1_000)));
        assert!(!same_moment(at(1_000), at(1_003)));
        assert!(same_moment(None, None));
        assert!(!same_moment(at(1_000), None));
    }

    #[test]
    fn size_and_date_decide_which_side_is_newer() {
        let cases = [
            (file(10, 100), file(10, 100), State::Same),
            (file(10, 100), file(10, 101), State::Same), // within tolerance
            (file(10, 200), file(10, 100), State::LeftNewer),
            (file(10, 100), file(10, 200), State::RightNewer),
            (file(10, 100), file(20, 100), State::Differ),
            (file(20, 300), file(10, 100), State::LeftNewer),
        ];
        for (left, right, expected) in cases {
            assert_eq!(
                state_of(Some(&left), Some(&right), None),
                expected,
                "{left:?} vs {right:?}"
            );
        }

        assert_eq!(state_of(Some(&file(1, 1)), None, None), State::OnlyLeft);
        assert_eq!(state_of(None, Some(&file(1, 1)), None), State::OnlyRight);
    }

    #[test]
    fn the_same_size_at_the_same_moment_is_a_difference_with_no_direction() {
        // Two files that claim the same time and are not the same length is
        // something being wrong, and guessing which one to keep is how the
        // wrong one wins.
        let state = state_of(Some(&file(10, 100)), Some(&file(20, 100)), None);
        assert_eq!(state, State::Differ);
        assert_eq!(suggested(state), Direction::Skip);
    }

    #[test]
    fn a_content_comparison_overrides_the_dates() {
        // Same bytes, different dates: nothing to copy either way.
        let state = state_of(Some(&file(10, 100)), Some(&file(10, 900)), Some(true));
        assert_eq!(state, State::Same);
        assert_eq!(suggested(state), Direction::Skip);

        // Different bytes at the same size and time: a difference, and still
        // no direction it can pick for you.
        let state = state_of(Some(&file(10, 100)), Some(&file(10, 100)), Some(false));
        assert_eq!(state, State::Differ);

        // Different bytes, and one is newer.
        let state = state_of(Some(&file(10, 900)), Some(&file(10, 100)), Some(false));
        assert_eq!(state, State::LeftNewer);
    }

    #[test]
    fn a_directory_on_both_sides_is_not_itself_a_difference() {
        let state = state_of(Some(&Facts::dir()), Some(&Facts::dir()), None);
        assert_eq!(state, State::Same);
    }

    #[test]
    fn what_each_state_would_do_by_default() {
        assert_eq!(suggested(State::OnlyLeft), Direction::ToRight);
        assert_eq!(suggested(State::OnlyRight), Direction::ToLeft);
        assert_eq!(suggested(State::LeftNewer), Direction::ToRight);
        assert_eq!(suggested(State::RightNewer), Direction::ToLeft);
        assert_eq!(suggested(State::Same), Direction::Skip);
        assert_eq!(suggested(State::Differ), Direction::Skip);
        // And the row cycles through all three and back.
        let mut direction = Direction::ToRight;
        for _ in 0..3 {
            direction = direction.next();
        }
        assert_eq!(direction, Direction::ToRight);
    }

    #[test]
    fn pairing_lines_the_two_listings_up_by_name() {
        let left = vec![
            ("shared.txt".to_string(), file(10, 100)),
            ("only-left.txt".to_string(), file(5, 100)),
        ];
        let right = vec![
            ("shared.txt".to_string(), file(10, 500)),
            ("only-right.txt".to_string(), file(7, 100)),
        ];
        let pairs = pair_up(&left, &right, true);

        let names: Vec<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["only-left.txt", "only-right.txt", "shared.txt"],
            "in name order, since neither pane's sort is the right one"
        );
        assert_eq!(pairs[0].state, State::OnlyLeft);
        assert_eq!(pairs[1].state, State::OnlyRight);
        assert_eq!(pairs[2].state, State::RightNewer);
    }

    #[test]
    fn where_the_platform_folds_case_so_does_the_pairing() {
        let left = vec![("README".to_string(), file(10, 100))];
        let right = vec![("readme".to_string(), file(10, 100))];

        // Linux: two different files, each only on one side.
        let apart = pair_up(&left, &right, true);
        assert_eq!(apart.len(), 2);
        assert_eq!(apart[0].state, State::OnlyLeft);

        // Windows and macOS: one file, and it matches.
        let together = pair_up(&left, &right, false);
        assert_eq!(together.len(), 1);
        assert_eq!(together[0].state, State::Same);

        assert!(case_sensitive(Platform::Linux));
        assert!(!case_sensitive(Platform::Windows));
        assert!(!case_sensitive(Platform::MacOs));
    }

    #[test]
    fn the_tasks_are_copies_to_an_exact_path_both_ways() {
        let mut pairs = vec![
            Pair::new("a.txt", Some(file(1, 200)), Some(file(1, 100))),
            Pair::new("deep/b.txt", None, Some(file(1, 100))),
            Pair::new("same.txt", Some(file(1, 100)), Some(file(1, 100))),
        ];
        assert_eq!(pairs[0].direction, Direction::ToRight);
        assert_eq!(pairs[1].direction, Direction::ToLeft);
        assert_eq!(pairs[2].direction, Direction::Skip);

        let planned = tasks(&pairs, Path::new("/left"), Path::new("/right"));
        assert_eq!(
            planned,
            vec![
                SyncTask::Copy {
                    from: "/left/a.txt".into(),
                    to: "/right/a.txt".into()
                },
                SyncTask::Copy {
                    from: "/right/deep/b.txt".into(),
                    to: "/left/deep/b.txt".into()
                },
            ],
            "the ones that already agree are not work"
        );

        // And a row turned round goes the other way.
        pairs[0].direction = Direction::ToLeft;
        let turned = tasks(&pairs[..1], Path::new("/left"), Path::new("/right"));
        assert_eq!(
            turned[0],
            SyncTask::Copy {
                from: "/right/a.txt".into(),
                to: "/left/a.txt".into()
            }
        );
    }

    #[test]
    fn each_side_marks_what_it_has_that_the_other_does_not() {
        let left = vec![
            ("only-left.txt".to_string(), file(1, 100)),
            ("newer-left.txt".to_string(), file(1, 900)),
            ("newer-right.txt".to_string(), file(1, 100)),
            ("same.txt".to_string(), file(1, 100)),
            ("odd.txt".to_string(), file(1, 100)),
            ("shared-dir".to_string(), Facts::dir()),
            ("left-dir".to_string(), Facts::dir()),
        ];
        let right = vec![
            ("only-right.txt".to_string(), file(1, 100)),
            ("newer-left.txt".to_string(), file(1, 100)),
            ("newer-right.txt".to_string(), file(1, 900)),
            ("same.txt".to_string(), file(1, 100)),
            ("odd.txt".to_string(), file(2, 100)),
            ("shared-dir".to_string(), Facts::dir()),
        ];

        let (mut mark_left, mut mark_right) = to_mark(&left, &right, true);
        mark_left.sort();
        mark_right.sort();
        assert_eq!(
            mark_left,
            ["newer-left.txt", "odd.txt", "only-left.txt"],
            "what this side has, and has a newer copy of"
        );
        assert_eq!(mark_right, ["newer-right.txt", "odd.txt", "only-right.txt"]);
        assert!(
            !mark_left.contains(&"left-dir".to_string()),
            "a directory is not marked: whether it differs is a question about \
             what is inside it"
        );
        assert!(
            mark_left.contains(&"odd.txt".to_string())
                && mark_right.contains(&"odd.txt".to_string()),
            "a difference with no direction is worth looking at on both sides"
        );
    }

    #[test]
    fn marking_replaces_whatever_was_marked_before() {
        let root = trees();
        let mut left = crate::panel::Panel::new(root.path().join("left"));
        let mut right = crate::panel::Panel::new(root.path().join("right"));
        left.mark_all();

        let (marked_left, marked_right) = mark_differences(&mut left, &mut right, true);
        assert_eq!(
            (marked_left, marked_right),
            (1, 1),
            "only-left.txt and only-right.txt; same.txt agrees and the two \
             directories are not compared by name"
        );
        assert_eq!(
            left.marked_count(),
            1,
            "the marks that were there are replaced, not added to"
        );
        assert!(left
            .entries
            .iter()
            .any(|e| e.marked && e.name == "only-left.txt"));
        assert!(right
            .entries
            .iter()
            .any(|e| e.marked && e.name == "only-right.txt"));
    }

    #[test]
    fn the_tally_counts_what_would_happen() {
        let pairs = vec![
            Pair::new("a", Some(file(1, 200)), Some(file(1, 100))),
            Pair::new("b", None, Some(file(1, 100))),
            Pair::new("c", Some(file(1, 100)), Some(file(1, 100))),
            Pair::new("d", Some(file(1, 100)), Some(file(2, 100))),
        ];
        assert_eq!(
            tally(&pairs),
            Tally {
                same: 1,
                to_right: 1,
                to_left: 1,
                skipped_differences: 1,
            }
        );
    }

    #[test]
    fn the_filter_hides_what_is_not_the_point() {
        let show = Show::differences_only();
        assert!(!show.allows(State::Same));
        assert!(show.allows(State::LeftNewer));
        assert!(show.allows(State::Differ));
        assert!(show.allows(State::OnlyLeft));
        assert!(show.allows(State::OnlyRight));

        let nothing = Show::default();
        assert!(!nothing.allows(State::LeftNewer));
    }

    /// Two trees, alike except in the ways the test is about.
    fn trees() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        std::fs::create_dir_all(left.join("both")).unwrap();
        std::fs::create_dir_all(right.join("both")).unwrap();
        std::fs::create_dir_all(left.join("left-only")).unwrap();

        std::fs::write(left.join("same.txt"), "identical").unwrap();
        std::fs::write(right.join("same.txt"), "identical").unwrap();
        std::fs::write(left.join("only-left.txt"), "here").unwrap();
        std::fs::write(right.join("only-right.txt"), "there").unwrap();
        std::fs::write(left.join("both/deep.txt"), "one").unwrap();
        std::fs::write(right.join("both/deep.txt"), "two-and-longer").unwrap();
        std::fs::write(left.join("left-only/inside.txt"), "x").unwrap();
        std::fs::write(left.join(".hidden"), "dot").unwrap();
        root
    }

    /// Collect a walk, in name order, as `name state`.
    fn walked(root: &Path, options: &Options) -> Vec<String> {
        struct Collect(Vec<Pair>);
        impl Sink for Collect {
            fn pair(&mut self, pair: Pair) -> bool {
                self.0.push(pair);
                true
            }
            fn looking_at(&mut self, _: &str) {}
            fn cancelled(&self) -> bool {
                false
            }
        }
        let mut sink = Collect(Vec::new());
        walk(&root.join("left"), &root.join("right"), options, &mut sink);
        sink.0.sort_by(|a, b| a.name.cmp(&b.name));
        sink.0
            .iter()
            .map(|p| format!("{} {}", p.name, p.state.mark()))
            .collect()
    }

    #[test]
    fn a_walk_reports_every_difference_and_no_agreement_twice() {
        let root = trees();
        let listed = walked(root.path(), &Options::default());
        assert_eq!(
            listed,
            [
                "both/deep.txt !=".to_string(),
                "left-only ->".to_string(),
                "only-left.txt ->".to_string(),
                "only-right.txt <-".to_string(),
                "same.txt =".to_string(),
            ],
            "the directory on both sides is not a row; what is inside it is, \
             and a directory only one side has copies whole"
        );
    }

    #[test]
    fn without_recursion_it_compares_the_two_listings_and_stops() {
        let root = trees();
        let options = Options {
            recursive: false,
            ..Options::default()
        };
        let listed = walked(root.path(), &options);
        assert!(
            !listed.iter().any(|line| line.contains('/')),
            "nothing from inside a subdirectory: {listed:?}"
        );
        assert!(listed.contains(&"only-left.txt ->".to_string()));
    }

    #[test]
    fn hidden_files_are_left_out_unless_asked_for() {
        let root = trees();
        let without = walked(root.path(), &Options::default());
        assert!(!without.iter().any(|l| l.starts_with(".hidden")));

        let with = walked(
            root.path(),
            &Options {
                include_hidden: true,
                ..Options::default()
            },
        );
        assert!(with.iter().any(|l| l.starts_with(".hidden")));
    }

    #[test]
    fn comparing_by_content_finds_what_dates_cannot() {
        let root = tempfile::tempdir().unwrap();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();

        // Same length, same date, different bytes - invisible to a quick
        // comparison and the whole reason the slow one exists.
        std::fs::write(left.join("a.txt"), "aaaa").unwrap();
        std::fs::write(right.join("a.txt"), "bbbb").unwrap();
        // Stamped rather than left to chance, so the quick comparison has
        // nothing at all to go on but the size.
        let when = std::fs::metadata(left.join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(right.join("a.txt"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();

        let quick = walked(root.path(), &Options::default());
        let slow = walked(
            root.path(),
            &Options {
                by_content: true,
                ..Options::default()
            },
        );
        assert_eq!(quick, ["a.txt =".to_string()], "size and date agree");
        assert_eq!(slow, ["a.txt !=".to_string()], "the bytes do not");
    }

    #[test]
    fn same_content_reads_no_further_than_it_has_to() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let c = dir.path().join("c");
        std::fs::write(&a, vec![7u8; CHUNK * 2 + 11]).unwrap();
        std::fs::write(&b, vec![7u8; CHUNK * 2 + 11]).unwrap();
        std::fs::write(&c, vec![7u8; CHUNK * 2 + 12]).unwrap();

        assert!(same_content(&a, &b).unwrap(), "identical across chunks");
        assert!(!same_content(&a, &c).unwrap(), "one byte longer");

        // A difference in the last chunk is still found.
        let mut bytes = vec![7u8; CHUNK * 2 + 11];
        *bytes.last_mut().unwrap() = 8;
        std::fs::write(&b, bytes).unwrap();
        assert!(!same_content(&a, &b).unwrap());
    }

    #[test]
    fn a_pair_will_not_be_pointed_at_a_file_that_is_not_there() {
        // Cycling an only-on-the-right row used to reach "copy it to the
        // right", which asks for a left-hand file that does not exist; the
        // run got as far as "No such file or directory".
        let mut orphan = Pair::new("orphan.txt", None, Some(file(18, 100)));
        assert_eq!(orphan.direction, Direction::ToLeft);
        assert!(!orphan.allows(Direction::ToRight));

        orphan.turn();
        assert_eq!(orphan.direction, Direction::Skip);
        orphan.turn();
        assert_eq!(
            orphan.direction,
            Direction::ToLeft,
            "past the impossible one"
        );

        let mut mine = Pair::new("mine.txt", Some(file(18, 100)), None);
        assert_eq!(mine.direction, Direction::ToRight);
        mine.turn();
        assert_eq!(mine.direction, Direction::Skip);
        mine.turn();
        assert_eq!(mine.direction, Direction::ToRight);

        // And a pair with both sides still cycles all three ways.
        let mut both = Pair::new("both.txt", Some(file(10, 200)), Some(file(10, 100)));
        let mut seen = Vec::new();
        for _ in 0..3 {
            both.turn();
            seen.push(both.direction);
        }
        seen.sort_by_key(|d| format!("{d:?}"));
        assert_eq!(
            seen,
            [Direction::Skip, Direction::ToLeft, Direction::ToRight]
        );
    }

    #[test]
    fn a_direction_that_cannot_happen_is_never_work() {
        // Belt and braces: even if a direction were set some other way, the
        // list of copies does not include one with nothing to copy.
        let mut orphan = Pair::new("orphan.txt", None, Some(file(18, 100)));
        orphan.direction = Direction::ToRight;
        assert!(!orphan.is_work());
        assert!(tasks(&[orphan], Path::new("/l"), Path::new("/r")).is_empty());
    }

    #[test]
    fn one_control_points_the_whole_list_one_way() {
        let mut pairs = vec![
            Pair::new("newer-left", Some(file(10, 200)), Some(file(10, 100))),
            Pair::new("newer-right", Some(file(10, 100)), Some(file(10, 200))),
            Pair::new("mine", Some(file(10, 100)), None),
            Pair::new("theirs", None, Some(file(10, 100))),
        ];
        let all: Vec<usize> = (0..pairs.len()).collect();

        // Everything right. Two rows already point that way and the row only
        // the right has cannot, so exactly one moves - the count is what
        // changed, not what was asked for.
        assert_eq!(turn_all(&mut pairs, &all, Bulk::All(Direction::ToRight)), 1);
        let directions: Vec<Direction> = pairs.iter().map(|p| p.direction).collect();
        assert_eq!(
            directions,
            [
                Direction::ToRight,
                Direction::ToRight,
                Direction::ToRight,
                Direction::ToLeft
            ]
        );

        // Nothing at all, then back to what the comparison worked out.
        assert_eq!(turn_all(&mut pairs, &all, Bulk::All(Direction::Skip)), 4);
        assert_eq!(tally(&pairs).skipped_differences, 4);
        assert_eq!(turn_all(&mut pairs, &all, Bulk::Suggested), 4);
        let directions: Vec<Direction> = pairs.iter().map(|p| p.direction).collect();
        assert_eq!(
            directions,
            [
                Direction::ToRight,
                Direction::ToLeft,
                Direction::ToRight,
                Direction::ToLeft
            ]
        );
        // Doing it twice is not a change the second time.
        assert_eq!(turn_all(&mut pairs, &all, Bulk::Suggested), 0);
    }

    #[test]
    fn a_whole_list_control_only_touches_the_rows_on_screen() {
        // The rows the filter is hiding are not what "all of them" means.
        let mut pairs = vec![
            Pair::new("mine", Some(file(10, 100)), None),
            Pair::new("theirs", None, Some(file(10, 100))),
        ];
        assert_eq!(turn_all(&mut pairs, &[1], Bulk::All(Direction::Skip)), 1);
        assert_eq!(pairs[0].direction, Direction::ToRight, "left where it was");
        assert_eq!(pairs[1].direction, Direction::Skip);
    }

    #[test]
    fn a_scan_runs_on_a_thread_and_stops_when_dropped() {
        let root = trees();
        let mut scan = Scan::spawn(
            root.path().join("left"),
            root.path().join("right"),
            Options::default(),
        );
        scan.join();
        let found = scan.snapshot();
        assert!(found.finished);
        assert!(!found.cancelled);
        assert!(!found.truncated);
        assert_eq!(found.pairs.len(), 5);
    }
}
