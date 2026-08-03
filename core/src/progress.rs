// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Long-running file operations, run off the UI thread with progress and
//! cancellation.
//!
//! The executor is deliberately split in two:
//!
//! * [`execute`] does the work against a [`Sink`], which reports progress and
//!   answers "should I stop?". It is synchronous and takes no threads, so the
//!   tests drive it directly - including cancellation, which is deterministic
//!   rather than a race against a timer.
//! * [`Job`] wraps that in a worker thread and shares a [`Progress`] snapshot
//!   with the UI.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::journal;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

const CHUNK: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    Move {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    Delete {
        targets: Vec<PathBuf>,
        /// To the system's trash, where it can be got back. The safe
        /// default; the permanent one is a separate key.
        to_trash: bool,
    },
    /// Pull members out of an archive onto real disk.
    ///
    /// A copy whose source happens to be inside a file, which is why it is
    /// recorded as one: "where did this come from" should not need two
    /// filters to answer.
    Extract {
        archive: PathBuf,
        /// Member paths inside the archive, already expanded from whatever
        /// was selected - a directory becomes everything under it.
        members: Vec<String>,
        /// The level being looked at, which comes off the front of each
        /// member on the way out.
        ///
        /// Extracting from two levels down should put the files *here*, not
        /// rebuild the whole path from the archive's root under the
        /// destination - that is what every other file manager does and what
        /// "copy these out" plainly means.
        from: String,
        destination: PathBuf,
        /// Held for this run only. Never recorded, never stored.
        password: Option<String>,
    },
    /// Make two trees agree - see [`crate::compare`].
    ///
    /// Each task names both ends, rather than a set of sources and one
    /// destination, because the two directions are mixed in a single run and
    /// each file lands at a path worked out from where it came from.
    Sync { tasks: Vec<SyncTask> },
}

/// One file a synchronize would move, and where to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncTask {
    /// Copy to exactly this path, making the parent directories on the way.
    Copy { from: PathBuf, to: PathBuf },
}

impl SyncTask {
    pub fn from(&self) -> &Path {
        match self {
            SyncTask::Copy { from, .. } => from,
        }
    }

    pub fn to(&self) -> &Path {
        match self {
            SyncTask::Copy { to, .. } => to,
        }
    }
}

impl Operation {
    pub fn verb(&self) -> &'static str {
        match self {
            Operation::Copy { .. } => "Copying",
            Operation::Move { .. } => "Moving",
            Operation::Delete { to_trash: true, .. } => "Moving to trash",
            Operation::Delete { .. } => "Deleting",
            Operation::Extract { .. } => "Extracting",
            Operation::Sync { .. } => "Synchronizing",
        }
    }

    pub fn past_tense(&self) -> &'static str {
        match self {
            Operation::Copy { .. } => "Copied",
            Operation::Move { .. } => "Moved",
            Operation::Delete { to_trash: true, .. } => "Moved to trash",
            Operation::Delete { .. } => "Deleted",
            Operation::Extract { .. } => "Extracted",
            Operation::Sync { .. } => "Synchronized",
        }
    }

    /// The kind this run is recorded under.
    pub fn recorded_as(&self) -> journal::Kind {
        match self {
            Operation::Copy { .. } => journal::Kind::Copy,
            // A synchronize is copies, in both directions at once - which is
            // exactly why it needs a heading of its own to be read as the one
            // thing it was.
            Operation::Sync { .. } => journal::Kind::Copy,
            Operation::Extract { .. } => journal::Kind::Copy,
            Operation::Move { .. } => journal::Kind::Move,
            Operation::Delete { to_trash: true, .. } => journal::Kind::Trash,
            Operation::Delete { .. } => journal::Kind::Delete,
        }
    }

    /// The one line that stands over the run in the account.
    ///
    /// Names where as well as how many: "Copy 42 items", read a week later,
    /// is not a record of anything.
    pub fn summarise(&self) -> String {
        match self {
            Operation::Copy {
                sources,
                destination,
            } => format!(
                "Copy {} item(s) to {}",
                sources.len(),
                destination.display()
            ),
            Operation::Move {
                sources,
                destination,
            } => format!(
                "Move {} item(s) to {}",
                sources.len(),
                destination.display()
            ),
            Operation::Delete { targets, to_trash } => format!(
                "{} {} item(s)",
                if *to_trash {
                    "Trash"
                } else {
                    "Delete for good"
                },
                targets.len()
            ),
            Operation::Extract {
                archive,
                members,
                destination,
                ..
            } => format!(
                "Extract {} item(s) from {} to {}",
                members.len(),
                archive
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| archive.display().to_string()),
                destination.display()
            ),
            Operation::Sync { tasks } => format!("Synchronize {} file(s)", tasks.len()),
        }
    }

    /// The top-level paths, for the scan that sizes the progress bar.
    ///
    /// A synchronize has none: its work is a list of pairs rather than a set
    /// of trees, so [`scan`] measures it directly.
    fn roots(&self) -> &[PathBuf] {
        match self {
            Operation::Copy { sources, .. } | Operation::Move { sources, .. } => sources,
            Operation::Delete { targets, .. } => targets,
            // Neither has a tree on disk to walk: a synchronize is a list of
            // pairs, and an extract's sources are inside a file.
            Operation::Sync { .. } | Operation::Extract { .. } => &[],
        }
    }
}

/// A snapshot of how far along an operation is.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub verb: &'static str,
    pub current: String,
    pub items_done: u64,
    pub items_total: u64,
    /// How many of `items_done` were left where they were rather than
    /// written. Worth its own number because "only newer" makes skipping the
    /// expected outcome rather than the exception, and a run that reports
    /// copying everything when it copied two of four is telling you the
    /// opposite of what happened.
    pub items_skipped: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub finished: bool,
    pub cancelled: bool,
    pub failures: Vec<String>,
}

impl Progress {
    fn new(verb: &'static str) -> Self {
        Progress {
            verb,
            ..Default::default()
        }
    }

    /// 0.0 to 1.0. Bytes drive the bar when there are any, because a single
    /// huge file would otherwise look stuck at 0%.
    pub fn fraction(&self) -> f64 {
        if self.finished {
            return 1.0;
        }
        if self.bytes_total > 0 {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        } else if self.items_total > 0 {
            (self.items_done as f64 / self.items_total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn percent(&self) -> u16 {
        (self.fraction() * 100.0).round() as u16
    }

    /// What a finished run comes to, in words.
    ///
    /// The skipped ones are named rather than folded into the total: a copy
    /// answered with "only newer" is meant to leave files alone, and a line
    /// saying it copied all of them would be reporting the opposite.
    pub fn outcome(&self, past: &str) -> String {
        let written = self.items_done.saturating_sub(self.items_skipped);
        if self.items_skipped == 0 {
            return format!("{past} {} item(s)", self.items_done);
        }
        format!(
            "{past} {written} item(s), left {} alone",
            self.items_skipped
        )
    }
}

/// A file already at the target, and what is about to land on it.
///
/// Both sides carry their size and date, because that is what the answer turns
/// on: whether the one already there is worth more than the one arriving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub source: PathBuf,
    pub target: PathBuf,
    pub source_size: u64,
    pub target_size: u64,
    pub source_modified: Option<SystemTime>,
    pub target_modified: Option<SystemTime>,
}

impl Conflict {
    /// Read both sides. `None` when the target is not in the way after all.
    pub fn read(source: &Path, target: &Path) -> Option<Conflict> {
        let target_meta = target.symlink_metadata().ok()?;
        let source_meta = source.symlink_metadata().ok()?;
        Some(Conflict {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
            source_size: source_meta.len(),
            target_size: target_meta.len(),
            source_modified: source_meta.modified().ok(),
            target_modified: target_meta.modified().ok(),
        })
    }

    /// Whether the file about to be written is the newer of the two.
    ///
    /// Not a decision - just the fact the question turns on most often.
    ///
    /// Two files stamped the same moment are not newer than each other, and
    /// "the same moment" carries [`crate::compare::TOLERANCE`]: without it a
    /// file already copied to a FAT stick reads as newer than itself, and
    /// "only newer" would copy the whole thing again every time.
    pub fn source_is_newer(&self) -> Option<bool> {
        if crate::compare::same_moment(self.source_modified, self.target_modified) {
            return Some(false);
        }
        Some(self.source_modified? > self.target_modified?)
    }
}

/// What happens to one conflicting file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Overwrite,
    Skip,
    Cancel,
}

/// The user's answer, which may stand for the rest of the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Overwrite,
    OverwriteAll,
    Skip,
    SkipAll,
    /// Keep going without asking again, overwriting only where the file
    /// arriving is the newer one. The unattended answer: a folder copied over
    /// a backup, where what you mean is "bring it up to date".
    OnlyNewer,
    Cancel,
}

impl Answer {
    /// What it does about the conflict in front of it.
    ///
    /// [`Answer::OnlyNewer`] is the one that cannot say on its own, because it
    /// is a rule rather than an answer - see [`Standing::decide`].
    pub fn resolution(self, conflict: &Conflict) -> Resolution {
        match self {
            Answer::Overwrite | Answer::OverwriteAll => Resolution::Overwrite,
            Answer::Skip | Answer::SkipAll => Resolution::Skip,
            Answer::OnlyNewer => Standing::OnlyNewer.decide(conflict),
            Answer::Cancel => Resolution::Cancel,
        }
    }

    /// Whether it answers every later conflict too.
    pub fn stands(self) -> bool {
        matches!(
            self,
            Answer::OverwriteAll | Answer::SkipAll | Answer::OnlyNewer | Answer::Cancel
        )
    }

    /// What it leaves behind for the rest of the run, if anything.
    fn standing(self) -> Option<Standing> {
        match self {
            Answer::OverwriteAll => Some(Standing::Always(Resolution::Overwrite)),
            Answer::SkipAll => Some(Standing::Always(Resolution::Skip)),
            Answer::OnlyNewer => Some(Standing::OnlyNewer),
            Answer::Cancel => Some(Standing::Always(Resolution::Cancel)),
            Answer::Overwrite | Answer::Skip => None,
        }
    }
}

/// An answer that covers the rest of the run.
///
/// Two shapes, because "overwrite them all" is a decision and "only the newer
/// ones" is a rule: one of them knows the answer already, and the other has to
/// look at each file to work it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Always(Resolution),
    OnlyNewer,
}

impl Standing {
    pub fn decide(&self, conflict: &Conflict) -> Resolution {
        match self {
            Standing::Always(resolution) => *resolution,
            // A file whose date cannot be read is not known to be newer, and
            // guessing that it is would overwrite on no evidence at all.
            Standing::OnlyNewer => match conflict.source_is_newer() {
                Some(true) => Resolution::Overwrite,
                _ => Resolution::Skip,
            },
        }
    }
}

/// Remembers a standing answer, so "all" is only asked for once.
#[derive(Debug, Default)]
pub struct Conflicts {
    standing: Option<Standing>,
}

impl Conflicts {
    /// What to do about this collision, asking only if nothing stands.
    pub fn resolve(&mut self, sink: &mut dyn Sink, conflict: &Conflict) -> Resolution {
        if let Some(standing) = self.standing {
            return standing.decide(conflict);
        }
        let answer = sink.conflict(conflict);
        self.standing = answer.standing();
        answer.resolution(conflict)
    }

    /// The standing answer, if one has been given.
    pub fn standing(&self) -> Option<Standing> {
        self.standing
    }

    /// Overwrite without asking, for an operation that has already asked.
    ///
    /// A synchronize is the one: the whole point of its dialog is that every
    /// pair and its direction were looked at before anything ran, so putting
    /// the same question a second time per file would be noise, and there is
    /// no answer to it the direction did not already give.
    pub fn overwriting() -> Conflicts {
        Conflicts {
            standing: Some(Standing::Always(Resolution::Overwrite)),
        }
    }
}

/// Where an operation reports to, and asks whether to stop.
pub trait Sink {
    fn item_started(&mut self, name: &str);
    /// This one was left where it was. Counts as done for the bar.
    fn item_skipped(&mut self) {
        self.item_done();
    }
    fn item_done(&mut self);
    fn bytes_copied(&mut self, count: u64);
    fn cancelled(&self) -> bool;

    /// Something is already there. Blocks until the user says what to do.
    ///
    /// The default is the safe answer: a sink with no way to ask is a sink
    /// that must not destroy anything.
    fn conflict(&mut self, _conflict: &Conflict) -> Answer {
        Answer::Skip
    }

    /// One thing that actually happened to one file.
    ///
    /// The progress bar does not care - it counts, and a count has no source
    /// and no target. But this is the only place that knows both ends of each
    /// individual copy, move and delete, so it is the only place a record of
    /// them can come from. Doing nothing by default leaves every sink that
    /// only wants a bar exactly as it was.
    fn happened(&mut self, _event: journal::Event) {}
}

/// Count files and bytes up front so the bar has a denominator.
pub fn scan(operation: &Operation) -> (u64, u64) {
    // Trashing a tree is one rename, not one per file, so counting the tree
    // would leave the bar stuck near zero and then jump to the end.
    if matches!(operation, Operation::Delete { to_trash: true, .. }) {
        return (operation.roots().len() as u64, 0);
    }
    let mut items = 0u64;
    let mut bytes = 0u64;
    if let Operation::Sync { tasks } = operation {
        // A synchronize already knows what it is going to touch: each task
        // names one file, so measuring it is reading the list rather than
        // walking a tree.
        for task in tasks {
            scan_path(task.from(), &mut items, &mut bytes);
        }
        return (items, bytes);
    }
    for root in operation.roots() {
        scan_path(root, &mut items, &mut bytes);
    }
    (items, bytes)
}

fn scan_path(path: &Path, items: &mut u64, bytes: &mut u64) {
    let Ok(metadata) = path.symlink_metadata() else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        // The directory itself counts as an item so empty trees still progress.
        *items += 1;
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                scan_path(&entry.path(), items, bytes);
            }
        }
    } else {
        *items += 1;
        *bytes += metadata.len();
    }
}

/// Pull one member out of an archive onto disk.
///
/// The member's path inside the archive becomes a path under the
/// destination, so a directory extracted keeps its shape rather than
/// arriving as a heap of loose files.
fn extract_one(
    archive: &Path,
    member: &str,
    from: &str,
    destination: &Path,
    password: Option<&str>,
    sink: &mut dyn Sink,
) -> std::io::Result<()> {
    sink.item_started(member);
    let bytes = crate::archive::read_with(archive, member, password)?;
    let into = destination.join(landing(member, from));
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&into, &bytes)?;
    sink.bytes_copied(bytes.len() as u64);
    sink.happened(
        journal::Event::new(
            journal::Kind::Copy,
            format!("{}/{member}", archive.display()),
        )
        .to(&into)
        .note(crate::entry::human_size(bytes.len() as u64)),
    );
    sink.item_done();
    Ok(())
}

/// Where a member lands, relative to the destination.
///
/// The level being looked at comes off the front, so extracting `notes.txt`
/// while inside `docs` gives `notes.txt` and not `docs/notes.txt`.
pub fn landing(member: &str, from: &str) -> String {
    if from.is_empty() {
        return member.to_string();
    }
    member
        .strip_prefix(&format!("{from}/"))
        .unwrap_or(member)
        .to_string()
}

/// Perform the operation. Returns a list of human-readable failures; an empty
/// list means everything succeeded.
pub fn execute(operation: &Operation, sink: &mut dyn Sink) -> Vec<String> {
    let mut failures = Vec::new();
    // One per operation, so "overwrite all" covers the run rather than one
    // source tree.
    let mut conflicts = Conflicts::default();

    match operation {
        Operation::Extract {
            archive,
            members,
            from,
            destination,
            password,
        } => {
            for member in members {
                if sink.cancelled() {
                    break;
                }
                if let Err(e) = extract_one(
                    archive,
                    member,
                    from,
                    destination,
                    password.as_deref(),
                    sink,
                ) {
                    failures.push(format!("{member}: {e}"));
                }
            }
        }
        Operation::Copy {
            sources,
            destination,
        } => {
            for source in sources {
                if sink.cancelled() {
                    break;
                }
                if let Err(e) = copy_into(source, destination, sink, &mut conflicts) {
                    failures.push(describe(source, &e));
                }
            }
        }
        Operation::Move {
            sources,
            destination,
        } => {
            for source in sources {
                if sink.cancelled() {
                    break;
                }
                if let Err(e) = move_into(source, destination, sink, &mut conflicts) {
                    failures.push(describe(source, &e));
                }
            }
        }
        Operation::Sync { tasks } => {
            for task in tasks {
                if sink.cancelled() {
                    break;
                }
                let SyncTask::Copy { from, to } = task;
                // Nothing is asked here. A synchronize has already shown
                // every pair and which way it is going, so a prompt per file
                // would be asking the same question a second time - and there
                // is no answer to it that the direction did not already give.
                if let Err(e) = sync_copy(from, to, sink) {
                    failures.push(describe(from, &e));
                }
            }
        }
        Operation::Delete { targets, to_trash } if *to_trash => {
            // A whole tree goes to the trash as one item - the point is to be
            // able to put it back, and putting back half of it is not that.
            //
            // The paths go to the system in batches rather than one at a
            // time, because on Windows each call pays PowerShell's startup:
            // eight files took four and a half seconds one by one, and a
            // hundred would have taken a minute, for work that is instant.
            // What a batch must not cost is knowing which file failed, so the
            // system reports on each one and the results are unpacked here
            // exactly as though they had been done in turn - a record each, a
            // step of the bar each, and a failure named after the file it
            // belongs to.
            let mut rest = targets.as_slice();
            while !rest.is_empty() {
                if sink.cancelled() {
                    break;
                }
                let take = crate::trash::batch_len(rest, crate::mount::Platform::current());
                let (batch, tail) = rest.split_at(take);

                // Named before the call, not after: the whole batch goes in
                // one request, so there is no per-file moment to announce,
                // and a dialog naming nothing while it works looks stuck.
                sink.item_started(&batch[0].display().to_string());

                for (target, result) in batch.iter().zip(crate::trash::trash_batch(batch)) {
                    match result {
                        Ok(()) => {
                            sink.happened(journal::Event::new(journal::Kind::Trash, target));
                            sink.item_done();
                        }
                        Err(e) => failures.push(describe(target, &e)),
                    }
                }
                rest = tail;
            }
        }
        Operation::Delete { targets, .. } => {
            for target in targets {
                if sink.cancelled() {
                    break;
                }
                // A permanent delete walks the tree, so the bar moves and a
                // cancel stops part way through.
                if let Err(e) = delete(target, sink) {
                    failures.push(describe(target, &e));
                }
            }
        }
    }

    failures
}

fn describe(path: &Path, error: &io::Error) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    format!("{name}: {error}")
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

/// Copy to an exact path, making the directories above it.
///
/// The overwrite prompt is deliberately not consulted - see the note where
/// this is called.
fn sync_copy(from: &Path, to: &Path, sink: &mut dyn Sink) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut conflicts = Conflicts::overwriting();
    copy_tree(from, to, sink, &mut conflicts)
}

fn copy_into(
    source: &Path,
    destination_dir: &Path,
    sink: &mut dyn Sink,
    conflicts: &mut Conflicts,
) -> io::Result<()> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))?;
    let target = destination_dir.join(name);
    if source == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination are the same",
        ));
    }
    if source.is_dir() && destination_dir.starts_with(source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot copy a directory into itself",
        ));
    }
    copy_tree(source, &target, sink, conflicts)
}

fn copy_tree(
    source: &Path,
    target: &Path,
    sink: &mut dyn Sink,
    conflicts: &mut Conflicts,
) -> io::Result<()> {
    if sink.cancelled() {
        return Err(cancelled_error());
    }
    let metadata = source.symlink_metadata()?;

    if metadata.is_dir() {
        sink.item_started(&source.display().to_string());
        fs::create_dir_all(target)?;
        sink.item_done();
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(
                &entry.path(),
                &target.join(entry.file_name()),
                sink,
                conflicts,
            )?;
        }
        Ok(())
    } else {
        sink.item_started(&source.display().to_string());
        // A skipped file still counts as done, or the bar would never reach
        // the end of a copy the user deliberately narrowed.
        match resolve_target(source, target, sink, conflicts)? {
            Resolution::Skip => {
                sink.item_skipped();
                Ok(())
            }
            Resolution::Cancel => Err(cancelled_error()),
            Resolution::Overwrite => {
                copy_file(source, target, sink)?;
                sink.item_done();
                Ok(())
            }
        }
    }
}

/// Decide what to do about `target` if something is already there.
///
/// `Overwrite` is also the answer when nothing is in the way - the caller then
/// writes as it always did.
fn resolve_target(
    source: &Path,
    target: &Path,
    sink: &mut dyn Sink,
    conflicts: &mut Conflicts,
) -> io::Result<Resolution> {
    let Ok(existing) = target.symlink_metadata() else {
        return Ok(Resolution::Overwrite);
    };
    // A directory in the way of a file is not a question, it is an error:
    // there is no answer that does what either word would mean.
    if existing.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a directory of that name is already there",
        ));
    }
    // Writing a file onto itself truncates it and then copies the nothing
    // that is left. Nobody means this.
    if same_file(source, target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination are the same file",
        ));
    }
    let Some(conflict) = Conflict::read(source, target) else {
        return Ok(Resolution::Overwrite);
    };
    Ok(conflicts.resolve(sink, &conflict))
}

/// Whether two paths are the same file, following links.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Copy in chunks so the bar moves within a single large file, and so a cancel
/// takes effect promptly instead of after the whole file.
fn copy_file(source: &Path, target: &Path, sink: &mut dyn Sink) -> io::Result<()> {
    let mut reader = fs::File::open(source)?;
    let mut writer = fs::File::create(target)?;
    let mut buffer = vec![0u8; CHUNK];

    loop {
        if sink.cancelled() {
            // Do not leave a half-written file behind.
            drop(writer);
            let _ = fs::remove_file(target);
            return Err(cancelled_error());
        }
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        sink.bytes_copied(read as u64);
    }

    writer.flush()?;
    // Carry permissions across (mode on Unix, read-only flag on Windows).
    if let Ok(metadata) = reader.metadata() {
        let _ = fs::set_permissions(target, metadata.permissions());
    }
    // Recorded here rather than at the call sites: this is the point at which
    // the bytes are on disk, and every path that copies a file comes through
    // it - the plain copy, the cross-filesystem move, and the synchronize.
    sink.happened(
        journal::Event::new(journal::Kind::Copy, source)
            .to(target)
            .note(crate::entry::human_size(
                reader.metadata().map(|m| m.len()).unwrap_or(0),
            )),
    );
    Ok(())
}

fn move_into(
    source: &Path,
    destination_dir: &Path,
    sink: &mut dyn Sink,
    conflicts: &mut Conflicts,
) -> io::Result<()> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))?;
    let target = destination_dir.join(name);
    if source == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination are the same",
        ));
    }
    if source.is_dir() && destination_dir.starts_with(source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot move a directory into itself",
        ));
    }

    // `rename` replaces the target without a word, so the question has to be
    // asked before it is called rather than left to it. Only for a file: a
    // directory landing on a directory is a merge, which the copy path below
    // does entry by entry, asking about each collision it actually finds.
    if !source.is_dir() {
        match resolve_target(source, &target, sink, conflicts)? {
            Resolution::Skip => {
                sink.item_started(&source.display().to_string());
                sink.item_skipped();
                return Ok(());
            }
            Resolution::Cancel => return Err(cancelled_error()),
            Resolution::Overwrite => {}
        }
    }

    // Within one filesystem this is instant, so there is nothing to report.
    // A directory onto an existing directory is refused by `rename`, which
    // sends it down the merging path below - the behaviour we want anyway.
    if fs::rename(source, &target).is_ok() {
        sink.item_started(&source.display().to_string());
        sink.happened(journal::Event::new(journal::Kind::Move, source).to(&target));
        sink.item_done();
        return Ok(());
    }

    // Across filesystems rename fails with EXDEV; fall back to copy + delete.
    copy_tree(source, &target, sink, conflicts)?;
    if sink.cancelled() {
        return Err(cancelled_error());
    }
    remove(source)
}

fn delete(path: &Path, sink: &mut dyn Sink) -> io::Result<()> {
    if sink.cancelled() {
        return Err(cancelled_error());
    }
    let metadata = path.symlink_metadata()?;

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            delete(&entry.path(), sink)?;
        }
    }

    sink.item_started(&path.display().to_string());
    remove(path)?;
    sink.happened(journal::Event::new(journal::Kind::Delete, path));
    sink.item_done();
    Ok(())
}

fn remove(path: &Path) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
}

// ---- threaded wrapper ------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panicking worker must not wedge the UI.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The question the worker is waiting on, and the answer it is waiting for.
#[derive(Debug, Default)]
struct Pending {
    asking: Option<Conflict>,
    answer: Option<Answer>,
}

struct SharedSink {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    pending: Arc<(Mutex<Pending>, Condvar)>,
    /// Where the account of this run goes, if one is being kept.
    journal: Option<journal::Journal>,
    /// The run these events belong to, so a copy of four hundred files is one
    /// heading with four hundred lines under it rather than four hundred
    /// loose lines.
    group: u64,
    /// How many have been written. A copy of a hundred thousand files stops
    /// naming them past the cap - see [`journal::MAX_EVENTS_PER_GROUP`].
    recorded: usize,
}

impl Sink for SharedSink {
    fn item_started(&mut self, name: &str) {
        lock(&self.progress).current = name.to_string();
    }

    fn item_done(&mut self) {
        lock(&self.progress).items_done += 1;
    }

    fn item_skipped(&mut self) {
        let mut progress = lock(&self.progress);
        progress.items_done += 1;
        progress.items_skipped += 1;
    }

    fn bytes_copied(&mut self, count: u64) {
        lock(&self.progress).bytes_done += count;
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn happened(&mut self, event: journal::Event) {
        let Some(journal) = &self.journal else {
            return;
        };
        if self.recorded >= journal::MAX_EVENTS_PER_GROUP {
            return;
        }
        self.recorded += 1;
        journal.record(event.in_group(self.group));
    }

    /// Post the question and sleep until the UI answers it.
    ///
    /// The wait has a timeout it does not need, so that a window closed with
    /// a question on screen leaves a worker that notices the cancel flag
    /// rather than one parked forever on a condvar nobody will signal.
    fn conflict(&mut self, conflict: &Conflict) -> Answer {
        let (lock_, signal) = &*self.pending;
        {
            let mut guard = lock(lock_);
            guard.asking = Some(conflict.clone());
            guard.answer = None;
        }
        signal.notify_all();

        loop {
            if self.cancel.load(Ordering::Relaxed) {
                lock(lock_).asking = None;
                return Answer::Cancel;
            }
            let guard = lock(lock_);
            let (mut guard, _) = signal
                .wait_timeout(guard, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner());
            if let Some(answer) = guard.answer.take() {
                guard.asking = None;
                return answer;
            }
        }
    }
}

/// An operation running on a worker thread.
pub struct Job {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    /// The conflict the worker is blocked on, and where its answer goes.
    pending: Arc<(Mutex<Pending>, Condvar)>,
    handle: Option<JoinHandle<()>>,
    pub operation: Operation,
}

impl Job {
    pub fn spawn(operation: Operation) -> Job {
        Self::spawn_with_cancel(operation, Arc::new(AtomicBool::new(false)), None)
    }

    /// Start, keeping an account of what it does.
    pub fn spawn_recorded(operation: Operation, journal: journal::Journal) -> Job {
        Self::spawn_with_cancel(operation, Arc::new(AtomicBool::new(false)), Some(journal))
    }

    /// Start with a caller-owned cancel flag.
    ///
    /// Handing the flag in lets a test set it *before* the worker starts, so
    /// cancellation can be asserted without racing the copy - a small job can
    /// otherwise finish before `request_cancel` is even called.
    pub fn spawn_with_cancel(
        operation: Operation,
        cancel: Arc<AtomicBool>,
        journal: Option<journal::Journal>,
    ) -> Job {
        let progress = Arc::new(Mutex::new(Progress::new(operation.verb())));

        // The heading goes down before the work starts, so a run killed
        // halfway still has one over whatever it managed.
        let group = journal::new_group_id();
        if let Some(journal) = &journal {
            journal.open_group(journal::Group {
                id: group,
                at: journal::now(),
                kind: operation.recorded_as(),
                summary: operation.summarise(),
            });
        }

        let pending: Arc<(Mutex<Pending>, Condvar)> = Arc::default();

        let worker_progress = Arc::clone(&progress);
        let worker_cancel = Arc::clone(&cancel);
        let worker_pending = Arc::clone(&pending);
        let worker_operation = operation.clone();

        let handle = std::thread::spawn(move || {
            let (items, bytes) = scan(&worker_operation);
            {
                let mut guard = lock(&worker_progress);
                guard.items_total = items;
                guard.bytes_total = bytes;
            }

            let mut sink = SharedSink {
                progress: Arc::clone(&worker_progress),
                cancel: Arc::clone(&worker_cancel),
                pending: Arc::clone(&worker_pending),
                journal: journal.clone(),
                group,
                recorded: 0,
            };
            let began = std::time::Instant::now();
            let failures = execute(&worker_operation, &mut sink);

            // How long the whole run took, written as its own record because
            // the heading went down before any of it started. A run with no
            // such record is one that never reached its end - killed, or the
            // program closed under it - which is worth being able to see.
            //
            // Per-file durations are deliberately not kept: sixteen bytes
            // copied in under a millisecond, four hundred times over, is not
            // a fact about anything. The total for the run is.
            if let Some(journal) = &journal {
                journal.close_group(group, began.elapsed().as_millis() as u64);
            }

            let mut guard = lock(&worker_progress);
            guard.failures = failures;
            guard.cancelled = worker_cancel.load(Ordering::Relaxed);
            guard.finished = true;
        });

        Job {
            progress,
            cancel,
            pending,
            handle: Some(handle),
            operation,
        }
    }

    /// The collision the operation is waiting on, if it is waiting on one.
    pub fn asking(&self) -> Option<Conflict> {
        lock(&self.pending.0).asking.clone()
    }

    /// Answer it, and let the worker carry on.
    ///
    /// The question is retired here rather than when the worker gets round to
    /// noticing: between handing over the answer and the worker waking there
    /// are a few microseconds in which the question is still on the record,
    /// and a UI polling in that window would put the box it has just answered
    /// straight back on the screen.
    pub fn answer(&self, answer: Answer) {
        let (lock_, signal) = &*self.pending;
        {
            let mut pending = lock(lock_);
            pending.answer = Some(answer);
            pending.asking = None;
        }
        if answer == Answer::Cancel {
            self.request_cancel();
        }
        signal.notify_all();
    }

    pub fn snapshot(&self) -> Progress {
        lock(&self.progress).clone()
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.progress).finished
    }

    /// Block until the worker stops. Used when quitting and by the tests.
    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod extracting {
    use super::*;

    #[test]
    fn a_member_lands_relative_to_the_level_it_was_taken_from() {
        // Standing in `docs` and copying `notes.txt` out should put it in the
        // other pane as `notes.txt`. Rebuilding the archive's whole path
        // under the destination is what nobody asked for.
        assert_eq!(landing("docs/notes.txt", "docs"), "notes.txt");
        assert_eq!(landing("docs/deep/buried.txt", "docs"), "deep/buried.txt");

        // From the top, the member path is already what it should be - and a
        // directory extracted keeps its shape.
        assert_eq!(landing("docs/notes.txt", ""), "docs/notes.txt");
        assert_eq!(landing("readme.txt", ""), "readme.txt");

        // A member that is not under the level is left alone rather than
        // mangled; it should not be in the list at all, and if it is, an
        // odd path beats a wrong one.
        assert_eq!(landing("elsewhere/a.txt", "docs"), "elsewhere/a.txt");
        // And a name that merely starts with the level is not under it.
        assert_eq!(landing("docsomething.txt", "docs"), "docsomething.txt");
    }

    #[test]
    fn an_extraction_says_what_it_is_and_is_recorded_as_a_copy() {
        let operation = Operation::Extract {
            archive: PathBuf::from("/home/me/papers.zip"),
            members: vec!["a.txt".into(), "b.txt".into()],
            from: String::new(),
            destination: PathBuf::from("/home/me/out"),
            password: None,
        };
        assert_eq!(
            operation.summarise(),
            "Extract 2 item(s) from papers.zip to /home/me/out"
        );
        assert_eq!(operation.verb(), "Extracting");
        assert_eq!(operation.past_tense(), "Extracted");
        // A copy whose source happens to be inside a file - so "where did
        // this come from" is one filter, not two.
        assert_eq!(operation.recorded_as(), journal::Kind::Copy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Records progress and can cancel after a fixed number of items, which
    /// makes cancellation deterministic instead of timing-dependent.
    struct TestSink {
        items: u64,
        bytes: u64,
        names: Vec<String>,
        cancel_after_items: Option<u64>,
        /// Answers handed out in order; the last one repeats.
        answers: Vec<Answer>,
        /// What was asked about, so a test can assert it was asked at all.
        asked: Vec<PathBuf>,
        /// The account of what happened, in the order it was written.
        events: Vec<journal::Event>,
    }

    impl TestSink {
        fn new() -> Self {
            TestSink {
                items: 0,
                bytes: 0,
                names: Vec::new(),
                cancel_after_items: None,
                answers: Vec::new(),
                asked: Vec::new(),
                events: Vec::new(),
            }
        }

        fn cancelling_after(items: u64) -> Self {
            TestSink {
                cancel_after_items: Some(items),
                ..Self::new()
            }
        }

        fn answering(answers: &[Answer]) -> Self {
            TestSink {
                answers: answers.to_vec(),
                ..Self::new()
            }
        }
    }

    impl Sink for TestSink {
        fn conflict(&mut self, conflict: &Conflict) -> Answer {
            self.asked.push(conflict.target.clone());
            if self.answers.len() > 1 {
                self.answers.remove(0)
            } else {
                // A sink with nothing scripted must not be the thing that
                // decides to destroy a file.
                self.answers.first().copied().unwrap_or(Answer::Skip)
            }
        }

        fn item_started(&mut self, name: &str) {
            self.names.push(name.to_string());
        }
        fn item_done(&mut self) {
            self.items += 1;
        }
        fn bytes_copied(&mut self, count: u64) {
            self.bytes += count;
        }
        fn cancelled(&self) -> bool {
            self.cancel_after_items
                .map(|limit| self.items >= limit)
                .unwrap_or(false)
        }
        fn happened(&mut self, event: journal::Event) {
            self.events.push(event);
        }
    }

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("a.txt"), vec![b'a'; 1000]).unwrap();
        fs::write(root.join("sub/b.txt"), vec![b'b'; 2000]).unwrap();
        fs::write(root.join("sub/deeper/c.txt"), vec![b'c'; 3000]).unwrap();
    }

    /// Real files, for the tests that hand them to the system's own trash.
    fn doomed(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                fs::write(&path, *name).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn trashing_several_still_records_and_counts_each_one() {
        // They go to the system in one call now, not one call each. What
        // comes back out has to be indistinguishable from having done them in
        // turn, because that is what the account and the bar are reading.
        let dir = tempfile::tempdir().unwrap();
        let targets = doomed(dir.path(), &["one.txt", "two.txt", "three.txt"]);

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Delete {
                targets: targets.clone(),
                to_trash: true,
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(sink.items, 3, "the bar counts files, not calls");
        assert_eq!(sink.events.len(), 3, "one record each, not one for the lot");
        assert!(sink
            .events
            .iter()
            .all(|event| event.kind == journal::Kind::Trash));

        // And each record names its own file, so the account can point at it.
        for target in &targets {
            let named = target.display().to_string();
            assert!(
                sink.events.iter().any(|event| event.path == named),
                "nothing recorded for {named}"
            );
            assert!(!target.exists(), "{named} is still there");
        }
    }

    #[test]
    fn a_trashing_that_partly_fails_names_the_file_that_would_not_go() {
        // The reason the batch reports per path. "Something in that call went
        // wrong" would be useless: it would neither name the bad file nor
        // clear the good ones.
        let dir = tempfile::tempdir().unwrap();
        let good = doomed(dir.path(), &["kept-going.txt", "went-too.txt"]);
        let missing = dir.path().join("never-existed.txt");

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Delete {
                targets: vec![good[0].clone(), missing, good[1].clone()],
                to_trash: true,
            },
            &mut sink,
        );

        assert_eq!(
            failures.len(),
            1,
            "one bad path, one complaint: {failures:?}"
        );
        assert!(
            failures[0].starts_with("never-existed.txt:"),
            "the complaint has to name the file: {}",
            failures[0]
        );
        assert_eq!(sink.items, 2, "the two that went still counted");
        assert_eq!(sink.events.len(), 2, "and only they were recorded");
        assert!(good.iter().all(|path| !path.exists()));
    }

    #[test]
    fn scan_counts_files_directories_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);

        let (items, bytes) = scan(&Operation::Delete {
            targets: vec![root.clone()],
            to_trash: false,
        });
        // 3 files + 3 directories (tree, sub, deeper).
        assert_eq!(items, 6);
        assert_eq!(bytes, 6000);
    }

    #[test]
    fn copy_reports_every_item_and_byte() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Copy {
                sources: vec![root.clone()],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(sink.items, 6);
        assert_eq!(sink.bytes, 6000);
        assert_eq!(
            fs::read(destination.join("tree/sub/deeper/c.txt"))
                .unwrap()
                .len(),
            3000
        );
    }

    #[test]
    fn cancelling_a_copy_stops_early_and_leaves_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        // Stop after the first two items are complete.
        let mut sink = TestSink::cancelling_after(2);
        let failures = execute(
            &Operation::Copy {
                sources: vec![root.clone()],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert_eq!(sink.items, 2, "should have stopped after two items");
        assert!(
            !failures.is_empty(),
            "cancellation is reported as a failure"
        );
        // The source is untouched by a cancelled copy.
        assert!(root.join("sub/deeper/c.txt").exists());
    }

    #[test]
    fn delete_removes_children_before_their_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Delete {
                targets: vec![root.clone()],
                to_trash: false,
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert!(!root.exists());
        assert_eq!(sink.items, 6);
        // A child is always reported before its parent directory.
        let deeper = sink
            .names
            .iter()
            .position(|n| n.ends_with("c.txt"))
            .unwrap();
        let parent = sink
            .names
            .iter()
            .position(|n| n.ends_with("deeper"))
            .unwrap();
        assert!(deeper < parent);
    }

    #[test]
    fn move_within_one_filesystem_uses_rename() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("m.txt");
        fs::write(&source, "payload").unwrap();
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Move {
                sources: vec![source.clone()],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination.join("m.txt")).unwrap(),
            "payload"
        );
    }

    #[test]
    fn copying_a_directory_into_itself_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Copy {
                sources: vec![root.clone()],
                destination: root.join("sub"),
            },
            &mut sink,
        );
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("into itself"), "{failures:?}");
    }

    #[test]
    fn a_failure_on_one_source_does_not_abort_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        fs::write(&good, "ok").unwrap();
        let missing = dir.path().join("missing.txt");
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Copy {
                sources: vec![missing, good],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert_eq!(failures.len(), 1);
        assert!(destination.join("good.txt").exists());
    }

    #[test]
    fn progress_fraction_prefers_bytes_and_is_clamped() {
        let mut p = Progress::new("Copying");
        assert_eq!(p.fraction(), 0.0);

        p.bytes_total = 1000;
        p.bytes_done = 250;
        assert_eq!(p.percent(), 25);

        // Item counts are the fallback when nothing has a size.
        let mut q = Progress::new("Deleting");
        q.items_total = 4;
        q.items_done = 3;
        assert_eq!(q.percent(), 75);

        // Finished always reads as complete.
        q.finished = true;
        assert_eq!(q.percent(), 100);
    }

    #[test]
    fn a_job_runs_on_a_thread_and_reports_completion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let mut job = Job::spawn(Operation::Copy {
            sources: vec![root],
            destination: destination.clone(),
        });
        job.join();

        let snapshot = job.snapshot();
        assert!(snapshot.finished);
        assert!(!snapshot.cancelled);
        assert!(snapshot.failures.is_empty(), "{:?}", snapshot.failures);
        assert_eq!(snapshot.items_done, 6);
        assert_eq!(snapshot.bytes_done, 6000);
        assert_eq!(snapshot.percent(), 100);
        assert!(destination.join("tree/a.txt").exists());
    }

    #[test]
    fn a_cancelled_job_reports_that_it_was_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        // Cancel before the worker starts. Calling request_cancel() after
        // spawning would race: this copy is small enough to finish first, and
        // an operation that already completed was not cancelled.
        let cancel = Arc::new(AtomicBool::new(true));
        let mut job = Job::spawn_with_cancel(
            Operation::Copy {
                sources: vec![root],
                destination: destination.clone(),
            },
            cancel,
            None,
        );
        job.join();

        let snapshot = job.snapshot();
        assert!(snapshot.cancelled);
        assert_eq!(snapshot.items_done, 0, "nothing should have been copied");
        assert!(!destination.join("tree").exists());
    }

    #[test]
    fn request_cancel_sets_the_flag_the_worker_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let job = Job::spawn_with_cancel(
            Operation::Copy {
                sources: vec![root],
                destination,
            },
            Arc::clone(&cancel),
            None,
        );
        job.request_cancel();
        assert!(cancel.load(Ordering::Relaxed), "the shared flag is raised");

        let mut job = job;
        job.join();
    }

    // ---- the account -------------------------------------------------------

    #[test]
    fn a_run_leaves_an_account_of_every_file_it_touched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();
        let journal = journal::Journal::at(dir.path().join("journal"), journal::Keep::default());

        let mut job = Job::spawn_recorded(
            Operation::Copy {
                sources: vec![root],
                destination: destination.clone(),
            },
            journal.clone(),
        );
        job.join();

        let rows = journal::arrange(journal.read(journal::Stream::Files, journal::Day::today()));
        assert_eq!(
            rows.len(),
            1,
            "one run, not a pile of loose lines: {rows:?}"
        );
        let journal::Row::Run { group, events, .. } = &rows[0] else {
            panic!("not grouped")
        };
        assert_eq!(group.kind, journal::Kind::Copy);
        assert!(
            group.summary.contains("Copy 1 item(s) to"),
            "{}",
            group.summary
        );
        assert!(!events.is_empty(), "the files it copied are named");

        // Both ends of every copy, which is the whole point - a count cannot
        // answer "where did that file go".
        for event in events {
            assert_eq!(event.kind, journal::Kind::Copy);
            assert!(event.to.is_some(), "{event:?}");
            assert_eq!(event.group, Some(group.id));
        }
        assert!(
            events.iter().any(|e| e.path.ends_with("a.txt")),
            "the file that was copied is named: {events:?}"
        );
    }

    #[test]
    fn a_delete_is_told_apart_from_a_trashing_in_the_account() {
        // One is recoverable and the other is not, which is exactly the
        // distinction anyone reading the account back is looking for.
        let dir = tempfile::tempdir().unwrap();
        let doomed = dir.path().join("doomed.txt");
        fs::write(&doomed, "x").unwrap();
        let journal = journal::Journal::at(dir.path().join("journal"), journal::Keep::default());

        let mut job = Job::spawn_recorded(
            Operation::Delete {
                targets: vec![doomed.clone()],
                to_trash: false,
            },
            journal.clone(),
        );
        job.join();

        let rows = journal::arrange(journal.read(journal::Stream::Files, journal::Day::today()));
        assert_eq!(rows[0].kind(), journal::Kind::Delete);
        let journal::Row::Run { events, .. } = &rows[0] else {
            panic!("not grouped")
        };
        assert_eq!(events.len(), 1);
        assert!(events[0].path.ends_with("doomed.txt"));
        assert!(journal::Kind::Delete.is_destructive());
        assert!(!journal::Kind::Trash.is_destructive());
    }

    #[test]
    fn a_run_with_no_journal_records_nothing_and_still_works() {
        // The rule the journal lives under: it is a record of the work, never
        // a participant in it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        tree(&root);
        let destination = dir.path().join("out");
        fs::create_dir_all(&destination).unwrap();

        let mut job = Job::spawn(Operation::Copy {
            sources: vec![root],
            destination: destination.clone(),
        });
        job.join();
        assert!(job.snapshot().finished);
        assert!(destination.join("tree").join("a.txt").exists());
    }

    // ---- overwriting ------------------------------------------------------

    /// A source file and an existing target with different contents.
    fn collision(dir: &Path) -> (PathBuf, PathBuf) {
        let source_dir = dir.join("from");
        let target_dir = dir.join("to");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        let source = source_dir.join("notes.txt");
        let target = target_dir.join("notes.txt");
        fs::write(&source, "new").unwrap();
        fs::write(&target, "PRECIOUS").unwrap();
        (source, target)
    }

    #[test]
    fn copying_onto_an_existing_file_asks_first() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = collision(dir.path());

        // Nothing scripted: the sink's safe default is Skip, and the file
        // that was already there is still there.
        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Copy {
                sources: vec![source.clone()],
                destination: target.parent().unwrap().to_path_buf(),
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(sink.asked, vec![target.clone()], "it never asked");
        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");
        // Skipped, but counted - or the bar would stop short of the end.
        assert_eq!(sink.items, 1);
    }

    #[test]
    fn overwrite_writes_and_skip_does_not() {
        for (answer, expected) in [(Answer::Overwrite, "new"), (Answer::Skip, "PRECIOUS")] {
            let dir = tempfile::tempdir().unwrap();
            let (source, target) = collision(dir.path());
            let mut sink = TestSink::answering(&[answer]);
            execute(
                &Operation::Copy {
                    sources: vec![source],
                    destination: target.parent().unwrap().to_path_buf(),
                },
                &mut sink,
            );
            assert_eq!(fs::read_to_string(&target).unwrap(), expected, "{answer:?}");
        }
    }

    #[test]
    fn moving_onto_an_existing_file_asks_too() {
        // `fs::rename` replaces the target without a word, so the question
        // has to be asked before it is called rather than left to it.
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = collision(dir.path());

        let mut sink = TestSink::answering(&[Answer::Skip]);
        execute(
            &Operation::Move {
                sources: vec![source.clone()],
                destination: target.parent().unwrap().to_path_buf(),
            },
            &mut sink,
        );

        assert_eq!(sink.asked, vec![target.clone()], "it never asked");
        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");
        // Skipped means the source is still where it was.
        assert!(source.exists(), "the source was moved away anyway");
    }

    #[test]
    fn all_is_asked_for_once_and_then_stands() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        let mut sources = Vec::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(from.join(name), "new").unwrap();
            fs::write(to.join(name), "old").unwrap();
            sources.push(from.join(name));
        }

        let mut sink = TestSink::answering(&[Answer::OverwriteAll]);
        execute(
            &Operation::Copy {
                sources,
                destination: to.clone(),
            },
            &mut sink,
        );

        assert_eq!(
            sink.asked.len(),
            1,
            "asked more than once: {:?}",
            sink.asked
        );
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(fs::read_to_string(to.join(name)).unwrap(), "new", "{name}");
        }
    }

    #[test]
    fn skip_all_leaves_every_one_of_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        let mut sources = Vec::new();
        for name in ["a.txt", "b.txt"] {
            fs::write(from.join(name), "new").unwrap();
            fs::write(to.join(name), "old").unwrap();
            sources.push(from.join(name));
        }
        // ...and one with no collision, which must still be copied.
        fs::write(from.join("fresh.txt"), "fresh").unwrap();
        sources.push(from.join("fresh.txt"));

        let mut sink = TestSink::answering(&[Answer::SkipAll]);
        execute(
            &Operation::Copy {
                sources,
                destination: to.clone(),
            },
            &mut sink,
        );

        assert_eq!(sink.asked.len(), 1);
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(to.join("b.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(to.join("fresh.txt")).unwrap(), "fresh");
    }

    #[test]
    fn answering_cancel_stops_the_whole_operation() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        for name in ["a.txt", "b.txt"] {
            fs::write(from.join(name), "new").unwrap();
            fs::write(to.join(name), "old").unwrap();
        }

        let mut sink = TestSink::answering(&[Answer::Cancel]);
        execute(
            &Operation::Copy {
                sources: vec![from.join("a.txt"), from.join("b.txt")],
                destination: to.clone(),
            },
            &mut sink,
        );

        assert_eq!(sink.asked.len(), 1, "kept asking after Cancel");
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(to.join("b.txt")).unwrap(), "old");
    }

    #[test]
    fn collisions_inside_a_tree_are_asked_about_one_by_one() {
        // The guard used to be only at the top level, so a nested file with
        // the same name went straight through it.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("from/tree");
        let destination = dir.path().join("to");
        fs::create_dir_all(source.join("deep")).unwrap();
        fs::write(source.join("deep/x.txt"), "new").unwrap();
        fs::create_dir_all(destination.join("tree/deep")).unwrap();
        fs::write(destination.join("tree/deep/x.txt"), "old").unwrap();

        let mut sink = TestSink::answering(&[Answer::Skip]);
        execute(
            &Operation::Copy {
                sources: vec![source],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert_eq!(sink.asked, vec![destination.join("tree/deep/x.txt")]);
        assert_eq!(
            fs::read_to_string(destination.join("tree/deep/x.txt")).unwrap(),
            "old"
        );
    }

    #[test]
    fn copying_a_file_onto_itself_is_refused_rather_than_asked_about() {
        // `File::create` would truncate it and then copy back the nothing
        // that was left. There is no answer that means anything here.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only.txt");
        fs::write(&file, "content").unwrap();

        let mut sink = TestSink::answering(&[Answer::OverwriteAll]);
        let failures = execute(
            &Operation::Copy {
                sources: vec![file.clone()],
                destination: dir.path().to_path_buf(),
            },
            &mut sink,
        );

        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "content");
    }

    #[test]
    fn a_directory_in_the_way_of_a_file_is_an_error_not_a_question() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(to.join("thing")).unwrap();
        fs::write(from.join("thing"), "a file").unwrap();

        let mut sink = TestSink::answering(&[Answer::OverwriteAll]);
        let failures = execute(
            &Operation::Copy {
                sources: vec![from.join("thing")],
                destination: to.clone(),
            },
            &mut sink,
        );

        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(sink.asked.is_empty(), "asked a question with no answer");
        assert!(to.join("thing").is_dir(), "the directory was replaced");
    }

    #[test]
    fn directories_merge_without_being_asked_about() {
        // Two directories of the same name is a merge, not a collision. Only
        // the files that actually land on each other are questions.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("from/shared");
        let destination = dir.path().join("to");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("new.txt"), "new").unwrap();
        fs::create_dir_all(destination.join("shared")).unwrap();
        fs::write(destination.join("shared/kept.txt"), "kept").unwrap();

        let mut sink = TestSink::new();
        let failures = execute(
            &Operation::Copy {
                sources: vec![source],
                destination: destination.clone(),
            },
            &mut sink,
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert!(sink.asked.is_empty(), "asked about a merge");
        assert_eq!(
            fs::read_to_string(destination.join("shared/new.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(destination.join("shared/kept.txt")).unwrap(),
            "kept"
        );
    }

    #[test]
    fn a_conflict_carries_what_the_answer_turns_on() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = collision(dir.path());

        let conflict = Conflict::read(&source, &target).unwrap();
        assert_eq!(conflict.source_size, 3); // "new"
        assert_eq!(conflict.target_size, 8); // "PRECIOUS"

        // The dates come from the filesystem, whose stamp resolution is not
        // something a test should race - so the comparison is checked against
        // times the test sets itself.
        let now = SystemTime::now();
        let hour = Duration::from_secs(3600);
        let newer_source = Conflict {
            source_modified: Some(now),
            target_modified: Some(now - hour),
            ..conflict.clone()
        };
        assert_eq!(newer_source.source_is_newer(), Some(true));
        let older_source = Conflict {
            source_modified: Some(now - hour),
            target_modified: Some(now),
            ..conflict.clone()
        };
        assert_eq!(older_source.source_is_newer(), Some(false));
        // Unknown on either side stays unknown rather than becoming a guess.
        let unknown = Conflict {
            source_modified: None,
            ..conflict
        };
        assert_eq!(unknown.source_is_newer(), None);
    }

    /// A conflict between two files stamped `seconds` apart, source later.
    fn dated(source: u64, target: u64) -> Conflict {
        Conflict {
            source: "/a".into(),
            target: "/b".into(),
            source_size: 1,
            target_size: 2,
            source_modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(source)),
            target_modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(target)),
        }
    }

    #[test]
    fn a_run_that_left_files_alone_says_so() {
        let mut progress = Progress::new("Copied");
        progress.items_done = 4;
        assert_eq!(progress.outcome("Copied"), "Copied 4 item(s)");

        // Two written, two left where they were - which is what "only newer"
        // is for, and the opposite of what "copied 4" would say.
        progress.items_skipped = 2;
        assert_eq!(progress.outcome("Copied"), "Copied 2 item(s), left 2 alone");

        progress.items_skipped = 4;
        assert_eq!(progress.outcome("Copied"), "Copied 0 item(s), left 4 alone");
    }

    #[test]
    fn a_question_stops_being_asked_the_moment_it_is_answered() {
        // The worker needs a few microseconds to wake up and take the answer.
        // A UI polling in that window used to find the question still on the
        // record and put the box it had just answered straight back up.
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.txt"), "arriving").unwrap();
        std::fs::write(to.join("a.txt"), "already there").unwrap();

        let job = Job::spawn(Operation::Copy {
            sources: vec![from.join("a.txt")],
            destination: to.clone(),
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while job.asking().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(job.asking().is_some(), "the copy never asked");

        job.answer(Answer::Skip);
        assert!(
            job.asking().is_none(),
            "answered, and still on the record for the next poll to find"
        );
    }

    #[test]
    fn only_newer_answers_every_later_conflict_on_its_own() {
        let mut conflicts = Conflicts::default();
        let mut sink = TestSink::answering(&[Answer::OnlyNewer]);

        // The one it is answered on, and then the rest without asking.
        assert_eq!(
            conflicts.resolve(&mut sink, &dated(900, 100)),
            Resolution::Overwrite,
            "the file arriving is the newer one"
        );
        assert_eq!(conflicts.standing(), Some(Standing::OnlyNewer));
        assert_eq!(
            conflicts.resolve(&mut sink, &dated(100, 900)),
            Resolution::Skip,
            "and the one already there is newer, so it stays"
        );
        assert_eq!(
            sink.asked.len(),
            1,
            "a rule is asked for once and then applied"
        );
    }

    #[test]
    fn only_newer_leaves_alone_what_it_cannot_call_newer() {
        let standing = Standing::OnlyNewer;

        // Stamped the same moment: not newer, so not overwritten. The
        // tolerance matters - a file already copied to a FAT stick comes back
        // up to two seconds off and must not be copied again.
        assert_eq!(standing.decide(&dated(100, 100)), Resolution::Skip);
        assert_eq!(standing.decide(&dated(102, 100)), Resolution::Skip);
        assert_eq!(standing.decide(&dated(103, 100)), Resolution::Overwrite);

        // No date to go on is not evidence of being newer.
        let undated = Conflict {
            source_modified: None,
            target_modified: None,
            ..dated(0, 0)
        };
        assert_eq!(standing.decide(&undated), Resolution::Skip);
    }

    #[test]
    fn a_standing_answer_is_only_asked_for_once() {
        let mut conflicts = Conflicts::default();
        let mut sink = TestSink::answering(&[Answer::OverwriteAll]);
        let conflict = Conflict {
            source: "/a".into(),
            target: "/b".into(),
            source_size: 1,
            target_size: 2,
            source_modified: None,
            target_modified: None,
        };
        assert_eq!(conflicts.standing(), None);
        assert_eq!(
            conflicts.resolve(&mut sink, &conflict),
            Resolution::Overwrite
        );
        assert_eq!(
            conflicts.standing(),
            Some(Standing::Always(Resolution::Overwrite))
        );
        assert_eq!(
            conflicts.resolve(&mut sink, &conflict),
            Resolution::Overwrite
        );
        assert_eq!(sink.asked.len(), 1);

        // A one-off answer does not stand.
        let mut conflicts = Conflicts::default();
        let mut sink = TestSink::answering(&[Answer::Overwrite]);
        conflicts.resolve(&mut sink, &conflict);
        assert_eq!(conflicts.standing(), None);
        conflicts.resolve(&mut sink, &conflict);
        assert_eq!(sink.asked.len(), 2);
    }

    #[test]
    fn a_job_posts_the_question_and_waits_for_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = collision(dir.path());

        let job = Job::spawn(Operation::Copy {
            sources: vec![source],
            destination: target.parent().unwrap().to_path_buf(),
        });

        // The worker blocks until answered, so this is a wait, not a race.
        let deadline = Instant::now() + Duration::from_secs(10);
        let asked = loop {
            if let Some(conflict) = job.asking() {
                break Some(conflict);
            }
            if job.is_finished() || Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let asked = asked.expect("the job never asked");
        assert_eq!(asked.target, target);
        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");

        job.answer(Answer::Overwrite);
        let mut job = job;
        job.join();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }
}
