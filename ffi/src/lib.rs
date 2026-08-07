// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The C ABI that a native front-end calls into.
//!
//! The engine is Rust and the Windows front-end is C#, so something has to sit
//! between them. This is it, and it is deliberately narrow: the front-end can
//! list a directory, make one, rename something, and start a copy, move or
//! delete that it can then watch and stop. Nothing else crosses yet - not
//! archives, not the terminal, not the journal. Widening it is meant to be a
//! decision each time, not a drift.
//!
//! Note the shape of the job entry point: one [`rcmd_job_start`] taking a
//! tagged request, rather than a C function per operation. The next operation
//! is a variant and a `match` arm, not another signature to get wrong, another
//! thing to free, and another place to forget a check.
//!
//! ## The shape, and why
//!
//! **Values cross as JSON, not as structs.** A `#[repr(C)]` mirror of every
//! engine type would be faster and would also be a second definition of each
//! one, silently free to disagree with the first.
//!
//! This used to say that serialising costs a few microseconds against a
//! `read_dir` that costs milliseconds, and that a flat array of fixed-width
//! records was the answer if it ever became the slow part - when measured,
//! not in advance. It has now been measured, and the old claim was wrong.
//! `C:\Windows\WinSxS`, 32,724 entries: the listing takes about 400ms and
//! serialising it takes about a second, for 10MB of JSON. Serialising is not
//! a rounding error at that size; it is the larger half.
//!
//! Three things follow, in the order worth doing them:
//!
//! 1. Most of those 10MB is the full path on every row, which is redundant -
//!    each one is the listing's own path plus the name. Leaving it out is 40%
//!    fewer bytes for about 20% less time, since serde's per-field cost
//!    outweighs the byte count.
//! 2. Neither number matters as much as *which thread pays them*. A second of
//!    work behind a progress indicator is a directory that takes a moment; the
//!    same second on the UI thread is a window that has stopped responding.
//! 3. Only then is the flat-array rewrite worth considering, and it should be
//!    measured against 1 and 2 rather than instead of them.
//!
//! **The front-end polls; nothing calls back into it.** A function pointer
//! into managed code, invoked from a Rust worker thread, is how interop gets
//! frightening: the garbage collector may move things, and the callback must
//! never unwind. [`Job`] already keeps its state behind a lock and hands out a
//! snapshot, so the front-end asking "how far along?" on a timer needs none of
//! that.
//!
//! **Nothing here may unwind into C.** A panic crossing the boundary is
//! undefined behaviour in C's terms, so every entry point catches one and
//! turns it into an error the caller can see.
//!
//! ## Who owns what
//!
//! Every `*mut c_char` returned here was allocated by Rust and must be given
//! back to [`rcmd_string_free`]. Every job handle from [`rcmd_job_start`]
//! must be given back to [`rcmd_job_free`]. Freeing either with the C#
//! allocator, or twice, corrupts the heap.

use std::ffi::{c_char, CStr, CString};
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use lost_commander_core::archive;
use lost_commander_core::compare;
use lost_commander_core::encoding;
use lost_commander_core::entry::{Entry, EntryKind};
use lost_commander_core::journal;
use lost_commander_core::netloc;
use lost_commander_core::panel::{read_entries, Order, SortBy};
use lost_commander_core::progress::{Job, Operation};

// ---- what crosses -------------------------------------------------------

#[derive(Serialize)]
struct EntryDto {
    name: String,
    path: String,
    /// `"parent"`, `"dir"` or `"file"` - the front-end sorts and draws by this.
    kind: &'static str,
    /// What the file looks like it is, from its name: `"image"`, `"code"`,
    /// `"archive"` and so on, or `"plain"` for anything unrecognised.
    ///
    /// A guess from the extension, made without opening anything, because it
    /// is wanted for every row of a directory that may hold ten thousand. Each
    /// front-end turns it into a picture its own way - the graphical one paints
    /// shapes, a native one asks the shell for the icon the rest of the desktop
    /// already uses - so what crosses is the bucket, never the drawing.
    filekind: &'static str,
    size: u64,
    /// Seconds since the Unix epoch, or null where the filesystem gave none.
    modified: Option<i64>,
    is_symlink: bool,
    is_dir: bool,
}

impl From<&Entry> for EntryDto {
    fn from(entry: &Entry) -> Self {
        EntryDto {
            name: entry.name.clone(),
            path: entry.path.display().to_string(),
            kind: match entry.kind {
                EntryKind::Parent => "parent",
                EntryKind::Dir => "dir",
                EntryKind::File => "file",
            },
            filekind: lost_commander_core::filekind::classify(entry).label(),
            size: entry.size,
            modified: entry.modified.and_then(|time| {
                time.duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_secs() as i64)
            }),
            is_symlink: entry.is_symlink,
            is_dir: entry.is_dir(),
        }
    }
}

#[derive(Serialize)]
struct Listing {
    path: String,
    entries: Vec<EntryDto>,
}

/// What an immediate operation produced, so the front-end can put the cursor
/// on it: making a directory and then not showing it is half the job.
#[derive(Serialize)]
struct Made {
    path: String,
}

/// One boolean per name asked about, in the order they were asked.
#[derive(Serialize)]
struct Matched {
    matched: Vec<bool>,
}

/// One label per tab, in the order the tabs were given.
#[derive(Serialize)]
struct Titles {
    titles: Vec<String>,
}

/// One visible row of the directory tree.
#[derive(Serialize)]
struct TreeNode {
    path: String,
    /// What to draw. The same as `name` everywhere but the root, which has no
    /// file name and shows its whole path instead.
    label: String,
    /// How far to indent it.
    depth: usize,
    expanded: bool,
    /// True once we have looked and found nothing to open, so the front-end
    /// can stop offering to open it. Always true for a file.
    leaf: bool,

    // The rest is exactly what a listing row carries, and is here so that a
    // front-end can put tree rows through the same code as listing rows -
    // marking, sorting into a selection, handing to a copy. A tree whose rows
    // were a different shape would need every operation written twice, and the
    // second copy is the one that goes wrong.
    name: String,
    kind: &'static str,
    filekind: &'static str,
    size: u64,
    modified: Option<i64>,
    is_symlink: bool,
    is_dir: bool,
}

#[derive(Serialize)]
struct TreeNodes {
    nodes: Vec<TreeNode>,
}

/// A saved or recently visited place.
#[derive(Serialize)]
struct Place {
    name: String,
    /// Where to go. For a local place this is a path something can `cd` to.
    path: String,
    /// How to show it, and what to save it back as.
    url: String,
    /// True for a share that has to be connected before it can be listed.
    ///
    /// It crosses so the front-end can show the place without pretending it
    /// can open it. Hiding network places would lose bookmarks the terminal
    /// front-end saved; offering them as though they were folders would fail
    /// at the click.
    network: bool,
}

impl From<&netloc::Location> for Place {
    fn from(location: &netloc::Location) -> Self {
        Place {
            name: location.name.clone(),
            path: location.path.clone(),
            url: location.to_url(),
            network: location.protocol.is_network(),
        }
    }
}

#[derive(Serialize)]
struct Places {
    pinned: Vec<Place>,
    /// Most recent first.
    recent: Vec<Place>,
}

#[derive(Serialize)]
struct ProgressDto {
    verb: String,
    current: String,
    items_done: u64,
    items_total: u64,
    items_skipped: u64,
    bytes_done: u64,
    bytes_total: u64,
    /// 0.0 to 1.0, worked out by the engine so both front-ends draw the same
    /// bar - bytes where there are any, because one huge file would otherwise
    /// sit at zero.
    fraction: f64,
    finished: bool,
    cancelled: bool,
    failures: Vec<String>,
}

/// A running job. The front-end only ever holds the pointer.
pub struct RcmdJob {
    job: Job,
}

/// What the front-end asked to have done.
///
/// One tagged shape rather than a C function per operation: adding the next
/// one is a variant here and a `match` arm below, not another entry point,
/// another signature to get wrong and another thing to free. It is also the
/// only place the front-end's request is checked, so the checking is in one
/// place instead of repeated per function.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Requested {
    Copy {
        sources: Vec<String>,
        destination: String,
    },
    Move {
        sources: Vec<String>,
        destination: String,
    },
    /// Copying out of an archive. A copy whose source happens to be inside a
    /// file, which is why it is a variant here and not a second job API.
    Extract {
        archive: String,
        /// Member paths inside the archive.
        members: Vec<String>,
        /// The level being looked at, which comes off the front of each member
        /// on the way out - extracting from two levels down puts the files
        /// *here*, rather than rebuilding the archive's whole tree under the
        /// destination.
        from: String,
        destination: String,
        /// For this run only. Never journaled, never written down.
        password: Option<String>,
    },
    Delete {
        targets: Vec<String>,
        /// To the recycle bin, where it can be got back. The front-end has to
        /// say which it means: defaulting either way would eventually delete
        /// something permanently that was meant to be recoverable.
        to_trash: bool,
    },
}

impl Requested {
    /// The engine's operation, or `None` if there is nothing to do.
    ///
    /// An empty list is refused rather than started. A job over no files
    /// finishes instantly and reports success, which reads as "it worked" when
    /// what happened is that nothing was selected.
    fn into_operation(self) -> Option<Operation> {
        fn paths(from: Vec<String>) -> Option<Vec<PathBuf>> {
            if from.is_empty() {
                return None;
            }
            Some(from.into_iter().map(PathBuf::from).collect())
        }

        match self {
            Requested::Copy {
                sources,
                destination,
            } => Some(Operation::Copy {
                sources: paths(sources)?,
                destination: PathBuf::from(destination),
            }),
            Requested::Move {
                sources,
                destination,
            } => Some(Operation::Move {
                sources: paths(sources)?,
                destination: PathBuf::from(destination),
            }),
            Requested::Extract {
                archive,
                members,
                from,
                destination,
                password,
            } => Some(Operation::Extract {
                archive: PathBuf::from(archive),
                members: {
                    if members.is_empty() {
                        return None;
                    }
                    members
                },
                from,
                destination: PathBuf::from(destination),
                password,
            }),
            Requested::Delete { targets, to_trash } => Some(Operation::Delete {
                targets: paths(targets)?,
                to_trash,
            }),
        }
    }
}

/// What a file turned out to be when it was read.
#[derive(Serialize)]
struct TextFile {
    text: String,
    /// The encoding's own label, so handing it straight back to a save means
    /// the file is written as whatever it was read as.
    encoding: String,
    /// The same with the doubt attached - "(a guess)" - for showing.
    described: String,
    newline: String,
    /// True where the file was longer than the cap. The front-end has to know:
    /// saving what was read would otherwise chop the rest of the file off.
    truncated: bool,
}

/// What a save did.
#[derive(Serialize)]
struct Written {
    bytes: u64,
    /// What the encoding could not hold, or null. Only ever set on a save that
    /// was explicitly allowed to lose something.
    lost: Option<String>,
}

/// Fold away `.` and `..` so an address bar shows a path a person would write.
///
/// The engine's, because both Rust front-ends need exactly the same answer
/// and one of them was using `std::fs::canonicalize` instead - which on
/// Windows shows `\?\C:\src` in the pane header.
use lost_commander_core::paths::tidied;

/// An encoding by its label, matched loosely enough to survive a round trip.
fn encoding_named(name: &str) -> Option<encoding::Encoding> {
    let wanted = name.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return None;
    }
    encoding::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.label().to_ascii_lowercase() == wanted)
}

fn newline_named(name: &str) -> Option<encoding::Newline> {
    let wanted = name.trim().to_ascii_uppercase();
    encoding::NEWLINES
        .iter()
        .copied()
        .find(|candidate| candidate.label() == wanted)
}

/// Which way round to sort. An empty or unknown name means the column's own
/// natural order, which is what picking a fresh column should give you.
fn order_named(name: &str, sort: SortBy) -> Order {
    match name.trim().to_ascii_lowercase().as_str() {
        "asc" | "ascending" => Order::Ascending,
        "desc" | "descending" => Order::Descending,
        _ => sort.natural_order(),
    }
}

/// How a listing is ordered. Unknown names fall back to name order rather than
/// failing: a listing in the wrong order is a smaller problem than no listing.
fn sort_named(name: &str) -> SortBy {
    match name {
        "ext" | "extension" => SortBy::Ext,
        "size" => SortBy::Size,
        "time" | "modified" => SortBy::Time,
        _ => SortBy::Name,
    }
}

// ---- plumbing -----------------------------------------------------------

/// Hand a string to the caller. Freed by [`rcmd_string_free`], never by C#.
fn out(text: String) -> *mut c_char {
    // A NUL inside would truncate the JSON. Nothing here should produce one,
    // but a filename is not ours to trust, so it is replaced rather than
    // allowed to cut the reply short.
    let cleaned = text.replace('\0', "\u{fffd}");
    match CString::new(cleaned) {
        Ok(owned) => owned.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// A reply the front-end can always parse, whatever went wrong.
fn failed(message: impl std::fmt::Display) -> *mut c_char {
    let escaped = serde_json::to_string(&message.to_string())
        .unwrap_or_else(|_| "\"something went wrong\"".to_string());
    out(format!("{{\"error\":{escaped}}}"))
}

/// A string as JSON, quotes and escapes included.
///
/// For the handful of replies built by hand rather than derived. A path can
/// hold a backslash and a quote, and both would end the string early.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

fn replied<T: Serialize>(value: &T) -> *mut c_char {
    match serde_json::to_string(value) {
        Ok(json) => out(json),
        Err(e) => failed(e),
    }
}

/// Read a C string the caller owns.
///
/// # Safety
/// `text` must be null or a valid NUL-terminated string that stays alive for
/// this call.
unsafe fn borrowed(text: *const c_char) -> Result<String, String> {
    if text.is_null() {
        return Err("a null string was passed where one was needed".to_string());
    }
    CStr::from_ptr(text)
        .to_str()
        .map(|s| s.to_string())
        .map_err(|_| "the string was not valid UTF-8".to_string())
}

/// Run a body that returns a reply, turning a panic into one.
fn guarded(body: impl FnOnce() -> *mut c_char) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(reply) => reply,
        // Deliberately not re-raised: unwinding into C is undefined, and the
        // front-end can show this.
        Err(_) => failed("the engine panicked"),
    }
}

// ---- the boundary -------------------------------------------------------

/// The engine's version, so the front-end can prove it loaded the right DLL.
#[no_mangle]
pub extern "C" fn rcmd_version() -> *mut c_char {
    guarded(|| out(format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION"))))
}

/// List a directory.
///
/// `sort` is `"name"`, `"ext"`, `"size"` or `"time"`; anything else is name
/// order. `order` is `"asc"` or `"desc"`; anything else - including empty -
/// means that column's natural order, which is A to Z for a name and biggest
/// or newest first for a size or a date. Returns `{"path":...,"entries":[...]}`
/// or `{"error":"..."}`.
///
/// Sorting is done here rather than in the front-end so that both front-ends
/// order a directory identically - including the rule that directories come
/// before files whatever the column, which is a thing a plain sort does not do.
///
/// # Safety
/// `path` and `sort` must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_list(
    path: *const c_char,
    show_hidden: u8,
    sort: *const c_char,
    order: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => PathBuf::from(path),
            Err(e) => return failed(e),
        };
        let sort = borrowed(sort)
            .map(|name| sort_named(&name))
            .unwrap_or(SortBy::Name);
        let order = borrowed(order)
            .map(|name| order_named(&name, sort))
            .unwrap_or_else(|_| sort.natural_order());
        match read_entries(&path, show_hidden != 0, sort, order) {
            // The ".." row is already the first of these: `read_entries` puts
            // it there, so both front-ends agree on whether there is one.
            // Adding another here put two of them in the pane, which is what
            // running the window showed and reading the code did not.
            Ok(entries) => replied(&Listing {
                path: path.display().to_string(),
                entries: entries.iter().map(EntryDto::from).collect(),
            }),
            Err(e) => failed(e),
        }
    })
}

/// Start a copy, a move or a delete.
///
/// `request` is JSON: `{"kind":"copy","sources":[...],"destination":"..."}`,
/// the same with `"move"`, or
/// `{"kind":"delete","targets":[...],"to_trash":true}`.
///
/// Returns null if the request cannot be read or names nothing to work on, in
/// which case nothing was started.
///
/// # Safety
/// `request` must be a valid NUL-terminated UTF-8 string. The handle returned
/// must be given to [`rcmd_job_free`] exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_job_start(request: *const c_char) -> *mut RcmdJob {
    let started = catch_unwind(AssertUnwindSafe(|| {
        let request = borrowed(request).ok()?;
        let requested: Requested = serde_json::from_str(&request).ok()?;
        let operation = requested.into_operation()?;
        // Recorded, not merely run. The engine keeps an account of what was
        // done - what moved where, what failed, how long a run took - and a
        // front-end that used `Job::spawn` would do the work and write none of
        // it down, leaving the journal empty and saying nothing had happened.
        // Which is what it did, until a viewer was built and showed it.
        Some(match account() {
            Some(book) => Job::spawn_recorded(operation, book),
            // Nowhere to write one: the work still goes ahead. An account is
            // worth keeping, and not worth refusing to copy a file over.
            None => Job::spawn(operation),
        })
    }));

    match started {
        Ok(Some(job)) => Box::into_raw(Box::new(RcmdJob { job })),
        _ => std::ptr::null_mut(),
    }
}

/// Make a directory called `name` inside `parent`.
///
/// Returns `{"path":"..."}` or `{"error":"..."}`. The name is checked by the
/// engine, which refuses separators, `.`, `..` and anything already there -
/// so a front-end cannot talk it into creating a directory somewhere else.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_mkdir(parent: *const c_char, name: *const c_char) -> *mut c_char {
    guarded(|| {
        let (parent, name) = match (borrowed(parent), borrowed(name)) {
            (Ok(parent), Ok(name)) => (parent, name),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        match lost_commander_core::fsops::create_dir(Path::new(&parent), &name) {
            Ok(path) => replied(&Made {
                path: path.display().to_string(),
            }),
            Err(e) => failed(e),
        }
    })
}

/// Work out where a typed path means, relative to where a pane is standing.
///
/// This is the engine's own `cd` resolution, not a second one written beside
/// it: `~/`, `%VARIABLES%`, `..` and plain relative names all mean here what
/// they mean when typed at the command line in the other front-ends. An
/// address bar that only accepted absolute paths would be a different program
/// wearing the same name.
///
/// Empty, or `~`, means home. Returns `{"path":"..."}` or `{"error":"..."}`;
/// the path is not checked for existing, because saying "no such directory"
/// is the caller's job and it wants the resolved name to say it with.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_resolve_path(
    typed: *const c_char,
    cwd: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let (typed, cwd) = match (borrowed(typed), borrowed(cwd)) {
            (Ok(typed), Ok(cwd)) => (typed, cwd),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let trimmed = typed.trim();
        let target = if trimmed.is_empty() || trimmed == "~" {
            lost_commander_core::shell::Intercepted::ChangeToHome
        } else {
            lost_commander_core::shell::Intercepted::ChangeTo(trimmed.to_string())
        };
        match lost_commander_core::shell::resolve_cd(&target, Path::new(&cwd)) {
            Some(path) => replied(&Made {
                path: tidied(&path).display().to_string(),
            }),
            None => failed("could not work out where that is"),
        }
    })
}

/// Read a file as text, working out what encoding it is in.
///
/// `as_encoding` forces one by label; empty means "work it out". Reading
/// everything as UTF-8 is what turns a Cyrillic or a Windows-made file into a
/// screen of replacement characters, which from the outside looks exactly like
/// a corrupt file - so what was detected comes back with the text and the
/// front-end can offer to read it another way.
///
/// Stops after `max_bytes` and says so, rather than pulling a gigabyte log
/// into memory to show the first screen of it.
///
/// # Safety
/// `path` and `as_encoding` must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_read_text(
    path: *const c_char,
    max_bytes: u64,
    as_encoding: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let forced = borrowed(as_encoding)
            .ok()
            .and_then(|name| encoding_named(&name));

        // Read here rather than through `fsops::read_preview`, which turns
        // tabs into spaces for the terminal viewer. That is a display choice
        // and it is not reversible: saving the result would replace every tab
        // in the file.
        use std::io::Read;

        let cap = max_bytes.min(usize::MAX as u64) as usize;
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) => return failed(e),
        };
        let mut buffer = Vec::new();
        // One byte past the cap, so a file that exactly fills it is not
        // reported as cut short.
        if let Err(e) = file.take(cap as u64 + 1).read_to_end(&mut buffer) {
            return failed(e);
        }
        // One byte over the cap is how we know there was more.
        let truncated = buffer.len() > cap;
        buffer.truncate(cap);

        let detected = encoding::sniff(&buffer);
        let used = forced.unwrap_or(detected.encoding);
        let text = encoding::decode(&buffer, used);
        let newline = encoding::sniff_newline(&text);

        replied(&TextFile {
            encoding: used.label().to_string(),
            // Where an encoding was forced there is no doubt to report; the
            // caller chose it.
            described: if forced.is_some() {
                used.label().to_string()
            } else {
                detected.describe()
            },
            newline: newline.label().to_string(),
            truncated,
            text,
        })
    })
}

/// Write text back, in the encoding and line ending given.
///
/// Refuses a save that would lose characters unless `allow_loss` says
/// otherwise, and says what would be lost. Writing a file back as CP1252 when
/// it has picked up a character CP1252 has no room for is a silent loss, and
/// silent loss on save is the one thing an editor must never do - so the
/// front-end has to ask, and be seen to ask.
///
/// # Safety
/// All three string arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_write_text(
    path: *const c_char,
    text: *const c_char,
    as_encoding: *const c_char,
    newline: *const c_char,
    allow_loss: u8,
) -> *mut c_char {
    guarded(|| {
        let (path, text) = match (borrowed(path), borrowed(text)) {
            (Ok(path), Ok(text)) => (path, text),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let used = borrowed(as_encoding)
            .ok()
            .and_then(|name| encoding_named(&name))
            .unwrap_or_default();
        let ending = borrowed(newline)
            .ok()
            .and_then(|name| newline_named(&name))
            .unwrap_or_default();

        let encoded = encoding::encode(&encoding::to_newline(&text, ending), used);
        let complaint = encoded.complaint(used);
        if let Some(complaint) = &complaint {
            if allow_loss == 0 {
                // Nothing is written. The file on disk is still the good one.
                return failed(complaint);
            }
        }

        match std::fs::write(&path, &encoded.bytes) {
            Ok(()) => replied(&Written {
                bytes: encoded.bytes.len() as u64,
                lost: complaint,
            }),
            Err(e) => failed(e),
        }
    })
}

/// Everything about one file, for a properties window.
#[derive(Serialize)]
struct PropertiesDto {
    name: String,
    path: String,
    kind: &'static str,
    size: u64,
    modified: Option<i64>,
    accessed: Option<i64>,
    created: Option<i64>,
    is_symlink: bool,
    /// Where a link points, which is what someone opened this to find out.
    link_target: Option<String>,
    /// The Unix bits as `rwxr-xr-x`, or null on a platform without them.
    mode: Option<String>,
    owner: Option<String>,
    group: Option<String>,
    /// The one Windows has that is worth a checkbox.
    readonly: bool,
}

/// Read everything about one path.
///
/// Reports the link itself rather than what it points at, because the window
/// is about the file that was selected - its own size, its own permissions,
/// and where it points, which is the thing worth knowing about a link.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_properties(path: *const c_char) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let facts = match lost_commander_core::perms::read(Path::new(&path)) {
            Ok(facts) => facts,
            Err(e) => return failed(e),
        };
        let seconds = |time: Option<std::time::SystemTime>| {
            time.and_then(|t| {
                t.duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_secs() as i64)
            })
        };
        replied(&PropertiesDto {
            name: facts.name(),
            path: facts.path.display().to_string(),
            kind: match facts.kind {
                EntryKind::Parent => "parent",
                EntryKind::Dir => "dir",
                EntryKind::File => "file",
            },
            size: facts.size,
            modified: seconds(facts.modified),
            accessed: seconds(facts.accessed),
            created: seconds(facts.created),
            is_symlink: facts.is_symlink,
            link_target: facts.link_target.map(|p| p.display().to_string()),
            mode: facts.mode.map(|m| m.symbolic()),
            owner: facts.owner,
            group: facts.group,
            readonly: facts.readonly,
        })
    })
}

/// What typing one character over a byte comes to.
///
/// The half matters, which is why this crosses at all. A byte is two hex
/// digits, and an editor that replaced the whole byte on the first keystroke
/// would make `4f` unreachable except by typing `04` and then `4f`. So a
/// keystroke in the hex column replaces one nibble and says whether the
/// cursor should move on; in the text column one keystroke is the whole byte.
///
/// `low` is which half the cursor is on. `pane` is 0 for hex, 1 for text.
/// The reply is `{"byte": n, "advance": bool, "low": bool}` - the byte to
/// store, whether to step to the next one, and which half comes next. A
/// character that is not a hex digit (in the hex column) or not writable as a
/// single byte (in the text column) comes back as `{"none": true}`, which
/// means the keystroke was not for this editor.
#[no_mangle]
pub extern "C" fn rcmd_hex_type(current: u8, character: u32, low: u8, pane: u8) -> *mut c_char {
    guarded(|| {
        let Some(character) = char::from_u32(character) else {
            return out("{\"none\":true}".to_string());
        };
        if pane != 0 {
            // The text column: one keystroke is one byte, and only the bytes
            // the column can actually show - anything else would be typed as
            // a dot and store something the reader did not mean.
            let byte = match character {
                '\u{20}'..='\u{7e}' => character as u8,
                _ => return out("{\"none\":true}".to_string()),
            };
            return out(format!(
                "{{\"byte\":{byte},\"advance\":true,\"low\":false}}"
            ));
        }
        match lost_commander_core::hex::hex_digit(character) {
            Some(digit) => {
                let byte = lost_commander_core::hex::with_nibble(current, digit, low != 0);
                let advance = low != 0;
                let next_low = low == 0;
                out(format!(
                    "{{\"byte\":{byte},\"advance\":{advance},\"low\":{next_low}}}"
                ))
            }
            None => out("{\"none\":true}".to_string()),
        }
    })
}

#[derive(serde::Deserialize)]
struct HexEdit {
    at: u64,
    /// What the editor read at that offset. Checked against the disk before
    /// anything is written, so a file that changed underneath the editor is
    /// refused whole rather than half-overwritten.
    was: u8,
    now: u8,
}

/// Write edited bytes back where they came from.
///
/// Overwrites, never inserts or deletes: the file is exactly as long after as
/// before, which is the one invariant a hex editor must keep - a length
/// change in the middle of a binary shifts everything after it and breaks
/// every offset the format stored.
///
/// The whole set is verified before any of it is written. Each edit carries
/// what the editor read at that offset; if the disk now says otherwise - the
/// file changed underneath, or shrank so the offset is past the end - the
/// reply is an error naming the first disagreement and *nothing* is written.
/// Verify-then-write is not atomic against a third writer, but it turns the
/// likely accident into a refusal instead of a corruption.
///
/// The reply is `{"written": n}` - bytes actually changed.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings; `edits_json` a
/// JSON array of `{"at": offset, "was": byte, "now": byte}`.
#[no_mangle]
pub unsafe extern "C" fn rcmd_hex_write(
    path: *const c_char,
    edits_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let asked: Vec<HexEdit> = match borrowed(edits_json)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        {
            Ok(asked) => asked,
            Err(e) => return failed(format!("the edits could not be read: {e}")),
        };
        if asked.is_empty() {
            return out("{\"written\":0}".to_string());
        }

        // Verify every byte before writing any. One read per edit is fine:
        // a hex editor's session is dozens of bytes, not millions.
        use std::io::{Read, Seek, SeekFrom};
        let mut reading = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) => return failed(format!("{path}: {e}")),
        };
        let size = match reading.metadata() {
            Ok(m) => m.len(),
            Err(e) => return failed(format!("{path}: {e}")),
        };
        for edit in &asked {
            if edit.at >= size {
                return failed(format!(
                    "the file is {size} bytes now, and the edit at {:#x} is past its end -                      it has changed since it was read, and nothing was written",
                    edit.at
                ));
            }
            let mut byte = [0u8; 1];
            if let Err(e) = reading
                .seek(SeekFrom::Start(edit.at))
                .and_then(|_| reading.read_exact(&mut byte))
            {
                return failed(format!("{path}: {e}"));
            }
            if byte[0] != edit.was {
                return failed(format!(
                    "the byte at {:#x} is {:02x} on disk, not the {:02x} that was read -                      the file has changed underneath the editor, and nothing was written",
                    edit.at, byte[0], edit.was
                ));
            }
        }
        drop(reading);

        let mut edits = lost_commander_core::hex::Edits::default();
        for edit in &asked {
            edits.set(edit.at, edit.was, edit.now);
        }
        match lost_commander_core::hex::write_back(Path::new(&path), &edits) {
            Ok(written) => out(format!("{{\"written\":{written}}}")),
            Err(e) => failed(format!("{path}: {e}")),
        }
    })
}

/// What the desktop would run to open a path.
///
/// The command, not the running of it. Starting a process is the front-end's
/// - it is the one that knows whether it may, and on Windows it wants
/// ShellExecute rather than a bare spawn so that the file's own registered
/// handler applies and Mark-of-the-Web still meets SmartScreen.
///
/// `runs_code` says whether opening this would amount to executing it. On
/// Windows that is the platform's business and the answer is only worth
/// showing; on Unix there is no Mark-of-the-Web, a `.desktop` runs whatever
/// it names, and it is worth a question first.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_open_command(path: *const c_char) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let target = Path::new(&path);
        let platform = lost_commander_core::mount::Platform::current();
        match lost_commander_core::open::open_command(platform, target, &|p| p.exists()) {
            Ok(launch) => out(format!(
                "{{\"program\":{},\"args\":{},\"runs_code\":{}}}",
                serde_json::to_string(&launch.program).unwrap_or_default(),
                serde_json::to_string(&launch.args).unwrap_or_default(),
                lost_commander_core::open::runs_code(
                    platform,
                    target,
                    lost_commander_core::open::is_executable(target)
                )
            )),
            Err(e) => failed(e),
        }
    })
}

// ---- settings -------------------------------------------------------------

/// Where the settings file is, honouring the test override.
///
/// The same pattern as the journal: tests write settings, and a test that
/// polluted the reader's real preferences would be a test that lied to its
/// user twice - once in the run, and again the next time they started the
/// program.
fn settings_path() -> Option<PathBuf> {
    match std::env::var("RCMD_SETTINGS_PATH") {
        Ok(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => lost_commander_core::config::Settings::config_path(),
    }
}

#[derive(Serialize, serde::Deserialize, Default)]
struct SettingsDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pane_split: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shell_height: Option<f32>,
}

/// The preferences a front-end starts from.
///
/// A subset, not the whole file: the shell choice and the journal knobs are
/// read where they are used, and a front-end that cached them at startup
/// would go stale the moment the other one changed them.
#[no_mangle]
pub extern "C" fn rcmd_settings_read() -> *mut c_char {
    guarded(|| {
        let settings = settings_path()
            .and_then(|path| lost_commander_core::config::Settings::load_from(&path).ok())
            .unwrap_or_default();
        replied(&SettingsDto {
            theme: settings.theme,
            pane_split: settings.pane_split,
            shell_height: settings.shell_height,
        })
    })
}

/// Save the fields that are present, leaving the rest of the file alone.
///
/// Read-modify-write against the file rather than against anything cached:
/// the other front-end writes the same file, and a save built on a stale copy
/// would quietly undo whatever it had changed since.
///
/// # Safety
/// `json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_settings_save(json: *const c_char) -> *mut c_char {
    guarded(|| {
        let asked: SettingsDto = match borrowed(json)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        {
            Ok(asked) => asked,
            Err(e) => return failed(format!("those settings could not be read: {e}")),
        };
        let Some(path) = settings_path() else {
            return failed("nowhere to keep settings on this machine".to_string());
        };

        let mut settings =
            lost_commander_core::config::Settings::load_from(&path).unwrap_or_default();
        if let Some(theme) = asked.theme {
            // Empty means "back to the default", which a front-end needs a
            // way to say; None means "not changing it".
            settings.theme = match theme.is_empty() {
                true => None,
                false => Some(theme),
            };
        }
        if let Some(split) = asked.pane_split {
            settings.pane_split = Some(split.clamp(0.1, 0.9));
        }
        if let Some(height) = asked.shell_height {
            settings.shell_height = Some(height.max(0.0));
        }

        match settings.save_to(&path) {
            Ok(()) => out("{\"ok\":true}".to_string()),
            Err(e) => failed(format!("{}: {e}", path.display())),
        }
    })
}

/// Every named colour scheme, in the order a picker should offer them.
///
/// The scheme is shared and the drawing is not: what crosses is a handful of
/// colours by role, and each front-end maps those onto its own machinery. A
/// "Norton Commander" that was blue in one window and something else in the
/// other would be two programs wearing one name.
#[no_mangle]
pub extern "C" fn rcmd_themes() -> *mut c_char {
    guarded(|| replied(&lost_commander_core::themes::all()))
}

// ---- markdown -----------------------------------------------------------

#[derive(Serialize)]
struct MarkdownDoc {
    blocks: Vec<lost_commander_core::markdown::Block>,
    /// Whether the file was longer than `max_bytes` and was cut.
    ///
    /// Said rather than silently shown short, the same way reading text says
    /// it: a document that stops halfway through with no explanation looks
    /// like a document that ends there.
    truncated: bool,
    /// The file's size, so a front-end can say how much it is not showing.
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Parse a markdown file into blocks a front-end can draw.
///
/// What crosses is the parse and never the drawing - what counts as a heading
/// is CommonMark's business and the engine's; what a heading looks like is the
/// front-end's. See [`lost_commander_core::markdown`].
///
/// Nothing is fetched. An image that points at the network is marked `remote`
/// and it is the front-end's job to draw a placeholder rather than load it: a
/// preview that fetched one would tell whoever hosts it that somebody opened
/// the file, which is what a tracking pixel is.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_markdown_read(path: *const c_char, max_bytes: u64) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let size = std::fs::metadata(&path)
            .map(|facts| facts.len())
            .unwrap_or(0);
        let text = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                return replied(&MarkdownDoc {
                    blocks: Vec::new(),
                    truncated: false,
                    size,
                    error: Some(format!("{path}: {e}")),
                })
            }
        };

        // Cut on a character boundary, not a byte: a document sliced through
        // the middle of a multi-byte character would not be text at all.
        let cap = max_bytes.min(usize::MAX as u64) as usize;
        let truncated = text.len() > cap;
        let text = match truncated {
            false => text,
            true => {
                let mut at = cap;
                while at > 0 && (text[at] & 0b1100_0000) == 0b1000_0000 {
                    at -= 1;
                }
                text[..at].to_vec()
            }
        };

        replied(&MarkdownDoc {
            blocks: lost_commander_core::markdown::parse(&String::from_utf8_lossy(&text)),
            truncated,
            size,
            error: None,
        })
    })
}

/// Parse markdown that is already in hand, rather than a file.
///
/// What an editor needs: the text being typed has no file behind it yet, and
/// writing it to one to find out what it looks like would be an editor that
/// saved on every keystroke.
///
/// # Safety
/// `text` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_markdown_parse(text: *const c_char) -> *mut c_char {
    guarded(|| {
        let text = borrowed(text).unwrap_or_default();
        replied(&MarkdownDoc {
            blocks: lost_commander_core::markdown::parse(&text),
            truncated: false,
            size: text.len() as u64,
            error: None,
        })
    })
}

/// Whether a name is one the markdown viewer should render.
///
/// # Safety
/// `name` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_is_markdown(name: *const c_char) -> u8 {
    let looks = catch_unwind(AssertUnwindSafe(|| {
        lost_commander_core::markdown::looks_like_markdown(&borrowed(name).unwrap_or_default())
    }));
    u8::from(matches!(looks, Ok(true)))
}

// ---- the terminal -------------------------------------------------------

/// A running shell on a pty, and how much of it the front-end has seen.
pub struct RcmdTerm {
    session: lost_commander_core::pty::PtySession,
    /// Bumped whenever the screen could have changed.
    ///
    /// The whole reason a terminal can be polled at all without burning a
    /// core: an unchanged screen answers with this number and nothing else.
    seq: u64,
    /// What the screen looked like when `seq` was last bumped, so that bytes
    /// which change nothing visible - a bell, a title change - do not.
    last: String,
}

#[derive(Serialize)]
struct TermScreen {
    seq: u64,
    rows: u16,
    cols: u16,
    cursor_row: u16,
    cursor_col: u16,
    /// False while scrolled back, where a cursor would be drawn over history.
    cursor_visible: bool,
    /// How far back the view is, in lines. Zero at the prompt.
    scrollback: usize,
    /// The shell has exited; the front-end should offer to close the tab.
    finished: bool,
    /// Whether this shell can report what it runs. False for cmd.exe and
    /// PowerShell, which have no seam of the kind bash and zsh give.
    journals: bool,
    title: String,
    /// Where the shell says it is, which is not always where the pane looks.
    cwd: Option<String>,
    /// The file a recording is being written to, if one is running.
    recording: Option<String>,
    /// How many lines it has taken, so the panel can say it is working.
    recorded: u64,
    /// Whether the shell is in the middle of something.
    busy: bool,
    /// Absent when nothing has changed since the sequence asked about.
    #[serde(skip_serializing_if = "Option::is_none")]
    lines: Option<Vec<lost_commander_core::termview::Row>>,
}

/// Start a shell on a pty.
///
/// `program` empty means "whatever this platform's interactive shell is".
/// The reply is an opaque handle, or null if the shell could not be started -
/// which on Windows means ConPTY refused, and there is nothing useful to say
/// about it beyond that.
///
/// # Safety
/// `program` and `cwd` must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_open(
    program: *const c_char,
    cwd: *const c_char,
    rows: u16,
    cols: u16,
) -> *mut RcmdTerm {
    let started = catch_unwind(AssertUnwindSafe(|| {
        let program = borrowed(program).ok()?;
        let cwd = borrowed(cwd).ok()?;
        // current_shell hands back (path, name); the path is what to spawn.
        let program = match program.is_empty() {
            true => lost_commander_core::shell::current_shell().0,
            false => program,
        };
        lost_commander_core::pty::PtySession::spawn(
            &program,
            Path::new(&cwd),
            rows.max(1),
            cols.max(1),
        )
        .ok()
    }));
    match started {
        Ok(Some(session)) => Box::into_raw(Box::new(RcmdTerm {
            session,
            seq: 1,
            last: String::new(),
        })),
        _ => std::ptr::null_mut(),
    }
}

/// What the screen looks like, if it has changed since `since`.
///
/// Poll as often as you like: when nothing has changed the reply is the
/// sequence number and the handful of facts around it, with no grid at all.
/// An idle shell costs a string compare.
///
/// # Safety
/// `term` must be a handle from [`rcmd_term_open`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_poll(term: *mut RcmdTerm, since: u64) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };

        // Compared against the last screen rather than against the bytes that
        // arrived: a shell repainting an identical prompt, or a title change,
        // is not a reason to repaint.
        let (text, rows, cols, cursor, lines) = term.session.with_screen(|screen| {
            let (rows, cols) = screen.size();
            (
                screen.contents_formatted(),
                rows,
                cols,
                screen.cursor_position(),
                lost_commander_core::termview::rows_of(screen),
            )
        });
        let text = String::from_utf8_lossy(&text).to_string();
        if text != term.last {
            term.last = text;
            term.seq += 1;
        }

        let scrollback = term.session.scrollback_offset();
        replied(&TermScreen {
            seq: term.seq,
            rows,
            cols,
            cursor_row: cursor.0,
            cursor_col: cursor.1,
            cursor_visible: scrollback == 0,
            scrollback,
            finished: term.session.finished(),
            journals: term.session.journals(),
            title: term.session.title.clone(),
            cwd: term
                .session
                .shell_cwd()
                .map(|path| path.display().to_string()),
            recording: term
                .session
                .recording()
                .map(|path| path.display().to_string()),
            recorded: term.session.recorded_lines(),
            busy: term.session.is_busy(),
            lines: match term.seq == since {
                true => None,
                false => Some(lines),
            },
        })
    })
}

#[derive(Serialize)]
struct TermRan {
    line: String,
    cwd: Option<String>,
    code: i32,
    ms: u64,
}

/// Commands this shell has finished since the last time this was asked.
///
/// Collected rather than pushed: the front-end polls for the screen anyway,
/// and a second thing to poll costs nothing. Empty for a shell with no seam
/// to hook, which is what `journals` in the screen reply is warning about.
///
/// # Safety
/// `term` must be a handle from [`rcmd_term_open`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_commands(term: *mut RcmdTerm) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        let ran: Vec<TermRan> = term
            .session
            .take_commands()
            .into_iter()
            .map(|one| TermRan {
                line: one.line,
                cwd: one.cwd.map(|path| path.display().to_string()),
                code: one.code,
                ms: one.ms,
            })
            .collect();
        replied(&ran)
    })
}

/// Start writing everything the shell prints to a file.
///
/// A recording is the terminal's own transcript, kept apart from the account:
/// the account says *what ran*, and a recording says *what it looked like*.
/// One is a list and the other is a file, and conflating them would make the
/// account unreadable the first time anyone built something noisy.
///
/// # Safety
/// `term` must be a live handle; `path` a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_record(term: *mut RcmdTerm, path: *const c_char) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        let path = borrowed(path).unwrap_or_default();
        match term.session.start_recording(Path::new(&path)) {
            Ok(()) => out(format!("{{\"recording\":{}}}", json_string(&path))),
            Err(e) => failed(format!("{path}: {e}")),
        }
    })
}

/// Stop the recording, and say where it went and how much it caught.
///
/// # Safety
/// `term` must be a handle from [`rcmd_term_open`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_stop_record(term: *mut RcmdTerm) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        match term.session.stop_recording() {
            Some((path, lines)) => out(format!(
                "{{\"path\":{},\"lines\":{lines}}}",
                json_string(&path.display().to_string())
            )),
            None => out("{\"path\":null,\"lines\":0}".to_string()),
        }
    })
}

/// A file name for a recording, from a title and a timestamp.
///
/// The engine's rule, because it is one: a name has to survive being a
/// filename on every platform, and each front-end inventing its own would
/// give two sets of transcripts that sort differently in the same folder.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_transcript_name(
    title: *const c_char,
    stamp: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let title = borrowed(title).unwrap_or_default();
        let stamp = borrowed(stamp).unwrap_or_default();
        out(format!(
            "{{\"name\":{}}}",
            json_string(&lost_commander_core::pty::transcript_name(&title, &stamp))
        ))
    })
}

/// Names, quoted the way this platform's shell needs them.
///
/// `names_json` is a JSON array; the reply is `{"line":"..."}` - the names
/// joined with spaces, ready to be typed at a prompt. The rule is the
/// engine's because it is the shell's: which characters need quoting, and
/// whether a quote is doubled or backslashed, is a property of cmd.exe or of
/// sh and not of the window asking.
///
/// # Safety
/// `names_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_shell_quote(names_json: *const c_char) -> *mut c_char {
    guarded(|| {
        let names: Vec<String> = borrowed(names_json)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let line = names
            .iter()
            .map(|name| lost_commander_core::shell::quote_here(name))
            .collect::<Vec<_>>()
            .join(" ");
        out(format!("{{\"line\":{}}}", json_string(&line)))
    })
}

/// Send text the reader typed.
///
/// # Safety
/// `term` must be a live handle; `text` a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_write(term: *mut RcmdTerm, text: *const c_char) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        let text = borrowed(text).unwrap_or_default();
        term.session.write_str(&text);
        out("{\"ok\":true}".to_string())
    })
}

/// Send a key that is not a character - an arrow, Home, Ctrl-C.
///
/// The key is named rather than numbered: `"Up"`, `"Home"`, `"Enter"`, or a
/// single letter for the control chords. Which of a toolkit's key constants
/// means Up is the front-end's business; what bytes an Up arrow sends down a
/// pty is the terminal's, and belongs in one place.
///
/// The reply says whether the key was one this understands, so a front-end
/// can let anything else fall through to the file manager.
///
/// # Safety
/// `term` must be a live handle; `name` a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_key(
    term: *mut RcmdTerm,
    name: *const c_char,
    ctrl: u8,
    alt: u8,
) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        let name = borrowed(name).unwrap_or_default();

        // The table is the engine's: the graphical crate needs the same
        // answers and the terminal one needs them too, and three copies of
        // "what does Left send" is three chances to disagree.
        let Some(bytes) = lost_commander_core::termview::key_bytes(&name, ctrl != 0, alt != 0)
        else {
            return out("{\"sent\":false}".to_string());
        };
        term.session.write(&bytes);
        out("{\"sent\":true}".to_string())
    })
}

/// Write a command a shell finished into the account.
///
/// The commands a hooked shell reports belong beside the copies and the
/// deletes, because "what did this program do" is one question and answering
/// it in two places is answering it in neither.
///
/// Kept out of `rcmd_term_commands` on purpose. That entry point *reads* what
/// the shell reported; this one *records* it, and a front-end that shows the
/// commands somewhere without wanting them filed - a busy indicator, a status
/// line - should not have to write to disk to do it.
///
/// A `note` of `exit 1` and the rest is the engine's own wording, so the
/// account reads the same whichever front-end filled it.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings; `ran_json` a
/// JSON object of the shape `rcmd_term_commands` hands back.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_journal(
    term: *mut RcmdTerm,
    ran_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        #[derive(serde::Deserialize)]
        struct RanDto {
            line: String,
            cwd: Option<String>,
            code: i32,
            ms: u64,
        }
        let ran: RanDto = match borrowed(ran_json)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        {
            Ok(ran) => ran,
            Err(e) => return failed(format!("that command could not be read: {e}")),
        };

        let Some(book) = account() else {
            // Nowhere to write is not an error worth stopping a shell over.
            return out("{\"ok\":false}".to_string());
        };
        // The shape every other writer uses, copied rather than paraphrased:
        // the *path* is the directory the command ran in and the *note* is
        // the line itself. This entry point had them the other way round -
        // path carried the command, note carried "in <dir>" - which read
        // plausibly in the raw file and wrongly everywhere else: the history
        // column shows the note, so it showed locations; its here-filter
        // compares the path, so it compared against command text and matched
        // nothing. One swapped pair, two symptoms, and the engine's own
        // readers (`commands_before`, `directory_of`, the TUI) were the
        // specification all along.
        let cwd = ran
            .cwd
            .unwrap_or_else(|| term.session.cwd.display().to_string());
        book.record(
            lost_commander_core::journal::Event::new(
                lost_commander_core::journal::Kind::Command,
                &cwd,
            )
            .note(ran.line)
            .by(lost_commander_core::shell::program_name(
                &term.session.program,
            ))
            .lasting(ran.ms)
            // A non-zero exit marks the record failed with the code as the
            // reason, exactly as the egui front-end files its own - one
            // account, one shape, whoever wrote the entry.
            .failed_if(ran.code != 0, format!("exit {}", ran.code)),
        );
        out("{\"ok\":true}".to_string())
    })
}

/// Move the view through the scrollback. Positive goes back into history.
///
/// # Safety
/// `term` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_scroll(
    term: *mut RcmdTerm,
    lines: i64,
    to_bottom: u8,
) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        if to_bottom != 0 {
            term.session.scroll_to_bottom();
        } else {
            term.session.scroll_by(lines);
        }
        // The view moved, so the screen has changed even though no byte came
        // in. Without this the poll would find identical contents and the
        // scroll would not paint.
        term.seq += 1;
        term.last.clear();
        out("{\"ok\":true}".to_string())
    })
}

/// Tell the shell the window changed shape.
///
/// # Safety
/// `term` must be a live handle.
/// Wipe the screen and the scrollback, and ask for a fresh prompt.
///
/// The wipe is fed to the emulator; the bare Enter afterwards brings the
/// prompt back, and the hook records it as an empty command, which every
/// history view filters out.
///
/// # Safety
/// `term` must be a live handle from `rcmd_term_open`.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_clean(term: *mut RcmdTerm) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        term.session.clean_screen();
        out("{\"ok\":true}".to_string())
    })
}

#[no_mangle]
pub unsafe extern "C" fn rcmd_term_resize(
    term: *mut RcmdTerm,
    rows: u16,
    cols: u16,
) -> *mut c_char {
    guarded(|| {
        let Some(term) = (unsafe { term.as_mut() }) else {
            return failed("that terminal is gone".to_string());
        };
        term.session.resize(rows.max(1), cols.max(1));
        term.seq += 1;
        term.last.clear();
        out("{\"ok\":true}".to_string())
    })
}

/// Close the shell and free the handle.
///
/// # Safety
/// `term` must be a handle from [`rcmd_term_open`], freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_term_free(term: *mut RcmdTerm) {
    if term.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let mut term = unsafe { Box::from_raw(term) };
        term.session.shutdown();
    }));
}

#[derive(Serialize)]
struct RootShell {
    /// `"command"`, `"shell"` or `"refused"`.
    kind: &'static str,
    /// What to spawn, for `"command"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    /// The line to type at a shell tab, for `"shell"` - `sudo` needs a
    /// terminal for its password prompt to appear on.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    /// Why not, for `"refused"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<String>,
}

/// How to get an administrator shell in `cwd` on this platform.
///
/// Three answers, and which one you get is the platform's business. On
/// Windows it is a *command*: an elevated process cannot inherit this one's
/// pty, so it gets a console of its own and UAC puts up its own prompt -
/// which is why an admin shell is a new window and never a tab in the drawer.
/// Elsewhere it is a *line to type*, because `sudo` with nowhere to ask is
/// `sudo` that fails. And where the platform does not do this at all, the
/// reason is worth saying rather than the button quietly doing nothing.
///
/// Nothing is launched here. This says what would be done; doing it is the
/// front-end's, after it has told the reader what is about to happen.
///
/// # Safety
/// `cwd` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_root_shell(cwd: *const c_char) -> *mut c_char {
    guarded(|| {
        let cwd = borrowed(cwd).unwrap_or_default();
        let asked = lost_commander_core::elevate::root_shell(
            lost_commander_core::mount::Platform::current(),
            Path::new(&cwd),
        );
        replied(&match asked {
            lost_commander_core::elevate::Elevation::Command(launch) => RootShell {
                kind: "command",
                program: Some(launch.program),
                args: launch.args,
                line: None,
                why: None,
            },
            lost_commander_core::elevate::Elevation::Shell(line) => RootShell {
                kind: "shell",
                program: None,
                args: Vec::new(),
                line: Some(line),
                why: None,
            },
            lost_commander_core::elevate::Elevation::Refused(why) => RootShell {
                kind: "refused",
                program: None,
                args: Vec::new(),
                line: None,
                why: Some(why),
            },
        })
    })
}

// ---- pictures -----------------------------------------------------------

#[derive(Serialize)]
struct ImagePlan {
    /// How big the result comes out.
    width: u32,
    height: u32,
    /// Why this cannot be written to that format at all, or null.
    ///
    /// Asked before the work rather than after it: an editor that lets you
    /// crop a screenshot for five minutes and *then* says the format will not
    /// take it has wasted five minutes and taught you nothing.
    refuses: Option<String>,
    /// What writing it back would quietly leave behind. Not errors - the file
    /// is written and the pixels are right - but the kind of loss you only
    /// notice long afterwards, so it is said while Save as is still an option.
    losses: Vec<String>,
    /// Whether the format itself throws away detail every time it is written.
    lossy: bool,
}

/// What turning, cropping and resizing this picture would come to.
///
/// The pixels are the front-end's business - Windows has decoders for more
/// formats than any crate here would bring, and they are already installed.
/// What crosses is what is *not* obvious: how quarter-turns compose, how big
/// the result is, and what a given format will refuse or silently discard.
///
/// `quarters` is clockwise quarter-turns. `carries` says what the file holds
/// beyond its pixels: whether it is animated, and whether it has metadata.
///
/// # Safety
/// `extension` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_image_plan(
    width: u32,
    height: u32,
    quarters: u8,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    resize_width: u32,
    resize_height: u32,
    extension: *const c_char,
    animated: u8,
    metadata: u8,
) -> *mut c_char {
    guarded(|| {
        // Trimmed here rather than in each front-end: every platform's "what
        // is this file's extension" hands back a leading dot, and the engine
        // matches on the bare word. Getting that wrong is silent - the format
        // simply stops being recognised as lossy.
        let extension = borrowed(extension).unwrap_or_default();
        let extension = extension.trim_start_matches('.');
        let edit = lost_commander_core::imageops::Edit {
            // A zero-sized crop means "no crop", the same convention as the
            // resize: a caller with nothing to cut does not invent a rectangle.
            crop: match (crop_width, crop_height) {
                (0, _) | (_, 0) => None,
                _ => Some(lost_commander_core::imageops::Crop {
                    x: crop_x,
                    y: crop_y,
                    width: crop_width,
                    height: crop_height,
                }),
            },
            transform: lost_commander_core::imageops::Transform {
                turn: lost_commander_core::imageops::Turn::from_quarters(quarters % 4),
                ..Default::default()
            },
            // Zero means "no resize", so that a caller with nothing to say
            // about size does not have to invent one.
            resize: match (resize_width, resize_height) {
                (0, _) | (_, 0) => None,
                size => Some(size),
            },
        };

        let (out_width, out_height) = edit.size_of((width.max(1), height.max(1)));
        replied(&ImagePlan {
            width: out_width,
            height: out_height,
            refuses: lost_commander_core::imageops::refuses(extension, (out_width, out_height)),
            losses: lost_commander_core::imageops::losses(
                extension,
                lost_commander_core::imageops::Carries {
                    animated: animated != 0,
                    metadata: metadata != 0,
                },
            ),
            lossy: lost_commander_core::imageops::is_lossy(extension),
        })
    })
}

/// The crop a drag over the *displayed* picture asks of the *source*.
///
/// The rectangle was dragged over a picture that may already be cropped,
/// mirrored, turned and resized, while the edit stores its crop against the
/// untouched source - so the drag has to be unprojected from the screen, the
/// turn and the mirrors undone, and the earlier crop's corner added back on.
/// All of that lives in the engine (`crop_from_drag`, `fold_crop`) because it
/// is the same arithmetic whichever front-end is dragging, and the inverse of
/// a turn is exactly the kind of thing two implementations get differently.
///
/// `drawn_*` is where the picture sits on screen, in whatever units the
/// screen uses. The linear mapping absorbs the resize, so the caller does not
/// say what the resize was. `base_*` is the crop already in effect - the
/// whole picture when there is none. The reply is the crop in source pixels,
/// or `{"none":true}` for a drag that left nothing (one stray pixel, or a
/// drag entirely off the picture) - which the caller treats as "no change",
/// so a slipped click cannot crop a picture away to nothing.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn rcmd_image_pick_crop(
    from_x: f32,
    from_y: f32,
    to_x: f32,
    to_y: f32,
    drawn_x: f32,
    drawn_y: f32,
    drawn_width: f32,
    drawn_height: f32,
    base_x: u32,
    base_y: u32,
    base_width: u32,
    base_height: u32,
    quarters: u8,
    flip_h: u8,
    flip_v: u8,
    source_width: u32,
    source_height: u32,
) -> *mut c_char {
    guarded(|| {
        let source = (source_width.max(1), source_height.max(1));
        let base = match (base_width, base_height) {
            (0, _) | (_, 0) => lost_commander_core::imageops::Crop::whole(source),
            _ => lost_commander_core::imageops::Crop {
                x: base_x,
                y: base_y,
                width: base_width,
                height: base_height,
            },
        };
        let transform = lost_commander_core::imageops::Transform {
            turn: lost_commander_core::imageops::Turn::from_quarters(quarters % 4),
            flip_h: flip_h != 0,
            flip_v: flip_v != 0,
        };

        // What the screen is showing: the base crop, turned. The drag is
        // unprojected against that, then folded through the transform back
        // to the source.
        let shown = transform.size_of((base.width, base.height));
        let drawn = lost_commander_core::imageops::Drawn {
            x: drawn_x,
            y: drawn_y,
            width: drawn_width,
            height: drawn_height,
        };
        let dragged = lost_commander_core::imageops::crop_from_drag(
            (from_x, from_y),
            (to_x, to_y),
            drawn,
            shown,
        );
        let folded = dragged.and_then(|dragged| {
            lost_commander_core::imageops::fold_crop(dragged, base, transform, source)
        });
        match folded {
            Some(crop) => out(format!(
                "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
                crop.x, crop.y, crop.width, crop.height
            )),
            None => out("{\"none\":true}".to_string()),
        }
    })
}

/// The other side of a resize, keeping the shape.
///
/// `changed_width` says which box was typed in; the other follows. Crossing it
/// rather than dividing in the front-end keeps the rounding identical between
/// the two, so a picture resized here and there comes out the same size.
#[no_mangle]
pub extern "C" fn rcmd_image_fit(
    width: u32,
    height: u32,
    want_width: u32,
    want_height: u32,
    changed_width: u8,
) -> *mut c_char {
    guarded(|| {
        let (w, h) = lost_commander_core::imageops::keep_aspect(
            (width.max(1), height.max(1)),
            (want_width, want_height),
            changed_width != 0,
        );
        out(format!("{{\"width\":{w},\"height\":{h}}}"))
    })
}

// ---- files that are not text --------------------------------------------

/// One row of a hex dump, already laid out.
#[derive(Serialize)]
struct HexRow {
    /// `000000a0`. Eight digits, which is the conventional width and covers
    /// four gigabytes; a larger file gets what it needs rather than a column
    /// that silently stops lining up.
    offset: String,
    /// Sixteen bytes, in two groups of eight, padded so a short last row keeps
    /// the text column where the rows above put it.
    hex: String,
    /// The same bytes as characters, with the unprintable ones as dots.
    text: String,
}

#[derive(Serialize)]
struct HexPage {
    rows: Vec<HexRow>,
    /// How many rows the whole file comes to, so a scrollbar can be drawn
    /// without reading it.
    total_rows: u64,
    size: u64,
}

/// Whether a file should be read as bytes rather than as text.
///
/// Its head only: a file is what its first few thousand bytes say it is, and
/// reading a gigabyte to find out is not an answer anyone waits for.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_is_binary(path: *const c_char) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        match lost_commander_core::hex::is_binary(Path::new(&path)) {
            Ok(binary) => out(format!("{{\"binary\":{binary}}}")),
            Err(e) => failed(e),
        }
    })
}

/// `count` rows of a hex dump, starting at row `from`.
///
/// A window, not a file. Row `n` starts at byte `n * 16`, so only what is on
/// screen is ever read and a four-gigabyte file costs exactly what a small one
/// does - which is the whole reason this is asked for a page at a time rather
/// than handed over whole.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_hex_read(
    path: *const c_char,
    from: u64,
    count: usize,
) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let dump = match lost_commander_core::hex::Dump::open(Path::new(&path)) {
            Ok(dump) => dump,
            Err(e) => return failed(e),
        };
        // Capped, so a front-end asking for a million rows gets a page rather
        // than a gigabyte of JSON and a long silence.
        let count = count.min(4_096);
        let rows = match dump.read(from, count) {
            Ok(rows) => rows,
            Err(e) => return failed(e),
        };
        let width = dump.offset_width();
        replied(&HexPage {
            rows: rows
                .iter()
                .map(|row| HexRow {
                    offset: format!("{:0width$x}", row.offset, width = width),
                    hex: row.hex(),
                    text: row.text(),
                })
                .collect(),
            total_rows: dump.rows(),
            size: dump.size,
        })
    })
}

// ---- the account of what was done ---------------------------------------

/// One line of the journal, flattened for a list.
#[derive(Serialize)]
struct JournalLine {
    /// True for a run's heading; the lines under it are its files.
    heading: bool,
    /// How far to indent: the files under a run sit beneath it.
    depth: usize,
    /// Local wall-clock time, which is what someone remembers by.
    clock: String,
    kind: &'static str,
    /// The summary for a run, or what was acted on for one file.
    text: String,
    /// Where it ended up, for the operations that move something.
    to: Option<String>,
    /// The detail that makes the line worth reading: `644 -> 755`, `exit 1`.
    note: String,
    /// Why it did not happen. Null means it did.
    failed: Option<String>,
    /// Which shell ran it, where that is known.
    shell: Option<String>,
    /// How long, where that is a thing worth saying - a run has a total and a
    /// command has one; a single file copy does not.
    took_ms: Option<u64>,
    /// Headings only: how many files, and how many of them failed.
    items: usize,
    failures: usize,
    /// Headings only. False for a run that never reached its end - killed, or
    /// still going - which is a difference worth being able to see, and the
    /// reason the closing record is separate from the heading.
    finished: bool,
}

#[derive(Serialize)]
struct JournalDay {
    /// `2026-07-30`, and what to hand back to read it.
    name: String,
    year: i32,
    month: u32,
    day: u32,
}

#[derive(Serialize)]
struct JournalPage {
    lines: Vec<JournalLine>,
    rows: usize,
    items: usize,
    failures: usize,
}

/// Where the account is kept.
///
/// The engine's own directory, unless `RCMD_JOURNAL_DIR` says otherwise. That
/// exists for two reasons: a portable install that keeps its records beside
/// itself, and the tests below - which start real jobs through the real entry
/// point, and would otherwise write what they did into the account of whoever
/// ran them.
/// One journal handle per directory, shared by every entry point.
///
/// The engine's journal holds the account in memory and counts a generation
/// per change - but both live on the *instance*, shared only by clones. A
/// journal built per call would re-read the files per question and report a
/// generation that never moved. Everything in this ABI that touches the
/// account goes through here, so a job's records are in the memory the next
/// `rcmd_history` reads, and `rcmd_journal_generation` moves when they land.
/// The held handle is rebuilt when `RCMD_JOURNAL_DIR` changes, which is what
/// the tests do to stay out of the real account.
fn account() -> Option<journal::Journal> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<Option<(PathBuf, journal::Journal)>>> =
        std::sync::OnceLock::new();
    let dir = journal_dir()?;
    let cell = HELD.get_or_init(|| std::sync::Mutex::new(None));
    let mut slot = cell.lock().ok()?;
    if let Some((held, book)) = slot.as_ref() {
        if *held == dir {
            return Some(book.clone());
        }
    }
    let book = journal::Journal::at(dir.clone(), journal::Keep::default());
    *slot = Some((dir, book.clone()));
    Some(book)
}

fn journal_dir() -> Option<PathBuf> {
    match std::env::var_os("RCMD_JOURNAL_DIR") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        // In a test build the real directory is not a fallback, it is a bug.
        // An environment variable is process-wide and the test harness runs
        // tests in parallel threads of one process, so the moment any test
        // cleared this one - as one legitimately does, to check the default -
        // every job running in another test at that instant wrote its
        // tempdir paths into the account of whoever ran the suite. That is
        // exactly the thing this override exists to prevent, and it was
        // happening. Nothing is written rather than something written
        // somewhere it should never appear.
        #[cfg(test)]
        _ => None,
        #[cfg(not(test))]
        _ => journal::Journal::default_dir(),
    }
}

fn shown_named(name: &str) -> journal::Shown {
    match name {
        "files" => journal::Shown::Files,
        "commands" => journal::Shown::Commands,
        _ => journal::Shown::All,
    }
}

/// Which days the account has anything for, newest first.
///
/// `shown` is `"all"`, `"files"` or `"commands"`. Never an error: no journal
/// at all is an empty list, which is a true answer.
///
/// # Safety
/// `shown` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_journal_days(shown: *const c_char) -> *mut c_char {
    guarded(|| {
        let shown = shown_named(&borrowed(shown).unwrap_or_default());
        let Some(book) = account() else {
            return replied(&Vec::<JournalDay>::new());
        };
        replied(
            &book
                .days_shown(shown)
                .into_iter()
                .map(|day| JournalDay {
                    name: format!("{:04}-{:02}-{:02}", day.year, day.month, day.day),
                    year: day.year,
                    month: day.month,
                    day: day.day,
                })
                .collect::<Vec<_>>(),
        )
    })
}

/// One day of the account, filtered, flattened into lines.
///
/// `filter` is `{"kinds":["copy",...],"failures_only":false,"text":"..."}`.
/// An empty `kinds` means every kind, which is what an untouched filter row
/// means.
///
/// # Safety
/// All three arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_journal_read(
    shown: *const c_char,
    day: *const c_char,
    filter_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let shown = shown_named(&borrowed(shown).unwrap_or_default());
        let day = borrowed(day).unwrap_or_default();
        let day = match parse_day(&day) {
            Some(day) => day,
            None => return failed(format!("{day} is not a day")),
        };

        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct FilterDto {
            kinds: Vec<String>,
            failures_only: bool,
            text: String,
        }
        let wanted: FilterDto = borrowed(filter_json)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let filter = journal::Filter {
            kinds: wanted
                .kinds
                .iter()
                .filter_map(|name| {
                    journal::KINDS
                        .iter()
                        .copied()
                        .find(|kind| kind.label().eq_ignore_ascii_case(name))
                })
                .collect(),
            failures_only: wanted.failures_only,
            text: wanted.text,
        };

        let Some(dir) = journal_dir() else {
            return replied(&JournalPage {
                lines: Vec::new(),
                rows: 0,
                items: 0,
                failures: 0,
            });
        };
        let book = account().unwrap_or_else(|| journal::Journal::at(dir, journal::Keep::default()));
        let rows = filter.apply(journal::arrange(book.read_shown(shown, day)));
        let tally = journal::tally(&rows);

        let mut lines = Vec::new();
        for line in journal::lines(&rows) {
            let row = &rows[line.row()];
            if line.is_heading() {
                let journal::Row::Run { group, took, .. } = row else {
                    continue;
                };
                lines.push(JournalLine {
                    heading: true,
                    depth: 0,
                    clock: journal::clock(group.at),
                    kind: group.kind.label(),
                    text: group.summary.clone(),
                    to: None,
                    note: String::new(),
                    failed: None,
                    shell: None,
                    took_ms: *took,
                    items: row.items(),
                    failures: row.failures(),
                    finished: took.is_some(),
                });
                continue;
            }
            let Some(event) = journal::event_at(&rows, &line) else {
                continue;
            };
            lines.push(JournalLine {
                heading: false,
                // Under a run, or standing on its own.
                depth: usize::from(!matches!(line, journal::Line::Alone { .. })),
                clock: journal::clock(event.at),
                kind: event.kind.label(),
                text: event.path.clone(),
                to: event.to.clone(),
                note: event.note.clone(),
                failed: event.failed.clone(),
                shell: event.shell.clone(),
                took_ms: event.ms,
                items: 1,
                failures: usize::from(event.is_failure()),
                finished: true,
            });
        }

        replied(&JournalPage {
            lines,
            rows: tally.rows,
            items: tally.items,
            failures: tally.failures,
        })
    })
}

/// `2026-07-30` into a day.
fn parse_day(text: &str) -> Option<journal::Day> {
    let mut parts = text.split('-');
    Some(journal::Day {
        year: parts.next()?.parse().ok()?,
        month: parts.next()?.parse().ok()?,
        day: parts.next()?.parse().ok()?,
    })
}

// ---- renaming a lot of files at once ------------------------------------

#[derive(Deserialize)]
struct RulesDto {
    /// Absent means the engine's default, which keeps the name. An empty
    /// string is not the same thing and is not treated as one: the engine
    /// takes an empty extension to mean "leave the file without one", so
    /// folding the two together would make "remove the extension"
    /// impossible to ask for.
    name: Option<String>,
    extension: Option<String>,
    #[serde(default)]
    find: String,
    #[serde(default)]
    replace: String,
    #[serde(default)]
    case_sensitive: bool,
    /// `"keep"`, `"lower"`, `"upper"`, `"title"` or `"first"`.
    #[serde(default)]
    case: String,
}

#[derive(Serialize)]
struct ChangeDto {
    was: String,
    name: String,
    from: String,
    to: String,
    /// Why this one cannot be used, or null. The words are the engine's.
    trouble: Option<&'static str>,
    /// Whether this one would actually move anything.
    moving: bool,
}

#[derive(Serialize)]
struct PlanDto {
    changes: Vec<ChangeDto>,
    /// How many would move, and how many are in trouble.
    moving: usize,
    troubled: usize,
}

#[derive(Serialize)]
struct AppliedDto {
    renamed: usize,
    failures: Vec<FailureDto>,
}

#[derive(Serialize)]
struct FailureDto {
    name: String,
    message: String,
}

fn rules_from(dto: RulesDto) -> lost_commander_core::rename::Rules {
    use lost_commander_core::rename::Case;
    lost_commander_core::rename::Rules {
        name: dto
            .name
            .unwrap_or_else(|| lost_commander_core::rename::KEEP_NAME.to_string()),
        extension: dto
            .extension
            .unwrap_or_else(|| lost_commander_core::rename::KEEP_EXTENSION.to_string()),
        find: dto.find,
        replace: dto.replace,
        case_sensitive: dto.case_sensitive,
        case: match dto.case.as_str() {
            "lower" => Case::Lower,
            "upper" => Case::Upper,
            "title" => Case::Title,
            "first" => Case::First,
            _ => Case::Keep,
        },
    }
}

/// The files a rename is about, read from what the front-end selected.
fn sources_from(raw: &str) -> Result<Vec<lost_commander_core::rename::Source>, serde_json::Error> {
    #[derive(Deserialize)]
    struct SourceDto {
        path: String,
        name: String,
        /// Seconds since the epoch; the date placeholders come from it.
        modified: Option<i64>,
    }
    let sources: Vec<SourceDto> = serde_json::from_str(raw)?;
    Ok(sources
        .into_iter()
        .map(|source| lost_commander_core::rename::Source {
            path: PathBuf::from(source.path),
            name: source.name,
            modified: source
                .modified
                .map(|seconds| UNIX_EPOCH + std::time::Duration::from_secs(seconds.max(0) as u64)),
        })
        .collect())
}

/// What the rules would do, without doing any of it.
///
/// Returns every line of the preview, including the ones that cannot be used
/// and why. Names are checked against *this* platform's rules, so a template
/// that would make `CON.txt` or a name ending in a space is refused here rather
/// than accepted and quietly turned into something else by Windows.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_rename_plan(
    sources_json: *const c_char,
    rules_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let (sources, rules) = match (borrowed(sources_json), borrowed(rules_json)) {
            (Ok(sources), Ok(rules)) => (sources, rules),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let sources = match sources_from(&sources) {
            Ok(sources) => sources,
            Err(e) => return failed(e),
        };
        let rules: RulesDto = match serde_json::from_str(&rules) {
            Ok(rules) => rules,
            Err(e) => return failed(e),
        };

        let changes = lost_commander_core::rename::plan(
            lost_commander_core::mount::Platform::current(),
            &sources,
            &rules_from(rules),
            &lost_commander_core::preview::on_disk,
        );
        let (moving, troubled) = lost_commander_core::rename::tally(&changes);
        replied(&PlanDto {
            changes: changes
                .iter()
                .map(|change| ChangeDto {
                    was: change.was.clone(),
                    name: change.name.clone(),
                    from: change.from.display().to_string(),
                    to: change.to.display().to_string(),
                    trouble: change.trouble.map(|t| t.message()),
                    moving: change.is_rename(),
                })
                .collect(),
            moving,
            troubled,
        })
    })
}

/// Carry out what [`rcmd_rename_plan`] previewed.
///
/// Takes the sources and rules again rather than a plan handed back, and works
/// the plan out afresh. Two reasons: the front-end cannot hand back an edited
/// plan and have it obeyed, and the check for names already on disk is made
/// against the disk as it is now rather than as it was when the preview was
/// drawn.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_rename_apply(
    sources_json: *const c_char,
    rules_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let (sources, rules) = match (borrowed(sources_json), borrowed(rules_json)) {
            (Ok(sources), Ok(rules)) => (sources, rules),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let sources = match sources_from(&sources) {
            Ok(sources) => sources,
            Err(e) => return failed(e),
        };
        let rules: RulesDto = match serde_json::from_str(&rules) {
            Ok(rules) => rules,
            Err(e) => return failed(e),
        };

        let changes = lost_commander_core::rename::plan(
            lost_commander_core::mount::Platform::current(),
            &sources,
            &rules_from(rules),
            &lost_commander_core::preview::on_disk,
        );
        let applied = lost_commander_core::rename::apply(&changes);
        replied(&AppliedDto {
            renamed: applied.renamed,
            failures: applied
                .failures
                .iter()
                .map(|failure| FailureDto {
                    name: failure.name.clone(),
                    message: failure.message.clone(),
                })
                .collect(),
        })
    })
}

// ---- comparing two files --------------------------------------------------

/// One line of the two-column view, flattened for the front-end.
///
/// Both sides on one row, with a null where a side has nothing, so the view is
/// a list of rows rather than two lists to keep in step - which is the whole
/// difficulty of drawing a diff.
#[derive(Serialize)]
struct DiffRow {
    /// `"same"`, `"left"` or `"right"`.
    kind: &'static str,
    left_number: Option<usize>,
    left_text: Option<String>,
    right_number: Option<usize>,
    right_text: Option<String>,
}

#[derive(Serialize)]
struct DiffDto {
    rows: Vec<DiffRow>,
    /// How many rows are not the same on both sides.
    changes: usize,
    /// The alignment gave up; the two are shown as they are.
    unaligned: bool,
    identical: bool,
}

/// What one pane offers a comparison.
#[derive(Deserialize)]
struct SideOffer {
    #[serde(default)]
    marked: Vec<String>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Serialize)]
struct ChosenDto {
    left: String,
    right: String,
    /// True when both came from one pane rather than one from each.
    from_one_pane: bool,
}

/// Which two files a "compare these" is about.
///
/// Each side goes in as `{"marked":[...],"cursor":"..."}`. Returns
/// `{"left":...,"right":...}` or `{"error":...}` saying what to do instead.
///
/// The decision crosses rather than being made in the front-end, and this one
/// is worth the trip. Marking two files beats the cursors, because marking is
/// deliberate and a cursor is only ever where it was left. More than two marks
/// is refused rather than quietly falling back to the cursors, which would
/// compare two files nobody pointed at. And the pair comes back in the order
/// the panes are on screen - the left pane's file on the left - because taking
/// the order from whichever pane has the keyboard makes the two columns swap
/// places depending on where you last clicked.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_diff_choose(
    left_json: *const c_char,
    right_json: *const c_char,
    active_is_left: u8,
) -> *mut c_char {
    guarded(|| {
        let (left, right) = match (borrowed(left_json), borrowed(right_json)) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let read = |raw: &str| -> Result<lost_commander_core::diff::Side, serde_json::Error> {
            let offer: SideOffer = serde_json::from_str(raw)?;
            Ok(lost_commander_core::diff::Side {
                marked: offer.marked.into_iter().map(PathBuf::from).collect(),
                cursor: offer.cursor.map(PathBuf::from),
            })
        };
        let (left, right) = match (read(&left), read(&right)) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };

        match lost_commander_core::diff::choose_from(&left, &right, active_is_left != 0) {
            Ok(chosen) => replied(&ChosenDto {
                left: chosen.left.display().to_string(),
                right: chosen.right.display().to_string(),
                from_one_pane: chosen.from_one_pane,
            }),
            // The engine's own words: they say what to do instead.
            Err(why) => failed(why),
        }
    })
}

/// Compare two files line by line.
///
/// Returns the rows, or `{"error":...}` where the two cannot be compared that
/// way. That refusal is worth reading rather than reporting as a failure: for
/// two binaries it names the byte at which they first differ, because an offset
/// is somewhere you can go and look and "they differ" is not.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_diff(left: *const c_char, right: *const c_char) -> *mut c_char {
    guarded(|| {
        let (left, right) = match (borrowed(left), borrowed(right)) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let diff =
            match lost_commander_core::diff::compare_files(Path::new(&left), Path::new(&right)) {
                Ok(diff) => diff,
                // The refusal's own words: it knows why, and it says where.
                Err(refusal) => return failed(refusal.message()),
            };
        replied(&DiffDto {
            rows: diff
                .rows
                .iter()
                .map(|row| {
                    let (kind, left, right) = match row {
                        lost_commander_core::diff::Row::Same { .. } => {
                            ("same", row.left(), row.right())
                        }
                        lost_commander_core::diff::Row::OnlyLeft { .. } => {
                            ("left", row.left(), None)
                        }
                        lost_commander_core::diff::Row::OnlyRight { .. } => {
                            ("right", None, row.right())
                        }
                    };
                    DiffRow {
                        kind,
                        left_number: left.map(|(n, _)| n),
                        left_text: left.map(|(_, text)| text.to_string()),
                        right_number: right.map(|(n, _)| n),
                        right_text: right.map(|(_, text)| text.to_string()),
                    }
                })
                .collect(),
            changes: diff.changes,
            unaligned: diff.unaligned,
            identical: diff.is_identical(),
        })
    })
}

// ---- comparing two directories ------------------------------------------

/// One side of a comparison, as the front-end already has it.
#[derive(Deserialize)]
struct SideDto {
    name: String,
    size: u64,
    /// Seconds since the epoch, or null where the filesystem gave none.
    modified: Option<i64>,
    is_dir: bool,
}

#[derive(Serialize)]
struct Differences {
    /// Names to mark on the left, and on the right.
    left: Vec<String>,
    right: Vec<String>,
}

/// Which files differ between two listings.
///
/// Both sides go in as `[{"name":...,"size":...,"modified":...,"is_dir":...}]`
/// and what comes back is the names to mark on each side.
///
/// The comparison crosses rather than the marking, because the rule is the
/// interesting part and it is not obvious: a file only on one side is marked
/// there, a newer file is marked on the side it is newer on, and one that
/// differs with no clear direction is marked on *both* - because both are worth
/// looking at and neither is the answer. Directories are left alone, since
/// "these two folders differ" is a question about what is in them.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_compare(
    left_json: *const c_char,
    right_json: *const c_char,
    case_sensitive: u8,
) -> *mut c_char {
    guarded(|| {
        let (left, right) = match (borrowed(left_json), borrowed(right_json)) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let read = |raw: &str| -> Result<Vec<(String, compare::Facts)>, serde_json::Error> {
            let side: Vec<SideDto> = serde_json::from_str(raw)?;
            Ok(side
                .into_iter()
                .map(|row| {
                    (
                        row.name,
                        compare::Facts {
                            size: row.size,
                            modified: row.modified.map(|seconds| {
                                UNIX_EPOCH + std::time::Duration::from_secs(seconds.max(0) as u64)
                            }),
                            is_dir: row.is_dir,
                        },
                    )
                })
                .collect())
        };
        let (left, right) = match (read(&left), read(&right)) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };

        let (mark_left, mark_right) = compare::to_mark(&left, &right, case_sensitive != 0);
        replied(&Differences {
            left: mark_left,
            right: mark_right,
        })
    })
}

// ---- duplicates ---------------------------------------------------------

/// A running duplicate hunt. The front-end only ever holds the pointer.
pub struct RcmdDupes {
    scan: lost_commander_core::dupes::Scan,
}

#[derive(Serialize)]
struct GroupDto {
    /// What each copy in this group weighs.
    size: u64,
    paths: Vec<String>,
}

#[derive(Serialize)]
struct DupesDto {
    /// Only the groups from the index asked for, as with a search.
    groups: Vec<GroupDto>,
    total: usize,
    /// What could be got back by keeping one of each and removing the rest.
    reclaimable: u64,
    current: String,
    finished: bool,
    cancelled: bool,
    truncated: bool,
}

/// Start hunting for duplicates under `root`.
///
/// `smallest` is the size below which a match is not worth reporting - empty
/// files are all copies of each other, which is true and useless.
///
/// # Safety
/// `root` must be a valid NUL-terminated UTF-8 string. The handle returned must
/// be given to [`rcmd_dupes_free`] exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_dupes_start(
    root: *const c_char,
    include_hidden: u8,
    smallest: u64,
) -> *mut RcmdDupes {
    let started = catch_unwind(AssertUnwindSafe(|| {
        let root = borrowed(root).ok()?;
        Some(lost_commander_core::dupes::Scan::spawn(
            PathBuf::from(root),
            lost_commander_core::dupes::Options {
                include_hidden: include_hidden != 0,
                smallest: smallest.max(1),
            },
        ))
    }));

    match started {
        Ok(Some(scan)) => Box::into_raw(Box::new(RcmdDupes { scan })),
        _ => std::ptr::null_mut(),
    }
}

/// How the hunt is going, and the groups from `from` onwards.
///
/// # Safety
/// `scan` must be a handle from [`rcmd_dupes_start`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_dupes_progress(scan: *mut RcmdDupes, from: usize) -> *mut c_char {
    guarded(|| {
        let Some(scan) = scan.as_ref() else {
            return failed("no scan");
        };
        let found = scan.scan.snapshot();
        // Worked out here rather than in the front-end: it is the number the
        // window exists to show, and both front-ends should agree on it.
        let reclaimable: u64 = found
            .groups
            .iter()
            .map(|group| group.size * (group.copies.len().saturating_sub(1)) as u64)
            .sum();
        replied(&DupesDto {
            groups: found
                .groups
                .iter()
                .skip(from)
                .map(|group| GroupDto {
                    size: group.size,
                    paths: group
                        .copies
                        .iter()
                        .map(|copy| copy.path.display().to_string())
                        .collect(),
                })
                .collect(),
            total: found.groups.len(),
            reclaimable,
            current: found.current.clone(),
            finished: found.finished,
            cancelled: found.cancelled,
            truncated: found.truncated,
        })
    })
}

/// Ask a hunt to stop. It stops between files, so this returns at once.
///
/// # Safety
/// `scan` must be a live handle from [`rcmd_dupes_start`].
#[no_mangle]
pub unsafe extern "C" fn rcmd_dupes_stop(scan: *mut RcmdDupes) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(scan) = scan.as_ref() {
            scan.scan.request_stop();
        }
    }));
}

/// Finish with a hunt, stopping and waiting rather than detaching.
///
/// # Safety
/// `scan` must be a handle from [`rcmd_dupes_start`], freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_dupes_free(scan: *mut RcmdDupes) {
    if scan.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(scan))));
}

// ---- searching ----------------------------------------------------------

/// A running search. The front-end only ever holds the pointer.
pub struct RcmdSearch {
    search: lost_commander_core::find::Search,
}

#[derive(Deserialize)]
struct QueryDto {
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    contains: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    include_hidden: bool,
}

#[derive(Serialize)]
struct HitDto {
    path: String,
    name: String,
    /// Where in the file the text was found, for a content search.
    line: Option<usize>,
    excerpt: Option<String>,
}

#[derive(Serialize)]
struct FoundDto {
    /// Only the hits from the index asked for, so a poll does not re-send
    /// everything already on screen.
    hits: Vec<HitDto>,
    /// How many there are altogether, which is what the caller counts from.
    total: usize,
    /// Where the walk has got to, for the line that says it is still going.
    current: String,
    finished: bool,
    cancelled: bool,
    /// It stopped at the cap rather than because it ran out of tree. Said out
    /// loud, because a list that quietly stopped short reads as a list of
    /// everything.
    truncated: bool,
}

/// Start searching under `root`.
///
/// `query_json` is `{"pattern":"*.rs","contains":"todo","case_sensitive":false,
/// "include_hidden":false}`. Returns null if the query asks for nothing - an
/// empty pattern with no text to find would walk the whole disk to report every
/// file on it.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings. The handle
/// returned must be given to [`rcmd_find_free`] exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_find_start(
    root: *const c_char,
    query_json: *const c_char,
) -> *mut RcmdSearch {
    let started = catch_unwind(AssertUnwindSafe(|| {
        let root = borrowed(root).ok()?;
        let raw = borrowed(query_json).ok()?;
        let dto: QueryDto = serde_json::from_str(&raw).ok()?;
        let query = lost_commander_core::find::Query {
            pattern: dto.pattern,
            contains: dto.contains,
            case_sensitive: dto.case_sensitive,
            include_hidden: dto.include_hidden,
        };
        if query.is_empty() {
            return None;
        }
        Some(lost_commander_core::find::Search::spawn(
            PathBuf::from(root),
            query,
        ))
    }));

    match started {
        Ok(Some(search)) => Box::into_raw(Box::new(RcmdSearch { search })),
        _ => std::ptr::null_mut(),
    }
}

/// How the search is going, and the hits from `from` onwards.
///
/// # Safety
/// `search` must be a handle from [`rcmd_find_start`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_find_progress(search: *mut RcmdSearch, from: usize) -> *mut c_char {
    guarded(|| {
        let Some(search) = search.as_ref() else {
            return failed("no search");
        };
        let found = search.search.snapshot();
        replied(&FoundDto {
            hits: found
                .hits
                .iter()
                .skip(from)
                .map(|hit| HitDto {
                    name: hit
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| hit.path.display().to_string()),
                    path: hit.path.display().to_string(),
                    line: hit.line,
                    excerpt: hit.excerpt.clone(),
                })
                .collect(),
            total: found.hits.len(),
            current: found.current.clone(),
            finished: found.finished,
            cancelled: found.cancelled,
            truncated: found.truncated,
        })
    })
}

/// Ask a search to stop. It stops between files, so this returns at once.
///
/// # Safety
/// `search` must be a live handle from [`rcmd_find_start`].
#[no_mangle]
pub unsafe extern "C" fn rcmd_find_stop(search: *mut RcmdSearch) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(search) = search.as_ref() {
            search.search.request_stop();
        }
    }));
}

/// Finish with a search.
///
/// Stops and waits, rather than detaching: a walk still running after the
/// window has forgotten about it would go on reading a disk nobody is watching.
///
/// # Safety
/// `search` must be a handle from [`rcmd_find_start`], freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_find_free(search: *mut RcmdSearch) {
    if search.is_null() {
        return;
    }
    // `Search` stops and joins in its own Drop, so this only has to let it go.
    let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(search))));
}

/// What was found inside an archive, at one level.
#[derive(Serialize)]
struct ArchiveListing {
    /// Which reader opened it: `"zip"`, `"tar.gz"` and so on.
    format: String,
    /// The level being shown, `/` separated and empty at the root.
    at: String,
    entries: Vec<EntryDto>,
    /// True where something inside needs a password, even though it listed.
    /// A zip keeps its names in the clear, so a locked archive can be looked
    /// through without being opened - and the front-end should say so before
    /// somebody tries to extract and is asked out of nowhere.
    locked: bool,
}

/// The last few archives read, so walking about inside one does not re-read it.
///
/// Purely a cache: keyed on the path together with its size and modification
/// time, so an archive that changes on disk is read again rather than answered
/// from a stale index. It exists because the formats differ enormously in what
/// listing costs - a zip's index is a few kilobytes at the end of the file,
/// while a `.tar.gz` has to be decompressed from the beginning to find out what
/// is in it. Re-reading that on every step into a subdirectory would mean
/// decompressing the whole archive per keystroke.
///
/// Bounded, and small: two panes can be inside two archives, and each may have
/// a tab or two. Past that the oldest goes.
static ARCHIVES: std::sync::Mutex<Vec<(ArchiveKey, std::sync::Arc<archive::Listing>)>> =
    std::sync::Mutex::new(Vec::new());

const ARCHIVES_KEPT: usize = 4;

#[derive(PartialEq, Eq, Clone)]
struct ArchiveKey {
    path: String,
    len: u64,
    modified: Option<std::time::SystemTime>,
    /// A password changes what can be read, so an archive opened with one is
    /// not the same listing as the same archive opened without.
    password: Option<String>,
}

fn archive_listing(
    path: &Path,
    password: Option<&str>,
) -> io::Result<std::sync::Arc<archive::Listing>> {
    let stat = std::fs::metadata(path)?;
    let key = ArchiveKey {
        path: path.display().to_string(),
        len: stat.len(),
        modified: stat.modified().ok(),
        password: password.map(|p| p.to_string()),
    };

    if let Ok(cache) = ARCHIVES.lock() {
        if let Some((_, found)) = cache.iter().find(|(k, _)| *k == key) {
            return Ok(found.clone());
        }
    }

    let listing = std::sync::Arc::new(archive::list_with(path, password)?);
    if let Ok(mut cache) = ARCHIVES.lock() {
        cache.retain(|(k, _)| *k != key);
        cache.push((key, listing.clone()));
        while cache.len() > ARCHIVES_KEPT {
            cache.remove(0);
        }
    }
    Ok(listing)
}

/// List one level inside an archive.
///
/// `at` is the level, `/` separated, empty for the root. `password` may be
/// empty. Returns `{"format":...,"at":...,"entries":[...],"locked":bool}`, or
/// on failure `{"error":...,"needs_password":bool,"wrong_password":bool}`.
///
/// Those two flags are the point of the error shape. "Locked" means ask;
/// "wrong" means ask again and say why. Collapsing them into one message would
/// leave the front-end guessing at the difference between "I have not been
/// asked yet" and "what you gave me is not it" by matching on a string.
///
/// # Safety
/// All three arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_archive_list(
    path: *const c_char,
    at: *const c_char,
    password: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        let at = borrowed(at).unwrap_or_default();
        let password = borrowed(password).ok().filter(|p| !p.is_empty());

        let listing = match archive_listing(Path::new(&path), password.as_deref()) {
            Ok(listing) => listing,
            Err(e) => return refused(&e),
        };

        let at = archive::normalise(&at);
        let mut entries: Vec<EntryDto> = Vec::new();
        // A row to go back up, so the front-end walks an archive with the same
        // keys it walks a directory with. At the root it leaves the archive.
        entries.push(EntryDto {
            name: "..".to_string(),
            path: parent_level(&at),
            kind: "parent",
            filekind: "parent",
            size: 0,
            modified: None,
            is_symlink: false,
            is_dir: true,
        });
        for level in archive::at(&listing.members, &at) {
            entries.push(EntryDto {
                filekind: if level.is_dir {
                    "folder"
                } else {
                    lost_commander_core::filekind::of_name(&level.name).label()
                },
                name: level.name,
                path: level.path,
                kind: if level.is_dir { "dir" } else { "file" },
                size: level.size,
                modified: level.modified.and_then(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|since| since.as_secs() as i64)
                }),
                is_symlink: false,
                is_dir: level.is_dir,
            });
        }

        replied(&ArchiveListing {
            format: listing.format.to_string(),
            at,
            entries,
            locked: listing.any_locked(),
        })
    })
}

/// One level up inside an archive, or empty at the root.
fn parent_level(at: &str) -> String {
    match at.rsplit_once('/') {
        Some((above, _)) => above.to_string(),
        None => String::new(),
    }
}

/// An error, with the two password answers told apart.
fn refused(error: &io::Error) -> *mut c_char {
    #[derive(Serialize)]
    struct Refused {
        error: String,
        needs_password: bool,
        wrong_password: bool,
    }
    let refused = archive::was_refused(error);
    replied(&Refused {
        error: error.to_string(),
        // "Needs" means it has not been asked for yet, which is the locked
        // case minus the refused one - otherwise a wrong password would read
        // as both at once and the front-end would not know which to say.
        needs_password: archive::is_locked(error) && !refused,
        wrong_password: refused,
    })
}

/// The directory tree, opened down to `target` and at every path in `expanded`.
///
/// `expanded_json` is a JSON array of paths the caller wants open. The reply is
/// `{"nodes":[{path,label,depth,expanded,leaf}]}` - the visible nodes, in
/// display order, already flattened.
///
/// Deliberately stateless. The engine's `Tree` is a mutable thing driven by
/// index, and keeping one alive here would mean a handle to create, destroy and
/// leak, plus indices that go stale the moment anything else changes. Instead
/// the front-end owns the only state worth owning - the set of paths it has
/// opened, which is a few strings and survives a reload - and the walking and
/// flattening happen here. The cost is rebuilding on each toggle, which is one
/// `read_dir` per open node rather than a walk of the disk.
///
/// # Safety
/// `target` and `expanded_json` must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_tree(
    target: *const c_char,
    expanded_json: *const c_char,
    show_hidden: u8,
    show_files: u8,
) -> *mut c_char {
    guarded(|| {
        let target = match borrowed(target) {
            Ok(target) => target,
            Err(e) => return failed(e),
        };
        let wanted: Vec<String> = borrowed(expanded_json)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        // Compared case-insensitively, because Windows hands the same directory
        // back with whatever capitalisation the caller used and a set keyed on
        // the exact spelling would quietly fail to match it.
        let wanted: std::collections::HashSet<String> =
            wanted.into_iter().map(|p| p.to_lowercase()).collect();

        let mut tree = lost_commander_core::tree::Tree::revealing_showing(
            Path::new(&target),
            show_hidden != 0,
            show_files != 0,
        );

        // Opening a node reveals children that may themselves want opening, so
        // this goes round until nothing is left to do. Bounded by the number of
        // nodes, because each pass sets `expanded` on one that was not before -
        // a bound rather than a `while true`, since a symlink loop under an
        // opened path would otherwise not have to stop.
        let mut guard = 0;
        while guard < 10_000 {
            guard += 1;
            let next = tree.nodes.iter().position(|node| {
                !node.expanded
                    && !node.leaf
                    && wanted.contains(&node.path.display().to_string().to_lowercase())
            });
            match next {
                Some(index) => tree.expand(index),
                None => break,
            }
        }

        replied(&TreeNodes {
            nodes: tree
                .nodes
                .iter()
                .map(|node| {
                    // One stat per visible node. Only what is on screen is in
                    // `nodes` - a shut directory's children were never read -
                    // so this costs a walk of what is open, not of the disk.
                    let facts = std::fs::symlink_metadata(&node.path);
                    let is_symlink = facts
                        .as_ref()
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false);
                    TreeNode {
                        path: node.path.display().to_string(),
                        label: node.label.clone(),
                        depth: node.depth,
                        expanded: node.expanded,
                        leaf: node.leaf,
                        name: node.label.clone(),
                        kind: if node.is_dir { "dir" } else { "file" },
                        filekind: if node.is_dir {
                            lost_commander_core::filekind::Kind::Folder.label()
                        } else {
                            lost_commander_core::filekind::of_name(&node.label).label()
                        },
                        // Zero for a directory, which is what a listing shows
                        // too - the size of a directory is the size of what is
                        // in it, and that is a question the scan answers.
                        size: match &facts {
                            Ok(m) if !node.is_dir => m.len(),
                            _ => 0,
                        },
                        modified: facts.as_ref().ok().and_then(|m| {
                            m.modified().ok().and_then(|time| {
                                time.duration_since(UNIX_EPOCH)
                                    .ok()
                                    .map(|since| since.as_secs() as i64)
                            })
                        }),
                        is_symlink,
                        is_dir: node.is_dir,
                    }
                })
                .collect(),
        })
    })
}

/// What to label a pane's tabs, given the directories they are showing.
///
/// `paths_json` is a JSON array of paths; the reply is `{"titles":[...]}`, one
/// per path, in order.
///
/// Only the labelling crosses, not the tabs themselves. A tab is an arrangement
/// of the window - which of several directories this pane is looking at - and
/// each front-end arranges differently. What is not obvious, and is worth
/// having once rather than twice, is the rule for telling two of them apart:
/// a title that collides takes its parent with it, *unless* the collision is
/// two tabs on the same directory, where no amount of path distinguishes them
/// and the long form would only be noise.
///
/// # Safety
/// `paths_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_tab_titles(paths_json: *const c_char) -> *mut c_char {
    guarded(|| {
        let raw = match borrowed(paths_json) {
            Ok(raw) => raw,
            Err(e) => return failed(e),
        };
        let paths: Vec<String> = match serde_json::from_str(&raw) {
            Ok(paths) => paths,
            Err(e) => return failed(e),
        };
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        replied(&Titles {
            titles: lost_commander_core::tabs::titles(&paths),
        })
    })
}

/// Where the bookmarks live: the real file, unless `RCMD_BOOKMARKS_PATH`
/// says otherwise - the same escape hatch the settings and the pins have,
/// for the same reason: a test must never write the sidebar of whoever
/// ran it.
fn bookmarks_path() -> Option<PathBuf> {
    match std::env::var_os("RCMD_BOOKMARKS_PATH") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => netloc::Bookmarks::config_path(),
    }
}

/// Where the recent places live, unless `RCMD_RECENT_PATH` says otherwise.
fn recent_places_path() -> Option<PathBuf> {
    match std::env::var_os("RCMD_RECENT_PATH") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => netloc::Bookmarks::recent_path(),
    }
}

/// Both lists, read from wherever the paths point right now.
fn bookmarks_now() -> netloc::Bookmarks {
    let mut saved = bookmarks_path()
        .and_then(|path| netloc::Bookmarks::load_from(&path).ok())
        .unwrap_or_default();
    if let Some(path) = recent_places_path() {
        saved.load_recent_from(&path);
    }
    saved
}

/// The pinned places and the recently visited ones.
///
/// Returns `{"pinned":[...],"recent":[...]}`. Never an error: an unreadable or
/// missing bookmarks file means "no bookmarks", which is a true answer, and a
/// sidebar that refused to draw because a config file was corrupt would be a
/// worse outcome than an empty one.
///
/// These are read and written afresh on every call rather than held here. The
/// file is a handful of lines, there is no handle for the front-end to leak,
/// and - the reason that matters - the terminal front-end is writing to the
/// same file, so a copy cached in memory would go stale the moment both are
/// open at once.
#[no_mangle]
pub extern "C" fn rcmd_places() -> *mut c_char {
    guarded(|| {
        let saved = bookmarks_now();
        replied(&Places {
            pinned: saved.locations.iter().map(Place::from).collect(),
            recent: saved.recent.iter().map(Place::from).collect(),
        })
    })
}

/// Pin a directory, or record having visited one.
///
/// `pinned` chooses which list it goes in. Returns `{"pinned":...}` as
/// [`rcmd_places`] does, so the front-end can redraw from the result rather
/// than asking again.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_place_add(path: *const c_char, pinned: u8) -> *mut c_char {
    guarded(|| {
        let path = match borrowed(path) {
            Ok(path) => path,
            Err(e) => return failed(e),
        };
        if path.trim().is_empty() {
            return failed("no place to remember");
        }

        let mut saved = bookmarks_now();
        let place = netloc::Location::local(&path);
        // `add` replaces by name, so pinning the same folder twice updates it
        // rather than making a second entry; `push_recent` moves a re-visited
        // place to the top for the same reason.
        //
        // Each list is saved to the file it lives in. The recent list moved
        // out to `recent.toml` and `save_to` skips it by design - saving
        // `bookmarks.toml` here recorded a visit into a file that does not
        // hold visits, which is how every sidebar drew RECENT empty.
        //
        // Said out loud if it could not be written. A pin that silently
        // failed to persist would look pinned until the program restarted.
        if pinned != 0 {
            saved.add(place);
            if let Some(file) = bookmarks_path() {
                if let Err(e) = saved.save_to(&file) {
                    return failed(e);
                }
            }
        } else {
            saved.push_recent(place);
            if let Some(file) = recent_places_path() {
                if let Err(e) = saved.save_recent_to(&file) {
                    return failed(e);
                }
            }
        }
        replied(&Places {
            pinned: saved.locations.iter().map(Place::from).collect(),
            recent: saved.recent.iter().map(Place::from).collect(),
        })
    })
}

/// Unpin the place called `name`, or forget a recent one.
///
/// By name rather than by position: the front-end draws a list it fetched
/// earlier, and an index into it means the wrong row if anything has changed
/// the list since - including the other front-end, writing the same file.
///
/// # Safety
/// `name` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rcmd_place_remove(name: *const c_char, pinned: u8) -> *mut c_char {
    guarded(|| {
        let name = match borrowed(name) {
            Ok(name) => name,
            Err(e) => return failed(e),
        };

        let mut saved = bookmarks_now();
        let list = if pinned != 0 {
            &mut saved.locations
        } else {
            &mut saved.recent
        };
        let before = list.len();
        list.retain(|place| place.name != name && place.to_url() != name);
        if list.len() == before {
            return failed(format!("{name} is not there"));
        }

        // The file the changed list lives in: `save_to` does not carry the
        // recent list, so forgetting a recent place must write `recent.toml`
        // or the row comes back on the next read.
        if pinned != 0 {
            if let Some(file) = bookmarks_path() {
                if let Err(e) = saved.save_to(&file) {
                    return failed(e);
                }
            }
        } else if let Some(file) = recent_places_path() {
            if let Err(e) = saved.save_recent_to(&file) {
                return failed(e);
            }
        }
        replied(&Places {
            pinned: saved.locations.iter().map(Place::from).collect(),
            recent: saved.recent.iter().map(Place::from).collect(),
        })
    })
}

/// Which of `names` match a glob.
///
/// `names_json` is a JSON array of strings; the reply is `{"matched":[...]}`,
/// one boolean per name, in the order given.
///
/// The whole list crosses at once rather than a call per name: a directory of
/// ten thousand files would otherwise mean ten thousand round trips to answer
/// one question about a pattern.
///
/// The matching itself is the engine's, deliberately. It backtracks, and it
/// folds case because selection by pattern always wants the forgiving rule -
/// nobody typing `*.JPG` means to leave the lower-case ones behind. A second
/// implementation in the front-end would be a second definition of "matches",
/// free to disagree with this one about exactly those edge cases.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_glob_match(
    pattern: *const c_char,
    names_json: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let (pattern, names) = match (borrowed(pattern), borrowed(names_json)) {
            (Ok(pattern), Ok(names)) => (pattern, names),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        let names: Vec<String> = match serde_json::from_str(&names) {
            Ok(names) => names,
            Err(e) => return failed(e),
        };
        replied(&Matched {
            matched: names
                .iter()
                .map(|name| lost_commander_core::panel::matches_glob(&pattern, name))
                .collect(),
        })
    })
}

/// Rename `path` to `new_name`, in the directory it is already in.
///
/// Returns `{"path":"..."}` or `{"error":"..."}`. Renaming is not moving: the
/// engine refuses a name with a separator in it, so this cannot be used to
/// walk a file into another directory by the back door.
///
/// # Safety
/// Both arguments must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn rcmd_rename(path: *const c_char, new_name: *const c_char) -> *mut c_char {
    guarded(|| {
        let (path, new_name) = match (borrowed(path), borrowed(new_name)) {
            (Ok(path), Ok(name)) => (path, name),
            (Err(e), _) | (_, Err(e)) => return failed(e),
        };
        match lost_commander_core::fsops::rename(Path::new(&path), &new_name) {
            Ok(path) => replied(&Made {
                path: path.display().to_string(),
            }),
            Err(e) => failed(e),
        }
    })
}

/// How far along a job is, as JSON.
///
/// # Safety
/// `job` must be a handle from [`rcmd_copy_start`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_job_progress(job: *mut RcmdJob) -> *mut c_char {
    guarded(|| {
        let Some(job) = job.as_ref() else {
            return failed("no job");
        };
        let snapshot = job.job.snapshot();
        replied(&ProgressDto {
            verb: snapshot.verb.to_string(),
            current: snapshot.current.clone(),
            items_done: snapshot.items_done,
            items_total: snapshot.items_total,
            items_skipped: snapshot.items_skipped,
            bytes_done: snapshot.bytes_done,
            bytes_total: snapshot.bytes_total,
            fraction: snapshot.fraction(),
            finished: snapshot.finished,
            cancelled: snapshot.cancelled,
            failures: snapshot.failures.clone(),
        })
    })
}

/// Ask a job to stop. It stops between files, so this returns at once.
///
/// # Safety
/// `job` must be a live handle from [`rcmd_copy_start`].
#[no_mangle]
pub unsafe extern "C" fn rcmd_job_cancel(job: *mut RcmdJob) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(job) = job.as_ref() {
            job.job.request_cancel();
        }
    }));
}

/// Finish with a job.
///
/// Cancels and waits, rather than detaching: a worker still copying after the
/// window has forgotten about it would go on writing files nobody is watching,
/// and the user would have no way left to stop it.
///
/// # Safety
/// `job` must be a handle from [`rcmd_copy_start`], freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn rcmd_job_free(job: *mut RcmdJob) {
    if job.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let mut owned = Box::from_raw(job);
        owned.job.request_cancel();
        owned.job.join();
    }));
}

/// Give back a string this library returned.
///
/// # Safety
/// `text` must have come from one of the functions above and not been freed.
#[no_mangle]
pub unsafe extern "C" fn rcmd_string_free(text: *mut c_char) {
    if text.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(CString::from_raw(text));
    }));
}

// ---- the account, read back -----------------------------------------------
//
// Everything below is the parity surface: the thirteen features the engine
// grew that no front-end could reach through this boundary. Same contract
// as everything above - JSON out, the caller polls, keys cross by name.

#[derive(Serialize)]
struct PastLine {
    line: String,
    cwd: String,
}

/// Commands run before now, `here` first, optionally narrowed.
///
/// `days` bounds how far back; `query` narrows case-insensitively (empty
/// means everything); `here_only` non-zero keeps only this directory's.
///
/// # Safety
/// `here` and `query` follow the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_history(
    here: *const c_char,
    days: i32,
    query: *const c_char,
    here_only: i32,
) -> *mut c_char {
    guarded(|| {
        let here = match borrowed(here) {
            Ok(text) => PathBuf::from(text),
            Err(e) => return failed(e),
        };
        let query = borrowed(query).unwrap_or_default();
        let Some(book) = account() else {
            return replied(&Vec::<PastLine>::new());
        };
        let past = book.with_records(journal::Stream::Shell, |records| {
            journal::commands_before(journal::since(records, days.max(1) as i64), &here)
        });
        let matched = journal::matching(&past, &query);
        let lines: Vec<PastLine> = matched
            .into_iter()
            .filter(|past| here_only == 0 || past.cwd == here)
            .map(|past| PastLine {
                line: past.line.clone(),
                cwd: past.cwd.display().to_string(),
            })
            .collect();
        replied(&lines)
    })
}

/// How many times the account has changed since this process began.
///
/// A front-end refilters its history views when this moves, instead of
/// re-reading once a second in case something happened. It moves with every
/// write made through this boundary - jobs, `rcmd_term_journal` - and with
/// sweeps.
#[no_mangle]
pub extern "C" fn rcmd_journal_generation() -> *mut c_char {
    guarded(|| {
        let generation = account().map(|book| book.generation()).unwrap_or(0);
        out(format!("{{\"generation\":{generation}}}"))
    })
}

#[derive(Serialize)]
struct FolderHappening {
    at: i64,
    kind: String,
    name: String,
    other: Option<String>,
    incoming: bool,
    failed: Option<String>,
}

/// What was done to the things in one folder, newest first, failures kept.
///
/// # Safety
/// `here` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_folder_history(here: *const c_char, days: i32) -> *mut c_char {
    guarded(|| {
        let here = match borrowed(here) {
            Ok(text) => PathBuf::from(text),
            Err(e) => return failed(e),
        };
        let Some(book) = account() else {
            return replied(&Vec::<FolderHappening>::new());
        };
        let rows: Vec<FolderHappening> = book.with_records(journal::Stream::Files, |records| {
            journal::happened_in(journal::since(records, days.max(1) as i64), &here)
                .into_iter()
                .map(|happening| FolderHappening {
                    at: happening.at,
                    kind: happening.kind.label().to_string(),
                    name: happening.name,
                    other: happening.other.map(|path| path.display().to_string()),
                    incoming: happening.incoming,
                    failed: happening.failed,
                })
                .collect()
        });
        replied(&rows)
    })
}

// ---- pinned commands -------------------------------------------------------

/// Where the pins live: the real file, unless `RCMD_PINNED_PATH` says
/// otherwise - the same escape hatch the settings have, for the same
/// reason: a test must never write the shelf of whoever ran it.
fn pinned_path() -> Option<PathBuf> {
    match std::env::var_os("RCMD_PINNED_PATH") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => lost_commander_core::pinned::Pinned::path(),
    }
}

fn pinned_now() -> lost_commander_core::pinned::Pinned {
    pinned_path()
        .and_then(|path| lost_commander_core::pinned::Pinned::load_from(&path).ok())
        .unwrap_or_default()
}

/// This folder's shelf, in the order it was built.
///
/// # Safety
/// `cwd` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_pins(cwd: *const c_char) -> *mut c_char {
    guarded(|| {
        let cwd = match borrowed(cwd) {
            Ok(text) => PathBuf::from(text),
            Err(e) => return failed(e),
        };
        let lines: Vec<String> = pinned_now()
            .here(&cwd)
            .iter()
            .map(|pin| pin.line.clone())
            .collect();
        replied(&lines)
    })
}

/// Pin a line to a folder, or take the pin off - and say which happened.
///
/// # Safety
/// `cwd` and `line` follow the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_pin_toggle(cwd: *const c_char, line: *const c_char) -> *mut c_char {
    guarded(|| {
        let cwd = match borrowed(cwd) {
            Ok(text) => PathBuf::from(text),
            Err(e) => return failed(e),
        };
        let line = match borrowed(line) {
            Ok(text) => text,
            Err(e) => return failed(e),
        };
        let Some(path) = pinned_path() else {
            return failed("no configuration directory on this platform");
        };
        let mut pinned = pinned_now();
        let pinned_now = pinned.toggle(&cwd, &line);
        if let Err(e) = pinned.save_to(&path) {
            return failed(format!("could not save the pins: {e}"));
        }
        out(format!("{{\"pinned\":{pinned_now}}}"))
    })
}

// ---- undo ------------------------------------------------------------------

#[derive(Serialize)]
struct UndoReply {
    nothing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<UndoRefused>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<lost_commander_core::undo::Plan>,
}

#[derive(Serialize)]
struct UndoRefused {
    what: String,
    why: String,
}

/// The last file operation and exactly what reversing it would do - or why
/// it cannot be. The front-end shows the plan verbatim and hands the same
/// JSON to `rcmd_undo_apply`, so what was approved is what runs.
#[no_mangle]
pub extern "C" fn rcmd_undo_plan() -> *mut c_char {
    guarded(|| {
        use lost_commander_core::undo::{self, Undoable};
        let Some(book) = account() else {
            return replied(&UndoReply {
                nothing: true,
                refused: None,
                plan: None,
            });
        };
        let answer = book.with_records(journal::Stream::Files, |records| undo::plan(records));
        let reply = match answer {
            Undoable::Nothing => UndoReply {
                nothing: true,
                refused: None,
                plan: None,
            },
            Undoable::Refused { what, why } => UndoReply {
                nothing: false,
                refused: Some(UndoRefused { what, why }),
                plan: None,
            },
            Undoable::Plan(plan) => UndoReply {
                nothing: false,
                refused: None,
                plan: Some(plan),
            },
        };
        replied(&reply)
    })
}

#[derive(Serialize)]
struct UndoFailure {
    path: String,
    why: String,
}

/// Do what a plan from `rcmd_undo_plan` said, and account for the doing.
///
/// # Safety
/// `plan_json` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_undo_apply(plan_json: *const c_char) -> *mut c_char {
    guarded(|| {
        use lost_commander_core::undo::{self, Step};
        let text = match borrowed(plan_json) {
            Ok(text) => text,
            Err(e) => return failed(e),
        };
        let plan: undo::Plan = match serde_json::from_str(&text) {
            Ok(plan) => plan,
            Err(e) => return failed(format!("that is not an undo plan: {e}")),
        };
        let failures = undo::apply(&plan);
        // The reversal is the newest operation now, recorded as what it
        // literally did - which is what makes undoing an undo work.
        if let Some(book) = account() {
            for step in &plan.steps {
                let event = match step {
                    Step::RemoveCopied { copy } => journal::Event::new(journal::Kind::Delete, copy)
                        .note("undo: the copy is removed"),
                    Step::MoveBack { now, was } => journal::Event::new(journal::Kind::Move, now)
                        .to(was)
                        .note("undo"),
                    Step::RemoveMade { dir } => journal::Event::new(journal::Kind::Delete, dir)
                        .note("undo: the directory is removed"),
                    Step::RestoreFromTrash { item } => {
                        journal::Event::new(journal::Kind::Move, &item.original)
                            .note("undo: restored from the trash")
                    }
                };
                book.record(event);
            }
        }
        let named: Vec<UndoFailure> = failures
            .into_iter()
            .map(|(path, why)| UndoFailure {
                path: path.display().to_string(),
                why,
            })
            .collect();
        replied(&named)
    })
}

// ---- the trash -------------------------------------------------------------

/// Everything deletion kept, each with where it came from and when.
#[no_mangle]
pub extern "C" fn rcmd_trash_list() -> *mut c_char {
    guarded(|| replied(&lost_commander_core::trash::list()))
}

/// Put one trashed thing back where it came from.
///
/// # Safety
/// `item_json` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_trash_restore(item_json: *const c_char) -> *mut c_char {
    guarded(|| {
        let item: lost_commander_core::trash::TrashedItem =
            match borrowed(item_json).and_then(|text| {
                serde_json::from_str(&text).map_err(|e| format!("that is not a trash item: {e}"))
            }) {
                Ok(item) => item,
                Err(e) => return failed(e),
            };
        match lost_commander_core::trash::restore(&item) {
            Ok(()) => {
                if let Some(book) = account() {
                    book.record(
                        journal::Event::new(journal::Kind::Move, &item.original)
                            .note("restored from the trash"),
                    );
                }
                out("{\"ok\":true}".to_string())
            }
            Err(e) => failed(e),
        }
    })
}

/// Remove one trashed thing for good.
///
/// # Safety
/// `item_json` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_trash_purge(item_json: *const c_char) -> *mut c_char {
    guarded(|| {
        let item: lost_commander_core::trash::TrashedItem =
            match borrowed(item_json).and_then(|text| {
                serde_json::from_str(&text).map_err(|e| format!("that is not a trash item: {e}"))
            }) {
                Ok(item) => item,
                Err(e) => return failed(e),
            };
        match lost_commander_core::trash::purge(&item) {
            Ok(()) => out("{\"ok\":true}".to_string()),
            Err(e) => failed(e),
        }
    })
}

// ---- saved windows ---------------------------------------------------------

/// Where the session lives, unless `RCMD_SESSION_PATH` says otherwise.
fn session_path() -> Option<PathBuf> {
    match std::env::var_os("RCMD_SESSION_PATH") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => lost_commander_core::session::Session::path(),
    }
}

/// The windows saved last time - the same file the other front-ends write,
/// so a session saved in one opens in any.
#[no_mangle]
pub extern "C" fn rcmd_session_read() -> *mut c_char {
    guarded(|| {
        let session = session_path()
            .and_then(|path| lost_commander_core::session::Session::load_from(&path).ok())
            .unwrap_or_default();
        replied(&session)
    })
}

/// Write the windows down, where the next start will find them.
///
/// # Safety
/// `session_json` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_session_save(session_json: *const c_char) -> *mut c_char {
    guarded(|| {
        let session: lost_commander_core::session::Session =
            match borrowed(session_json).and_then(|text| {
                serde_json::from_str(&text).map_err(|e| format!("that is not a session: {e}"))
            }) {
                Ok(session) => session,
                Err(e) => return failed(e),
            };
        let Some(path) = session_path() else {
            return failed("no configuration directory on this platform");
        };
        match session.save_to(&path) {
            Ok(()) => out("{\"ok\":true}".to_string()),
            Err(e) => failed(e),
        }
    })
}

// ---- the command line's engine half ----------------------------------------

/// `%f`, `%s` and `%d` expanded against what the panels show, each name
/// quoted the way this platform's shell wants. `marked_json` is a JSON array
/// of names.
///
/// # Safety
/// Every pointer follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_expand_command(
    line: *const c_char,
    file: *const c_char,
    marked_json: *const c_char,
    other_dir: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let line = match borrowed(line) {
            Ok(text) => text,
            Err(e) => return failed(e),
        };
        let file = borrowed(file).ok().filter(|name| !name.is_empty());
        let marked: Vec<String> = borrowed(marked_json)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let other = PathBuf::from(borrowed(other_dir).unwrap_or_default());
        let expanded = lost_commander_core::shell::expand_placeholders(
            &line,
            file.as_deref(),
            &marked,
            &other,
            lost_commander_core::mount::Platform::current(),
        );
        out(format!("{{\"expanded\":{}}}", json_string(&expanded)))
    })
}

/// Whether a line contains something expansion would change - the hint for
/// showing a preview only when there is one to show.
///
/// # Safety
/// `line` follows the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_has_placeholders(line: *const c_char) -> *mut c_char {
    guarded(|| {
        let line = borrowed(line).unwrap_or_default();
        out(format!(
            "{{\"placeholders\":{}}}",
            lost_commander_core::shell::has_placeholders(&line)
        ))
    })
}

#[derive(Serialize)]
struct ShellChoice {
    program: String,
    journaled: bool,
}

/// The shells on this machine, each saying whether its commands can be
/// recorded - pwsh and WSL included where they exist.
#[no_mangle]
pub extern "C" fn rcmd_shells() -> *mut c_char {
    guarded(|| {
        let shells: Vec<ShellChoice> = lost_commander_core::shell::discover_shells()
            .into_iter()
            .map(|program| ShellChoice {
                journaled: lost_commander_core::shellhook::journals(&program),
                program,
            })
            .collect();
        replied(&shells)
    })
}

/// The `cd` that moves one shell to one directory, in that shell's own
/// language - `cmd` gets `/d` and quotes, POSIX gets single quotes, and WSL
/// gets `/mnt/c` spelling, because a cd with the Windows spelling would fail
/// on every single directory.
///
/// # Safety
/// `program` and `path` follow the rules of every string in this ABI.
#[no_mangle]
pub unsafe extern "C" fn rcmd_cd_command(
    program: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    guarded(|| {
        let program = match borrowed(program) {
            Ok(text) => text,
            Err(e) => return failed(e),
        };
        let path = match borrowed(path) {
            Ok(text) => PathBuf::from(text),
            Err(e) => return failed(e),
        };
        let line = lost_commander_core::shell::cd_command(&program, &path);
        out(format!("{{\"line\":{}}}", json_string(&line)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::ffi::CString;
    use std::time::{Duration, Instant};

    /// Call across the boundary the way the front-end will, and read the
    /// reply back - including giving the string back afterwards.
    fn reply_of(pointer: *mut c_char) -> Value {
        assert!(!pointer.is_null(), "the boundary returned nothing at all");
        let text = unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .expect("valid UTF-8")
            .to_string();
        unsafe { rcmd_string_free(pointer) };
        serde_json::from_str(&text).expect("the reply should be JSON")
    }

    fn c(text: &str) -> CString {
        CString::new(text).unwrap()
    }

    #[test]
    fn a_listing_names_what_is_in_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let path = c(&dir.path().display().to_string());
        let sort = c("name");
        let natural = c("");
        let reply =
            reply_of(unsafe { rcmd_list(path.as_ptr(), 0, sort.as_ptr(), natural.as_ptr()) });

        let entries = reply["entries"].as_array().expect("entries");
        let named = |name: &str| {
            entries
                .iter()
                .find(|e| e["name"] == name)
                .unwrap_or_else(|| panic!("no {name} in {entries:?}"))
                .clone()
        };
        assert_eq!(named("one.txt")["kind"], "file");
        assert_eq!(named("one.txt")["size"], 1);
        assert_eq!(named("sub")["kind"], "dir");
        assert_eq!(named("sub")["is_dir"], true);

        // The walk-up row comes from the engine, so both front-ends agree
        // there is one - and there is exactly one. Adding a second here on the
        // way past put two ".." rows in the pane, which only showed up when
        // the window was actually run.
        let parents: Vec<_> = entries.iter().filter(|e| e["kind"] == "parent").collect();
        assert_eq!(parents.len(), 1, "one walk-up row, not two: {parents:?}");
        assert_eq!(parents[0]["name"], "..");
        assert_eq!(entries[0]["kind"], "parent", "and it leads the list");
    }

    #[test]
    fn a_directory_that_is_not_there_is_an_error_not_a_crash() {
        let path = c(r"C:\definitely\not\a\directory\anywhere");
        let sort = c("name");
        let natural = c("");
        let reply =
            reply_of(unsafe { rcmd_list(path.as_ptr(), 0, sort.as_ptr(), natural.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");
        // And nothing that could be mistaken for a listing came back.
        assert!(reply["entries"].is_null());
    }

    #[test]
    fn a_null_path_is_refused_rather_than_dereferenced() {
        let sort = c("name");
        let natural = c("");
        let reply =
            reply_of(unsafe { rcmd_list(std::ptr::null(), 0, sort.as_ptr(), natural.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");

        // A null sort is not fatal either - it just means name order.
        let path = c(".");
        let reply =
            reply_of(unsafe { rcmd_list(path.as_ptr(), 0, std::ptr::null(), std::ptr::null()) });
        assert!(reply["entries"].is_array(), "{reply:?}");
    }

    #[test]
    fn a_listing_is_ordered_the_way_it_was_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        // Names deliberately the opposite way round from the sizes, so name
        // order and size order cannot both be satisfied by accident.
        std::fs::write(dir.path().join("a-small.txt"), vec![b'x'; 10]).unwrap();
        std::fs::write(dir.path().join("m-mid.txt"), vec![b'x'; 500]).unwrap();
        std::fs::write(dir.path().join("z-big.txt"), vec![b'x'; 3000]).unwrap();
        let path = c(&dir.path().display().to_string());

        let names_of = |sort: &str, order: &str| -> Vec<String> {
            let sort = c(sort);
            let order = c(order);
            let reply =
                reply_of(unsafe { rcmd_list(path.as_ptr(), 0, sort.as_ptr(), order.as_ptr()) });
            reply["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "file")
                .map(|e| e["name"].as_str().unwrap().to_string())
                .collect()
        };

        // An empty order means the column's own: names read A to Z, and sizes
        // read biggest first, because that is what each is usually wanted for.
        assert_eq!(
            names_of("name", ""),
            ["a-small.txt", "m-mid.txt", "z-big.txt"]
        );
        assert_eq!(
            names_of("size", ""),
            ["z-big.txt", "m-mid.txt", "a-small.txt"]
        );

        // And asked for the other way round, each turns over.
        assert_eq!(
            names_of("name", "desc"),
            ["z-big.txt", "m-mid.txt", "a-small.txt"]
        );
        assert_eq!(
            names_of("size", "asc"),
            ["a-small.txt", "m-mid.txt", "z-big.txt"]
        );

        // An order nobody has heard of is the natural one rather than an error.
        assert_eq!(names_of("name", "sideways"), names_of("name", ""));
        // As is a column nobody has heard of.
        assert_eq!(names_of("sideways", ""), names_of("name", ""));
    }

    #[test]
    fn hidden_files_are_shown_only_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.txt"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        let path = c(&dir.path().display().to_string());
        let sort = c("name");
        let natural = c("");

        let count = |show: u8| {
            let reply = reply_of(unsafe {
                rcmd_list(path.as_ptr(), show, sort.as_ptr(), natural.as_ptr())
            });
            reply["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "file")
                .count()
        };
        assert_eq!(count(0), 1, "the dotfile is not shown by default");
        assert_eq!(count(1), 2, "and is when asked for");
    }

    #[test]
    fn a_directory_is_made_and_named_back() {
        let dir = tempfile::tempdir().unwrap();
        let parent = c(&dir.path().display().to_string());
        let name = c("new folder");

        let reply = reply_of(unsafe { rcmd_mkdir(parent.as_ptr(), name.as_ptr()) });
        let made = reply["path"].as_str().expect("a path back");
        assert!(Path::new(made).is_dir());
        assert!(dir.path().join("new folder").is_dir());

        // Twice is an error, not a silent success on the one already there.
        let again = reply_of(unsafe { rcmd_mkdir(parent.as_ptr(), name.as_ptr()) });
        assert!(again["error"].is_string(), "{again:?}");
    }

    #[test]
    fn a_name_that_would_escape_the_directory_is_refused() {
        // The front-end is not trusted with this. A name carrying a separator
        // would make "create a folder here" mean "create one somewhere else",
        // and the same trick on rename would move a file rather than rename it.
        let dir = tempfile::tempdir().unwrap();
        let parent = c(&dir.path().display().to_string());

        for bad in [r"..\escaped", "sub/escaped", "..", ".", "  "] {
            let name = c(bad);
            let reply = reply_of(unsafe { rcmd_mkdir(parent.as_ptr(), name.as_ptr()) });
            assert!(
                reply["error"].is_string(),
                "{bad:?} should be refused: {reply:?}"
            );
        }
        assert!(!dir.path().parent().unwrap().join("escaped").exists());
    }

    #[test]
    fn a_typed_path_means_what_it_means_at_a_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().display().to_string();
        let cwd = c(&here);

        let resolve = |typed: &str| -> Value {
            let typed = c(typed);
            reply_of(unsafe { rcmd_resolve_path(typed.as_ptr(), cwd.as_ptr()) })
        };

        // Relative, which is the whole reason not to write this again here.
        assert_eq!(
            resolve("sub")["path"].as_str(),
            Some(dir.path().join("sub").display().to_string().as_str())
        );
        // Absolute is left alone.
        assert_eq!(resolve(r"C:\Windows")["path"], r"C:\Windows");
        // Home, both ways of asking for it.
        let home = dirs::home_dir().unwrap().display().to_string();
        assert_eq!(resolve("~")["path"], home);
        assert_eq!(resolve("")["path"], home);
        // ".." is folded away rather than left in the text: an address bar
        // reading "C:\a\b\.." is a path nobody would have typed.
        let up = resolve("..")["path"].as_str().unwrap().to_string();
        assert!(!up.contains(".."), "should have been folded: {up}");
        assert_eq!(
            up,
            dir.path().parent().unwrap().display().to_string(),
            "one above where the pane is standing"
        );
        // Two of them fold twice.
        let twice = resolve(r"..\..")["path"].as_str().unwrap().to_string();
        assert_eq!(
            twice,
            dir.path()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn folding_a_path_keeps_what_it_cannot_fold() {
        // Nothing to fold into: dropping the ".." would quietly change which
        // directory was meant, so it stays.
        assert_eq!(tidied(Path::new("..")), PathBuf::from(".."));
        assert_eq!(tidied(Path::new(r"..\..")), PathBuf::from(r"..\.."));
        // A "." carries no meaning of its own and goes.
        assert_eq!(tidied(Path::new(r"a\.\b")), PathBuf::from(r"a\b"));
        // And the root is not something ".." can climb past.
        assert_eq!(tidied(Path::new(r"C:\..")), PathBuf::from(r"C:\"));
    }

    #[test]
    fn a_file_is_read_back_as_what_it_actually_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("utf8.txt");
        std::fs::write(&path, "hello\nwith a \u{fb} in it\n").unwrap();
        let path_c = c(&path.display().to_string());
        let detect = c("");

        let reply = reply_of(unsafe { rcmd_read_text(path_c.as_ptr(), 1 << 20, detect.as_ptr()) });
        assert_eq!(reply["text"], "hello\nwith a \u{fb} in it\n");
        assert_eq!(reply["encoding"], "UTF-8");
        assert_eq!(reply["truncated"], false);
        assert_eq!(reply["newline"], "LF");
    }

    #[test]
    fn a_file_too_big_to_read_says_it_was_cut_short() {
        // Silence here would be the dangerous kind: the front-end would save
        // the part it read and chop the rest of the file off.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.txt");
        std::fs::write(&path, "x".repeat(5000)).unwrap();
        let path_c = c(&path.display().to_string());
        let detect = c("");

        let reply = reply_of(unsafe { rcmd_read_text(path_c.as_ptr(), 100, detect.as_ptr()) });
        assert_eq!(reply["truncated"], true);
        assert_eq!(reply["text"].as_str().unwrap().len(), 100);

        // And the same file read whole does not claim to be cut short.
        let reply = reply_of(unsafe { rcmd_read_text(path_c.as_ptr(), 1 << 20, detect.as_ptr()) });
        assert_eq!(reply["truncated"], false);
        assert_eq!(reply["text"].as_str().unwrap().len(), 5000);
    }

    #[test]
    fn text_survives_a_round_trip_through_the_encoding_it_came_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cyrillic.txt");
        let path_c = c(&path.display().to_string());
        let written = "Привет\nмир\n";

        // Write as Windows-1251, which holds Cyrillic, then read it back with
        // no encoding forced: what comes out has to be what went in.
        let text = c(written);
        let enc = c("Windows-1251");
        let nl = c("LF");
        let reply = reply_of(unsafe {
            rcmd_write_text(path_c.as_ptr(), text.as_ptr(), enc.as_ptr(), nl.as_ptr(), 0)
        });
        assert!(reply["bytes"].as_u64().unwrap() > 0, "{reply:?}");
        assert!(reply["lost"].is_null(), "nothing should have been lost");

        // Forced back the same way, because a guess between single-byte
        // tables is exactly that.
        let detect = c("Windows-1251");
        let back = reply_of(unsafe { rcmd_read_text(path_c.as_ptr(), 1 << 20, detect.as_ptr()) });
        assert_eq!(back["text"], written);
        assert_eq!(back["encoding"], "Windows-1251");
        assert_eq!(
            back["described"], "Windows-1251",
            "no doubt when it was told"
        );
    }

    #[test]
    fn a_save_that_would_lose_characters_writes_nothing_and_says_why() {
        // The rule this exists for: silent loss on save is the one thing an
        // editor must never do. The file on disk stays the good one until
        // somebody says otherwise in as many words.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cannot-fit.txt");
        std::fs::write(&path, "the original, still good").unwrap();
        let path_c = c(&path.display().to_string());

        // A Cyrillic character has no room in Windows-1252.
        let text = c("Привет");
        let enc = c("Windows-1252");
        let nl = c("LF");

        let refused = reply_of(unsafe {
            rcmd_write_text(path_c.as_ptr(), text.as_ptr(), enc.as_ptr(), nl.as_ptr(), 0)
        });
        assert!(refused["error"].is_string(), "{refused:?}");
        assert!(
            refused["error"].as_str().unwrap().contains("Windows-1252"),
            "it should say which encoding could not hold it: {refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "the original, still good",
            "a refused save must not have touched the file"
        );

        // Allowed explicitly, it writes and still reports what went.
        let allowed = reply_of(unsafe {
            rcmd_write_text(path_c.as_ptr(), text.as_ptr(), enc.as_ptr(), nl.as_ptr(), 1)
        });
        assert!(allowed["bytes"].as_u64().unwrap() > 0, "{allowed:?}");
        assert!(
            allowed["lost"].as_str().unwrap().contains("cannot hold"),
            "a loss that was allowed still has to be reported: {allowed:?}"
        );
        assert_ne!(
            std::fs::read_to_string(&path).unwrap(),
            "the original, still good"
        );
    }

    #[test]
    fn line_endings_are_written_the_way_they_were_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endings.txt");
        let path_c = c(&path.display().to_string());
        let text = c("one\ntwo\n");
        let enc = c("UTF-8");

        let crlf = c("CRLF");
        reply_of(unsafe {
            rcmd_write_text(
                path_c.as_ptr(),
                text.as_ptr(),
                enc.as_ptr(),
                crlf.as_ptr(),
                0,
            )
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"one\r\ntwo\r\n");

        let lf = c("LF");
        reply_of(unsafe {
            rcmd_write_text(path_c.as_ptr(), text.as_ptr(), enc.as_ptr(), lf.as_ptr(), 0)
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn a_listing_says_what_each_file_looks_like() {
        // The tag crosses so a front-end can pick a picture without keeping a
        // second table of extensions that would drift from the engine's.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a-folder")).unwrap();
        std::fs::write(dir.path().join("photo.JPG"), "x").unwrap();
        std::fs::write(dir.path().join("main.rs"), "x").unwrap();
        std::fs::write(dir.path().join("mystery"), "x").unwrap();
        let path = c(&dir.path().display().to_string());
        let sort = c("name");
        let natural = c("");

        let reply =
            reply_of(unsafe { rcmd_list(path.as_ptr(), 0, sort.as_ptr(), natural.as_ptr()) });
        let of = |name: &str| -> String {
            reply["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing"))["filekind"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(of(".."), "parent");
        assert_eq!(of("a-folder"), "folder");
        assert_eq!(of("photo.JPG"), "image", "shouted extensions count too");
        assert_eq!(of("main.rs"), "code");
        assert_eq!(of("mystery"), "plain");
    }

    /// A zip with a directory in it, made by the `zip` crate the engine reads
    /// with - so this tests the crossing rather than agreeing with itself
    /// about a file format.
    fn a_zip_at(path: &Path) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut writer = ::zip::ZipWriter::new(file);
        let plain: ::zip::write::FileOptions<()> = ::zip::write::FileOptions::default();
        writer.start_file("readme.txt", plain).unwrap();
        writer.write_all(b"at the root").unwrap();
        writer.start_file("docs/one.txt", plain).unwrap();
        writer.write_all(b"inside docs").unwrap();
        writer.start_file("docs/deep/two.txt", plain).unwrap();
        writer.write_all(b"deeper still").unwrap();
        writer.finish().unwrap();
    }

    /// The sources a rename works on, as the front-end sends them.
    fn sources_json(dir: &Path, names: &[&str]) -> String {
        serde_json::to_string(
            &names
                .iter()
                .map(|name| {
                    serde_json::json!({
                        "path": dir.join(name).display().to_string(),
                        "name": name,
                        "modified": null,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn a_hex_page_is_a_window_and_not_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bytes.bin");
        // Forty bytes: two full rows of sixteen and a short one of eight.
        std::fs::write(&file, (0u8..40).collect::<Vec<u8>>()).unwrap();
        let path = c(&file.display().to_string());

        let reply = reply_of(unsafe { rcmd_hex_read(path.as_ptr(), 0, 2) });
        assert_eq!(reply["size"], 40);
        assert_eq!(reply["total_rows"], 3, "40 bytes is three rows of sixteen");
        // Two rows asked for, two rows given - the rest of the file untouched.
        assert_eq!(reply["rows"].as_array().unwrap().len(), 2);

        let first = &reply["rows"].as_array().unwrap()[0];
        // Eight digits even for a tiny file: the conventional width, and one
        // that does not change under the reader as a file grows.
        assert_eq!(first["offset"], "00000000");
        assert!(
            first["hex"].as_str().unwrap().starts_with("00 01 02"),
            "{first:?}"
        );
        // The two groups of eight, so the eye can count without counting.
        assert!(
            first["hex"].as_str().unwrap().contains("07  08"),
            "{first:?}"
        );

        // The last row is short, and the text column still lines up.
        let last = reply_of(unsafe { rcmd_hex_read(path.as_ptr(), 2, 10) });
        let rows = last["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "only one row is left: {last:?}");
        assert_eq!(rows[0]["offset"], "00000020");
        assert_eq!(
            rows[0]["hex"].as_str().unwrap().len(),
            reply["rows"].as_array().unwrap()[0]["hex"]
                .as_str()
                .unwrap()
                .len(),
            "a short row is padded to the same width"
        );
    }

    #[test]
    fn asking_past_the_end_is_empty_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.bin");
        std::fs::write(&file, [1u8, 2, 3]).unwrap();
        let path = c(&file.display().to_string());

        let reply = reply_of(unsafe { rcmd_hex_read(path.as_ptr(), 99, 4) });
        assert_eq!(reply["rows"].as_array().unwrap().len(), 0, "{reply:?}");
        // And it still says how big the file is, so a view that scrolled too
        // far can work out where to go back to.
        assert_eq!(reply["size"], 3);
    }

    #[test]
    fn text_and_binary_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        let binary = dir.path().join("thing.bin");
        std::fs::write(&text, "just some words\nand more\n").unwrap();
        // NULs are what makes it not text.
        std::fs::write(&binary, [0u8, 1, 2, 0, 255, 0]).unwrap();

        let t = c(&text.display().to_string());
        let b = c(&binary.display().to_string());
        assert_eq!(
            reply_of(unsafe { rcmd_is_binary(t.as_ptr()) })["binary"],
            false
        );
        assert_eq!(
            reply_of(unsafe { rcmd_is_binary(b.as_ptr()) })["binary"],
            true
        );
    }

    #[test]
    fn the_days_can_always_be_asked_for() {
        // Read-only, so this leaves the real account alone. No journal at all
        // has to mean an empty list rather than an error: a viewer that
        // refused to open because nothing had been recorded yet would be
        // wrong about a program that had simply not been used.
        for shown in ["all", "files", "commands", "nonsense"] {
            let shown = c(shown);
            let reply = reply_of(unsafe { rcmd_journal_days(shown.as_ptr()) });
            assert!(reply.is_array(), "{shown:?}: {reply:?}");
        }
    }

    #[test]
    fn a_day_that_is_not_a_day_is_an_error_not_an_empty_page() {
        // Otherwise a typo in a date reads as "nothing happened then", which
        // is a different thing and a worse one.
        let shown = c("all");
        let filter = c("{}");
        for bad in ["", "yesterday", "2026-13", "2026-x-01"] {
            let day = c(bad);
            let reply = reply_of(unsafe {
                rcmd_journal_read(shown.as_ptr(), day.as_ptr(), filter.as_ptr())
            });
            assert!(reply["error"].is_string(), "{bad:?}: {reply:?}");
        }
    }

    #[test]
    fn a_filter_naming_a_kind_nobody_has_heard_of_is_ignored_not_fatal() {
        // An unknown kind drops out of the filter rather than emptying the
        // page: a filter is a way of looking, and one that cannot be
        // understood should show everything rather than nothing.
        let shown = c("all");
        let day = c("2026-07-30");
        let filter = c(r#"{"kinds":["copied","teleported"],"text":"","failures_only":false}"#);
        let reply =
            reply_of(unsafe { rcmd_journal_read(shown.as_ptr(), day.as_ptr(), filter.as_ptr()) });
        assert!(reply["lines"].is_array(), "{reply:?}");
        assert!(reply["error"].is_null());
    }

    #[test]
    fn a_rename_plan_previews_without_touching_anything() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["one.txt", "two.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let sources = c(&sources_json(dir.path(), &["one.txt", "two.txt"]));
        // Keep the name, count from 1, keep the extension.
        let rules = c(r#"{"name":"shot-[C]","extension":"[E]"}"#);

        let reply = reply_of(unsafe { rcmd_rename_plan(sources.as_ptr(), rules.as_ptr()) });
        let names: Vec<&str> = reply["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["shot-1.txt", "shot-2.txt"], "{reply:?}");
        assert_eq!(reply["moving"], 2);
        assert_eq!(reply["troubled"], 0);
        // Previewing moved nothing.
        assert!(dir.path().join("one.txt").exists());
        assert!(!dir.path().join("shot-1.txt").exists());
    }

    #[test]
    fn a_plan_says_which_names_cannot_be_used_and_why() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        // Both would become the same name, which is not a rename either can do.
        let sources = c(&sources_json(dir.path(), &["a.txt", "b.txt"]));
        let rules = c(r#"{"name":"same","extension":"[E]"}"#);
        let reply = reply_of(unsafe { rcmd_rename_plan(sources.as_ptr(), rules.as_ptr()) });
        assert_eq!(reply["troubled"], 2, "{reply:?}");
        assert_eq!(reply["moving"], 0, "neither can move: {reply:?}");
        let trouble = reply["changes"].as_array().unwrap()[0]["trouble"]
            .as_str()
            .unwrap();
        assert!(trouble.contains("two files"), "{trouble}");
    }

    #[test]
    fn a_name_windows_would_quietly_change_is_refused() {
        // A trailing dot is accepted by the call and then dropped, so the file
        // is not the one that was asked for. And CON is a device, not a name.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();

        for bad in [
            r#"{"name":"CON","extension":"txt"}"#,
            r#"{"name":"trailing.","extension":""}"#,
        ] {
            let sources = c(&sources_json(dir.path(), &["a.txt"]));
            let rules = c(bad);
            let reply = reply_of(unsafe { rcmd_rename_plan(sources.as_ptr(), rules.as_ptr()) });
            if cfg!(windows) {
                assert_eq!(reply["troubled"], 1, "{bad} should be refused: {reply:?}");
            }
        }
    }

    #[test]
    fn applying_a_plan_renames_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("first.txt"), "I am first").unwrap();
        std::fs::write(dir.path().join("second.txt"), "I am second").unwrap();

        let sources = c(&sources_json(dir.path(), &["first.txt", "second.txt"]));
        let rules = c(r#"{"name":"file-[C]","extension":"[E]"}"#);
        let reply = reply_of(unsafe { rcmd_rename_apply(sources.as_ptr(), rules.as_ptr()) });

        assert_eq!(reply["renamed"], 2, "{reply:?}");
        assert_eq!(reply["failures"].as_array().map(|f| f.len()), Some(0));
        // The counter follows the order they were given in, so the contents
        // prove which file went where rather than only the names.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file-1.txt")).unwrap(),
            "I am first"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file-2.txt")).unwrap(),
            "I am second"
        );
        assert!(!dir.path().join("first.txt").exists());
    }

    #[test]
    fn a_name_already_taken_by_something_else_is_refused() {
        // Not by one of the files being renamed - those can trade names, and
        // the engine steps one aside through a temporary to let them. This is
        // a bystander, and renaming onto it would destroy it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "I was a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "I was b").unwrap();

        let sources = c(&sources_json(dir.path(), &["a.txt"]));
        let rules = c(r#"{"name":"b","extension":"[E]"}"#);
        let reply = reply_of(unsafe { rcmd_rename_plan(sources.as_ptr(), rules.as_ptr()) });

        assert_eq!(reply["troubled"], 1, "{reply:?}");
        assert_eq!(reply["moving"], 0);
        assert!(reply["changes"].as_array().unwrap()[0]["trouble"]
            .as_str()
            .unwrap()
            .contains("already exists"));
        // And the bystander is untouched, because a plan moves nothing.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "I was b"
        );
    }

    #[test]
    fn the_pair_comes_back_in_the_order_the_panes_are_on_screen() {
        // The rule that is not obvious, and the one a front-end gets wrong by
        // reaching for "the active pane first": which pane has the keyboard
        // must not decide which file is drawn on the left, or the two columns
        // swap places depending on where you last clicked.
        let left = c(r#"{"cursor":"C:\\left\\a.txt"}"#);
        let right = c(r#"{"cursor":"C:\\right\\b.txt"}"#);

        for active_is_left in [1u8, 0u8] {
            let reply = reply_of(unsafe {
                rcmd_diff_choose(left.as_ptr(), right.as_ptr(), active_is_left)
            });
            assert_eq!(
                reply["left"], r"C:\left\a.txt",
                "with active_is_left={active_is_left}: {reply:?}"
            );
            assert_eq!(reply["right"], r"C:\right\b.txt");
            assert_eq!(reply["from_one_pane"], false);
        }
    }

    #[test]
    fn two_marks_in_one_pane_beat_the_cursors() {
        // Marking two files is a deliberate act; a cursor is wherever it was
        // left. So the marks win - and still win after tabbing to the other
        // pane, which only decides whose marks are looked at first.
        let left = c(r#"{"marked":["C:\\one.txt","C:\\two.txt"],"cursor":"C:\\one.txt"}"#);
        let right = c(r#"{"cursor":"C:\\other.txt"}"#);

        let reply = reply_of(unsafe { rcmd_diff_choose(left.as_ptr(), right.as_ptr(), 1) });
        assert_eq!(reply["left"], r"C:\one.txt");
        assert_eq!(reply["right"], r"C:\two.txt");
        assert_eq!(reply["from_one_pane"], true);

        let reply = reply_of(unsafe { rcmd_diff_choose(left.as_ptr(), right.as_ptr(), 0) });
        assert_eq!(reply["left"], r"C:\one.txt", "still the marked pair");
    }

    #[test]
    fn more_than_two_marks_is_refused_rather_than_guessed_at() {
        // Falling back to the cursors here would compare two files nobody
        // pointed at, having been shown three that were.
        let left = c(r#"{"marked":["C:\\a.txt","C:\\b.txt","C:\\c.txt"],"cursor":"C:\\a.txt"}"#);
        let right = c(r#"{"cursor":"C:\\other.txt"}"#);
        let reply = reply_of(unsafe { rcmd_diff_choose(left.as_ptr(), right.as_ptr(), 1) });
        let message = reply["error"].as_str().expect("a refusal");
        assert!(message.contains('3'), "it should say how many: {message}");
    }

    #[test]
    fn the_same_file_on_both_sides_is_refused() {
        let side = c(r#"{"cursor":"C:\\same.txt"}"#);
        let reply = reply_of(unsafe { rcmd_diff_choose(side.as_ptr(), side.as_ptr(), 1) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn a_diff_lines_the_two_files_up() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, "same one\nonly on the left\nsame two\n").unwrap();
        std::fs::write(&right, "same one\nsame two\nonly on the right\n").unwrap();

        let l = c(&left.display().to_string());
        let r = c(&right.display().to_string());
        let reply = reply_of(unsafe { rcmd_diff(l.as_ptr(), r.as_ptr()) });

        assert_eq!(reply["identical"], false);
        assert_eq!(reply["changes"], 2, "{reply:?}");
        let kinds: Vec<&str> = reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"left"), "{kinds:?}");
        assert!(kinds.contains(&"right"), "{kinds:?}");
        // A row that is only on one side has nothing on the other, which is
        // what lets the view draw both columns from one list.
        let only_left = reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "left")
            .unwrap();
        assert_eq!(only_left["left_text"], "only on the left");
        assert!(only_left["right_text"].is_null());
        assert!(only_left["right_number"].is_null());
    }

    #[test]
    fn two_identical_files_are_said_to_be_identical() {
        // Rather than shown as a diff with nothing in it, which is a window
        // asking to be read for an answer it could have given in a sentence.
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("a.txt");
        let right = dir.path().join("b.txt");
        std::fs::write(&left, "one\ntwo\n").unwrap();
        std::fs::write(&right, "one\ntwo\n").unwrap();

        let l = c(&left.display().to_string());
        let r = c(&right.display().to_string());
        let reply = reply_of(unsafe { rcmd_diff(l.as_ptr(), r.as_ptr()) });
        assert_eq!(reply["identical"], true);
        assert_eq!(reply["changes"], 0);
    }

    #[test]
    fn two_binaries_are_refused_with_the_byte_they_differ_at() {
        // The refusal is the useful part: an offset is somewhere to go and
        // look, where "they differ" is not.
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("a.bin");
        let right = dir.path().join("b.bin");
        std::fs::write(&left, [0u8, 1, 2, 3, 0, 0, 9]).unwrap();
        std::fs::write(&right, [0u8, 1, 2, 3, 0, 0, 7]).unwrap();

        let l = c(&left.display().to_string());
        let r = c(&right.display().to_string());
        let reply = reply_of(unsafe { rcmd_diff(l.as_ptr(), r.as_ptr()) });
        let message = reply["error"].as_str().expect("a refusal");
        assert!(message.contains("not text"), "{message}");
        assert!(message.contains("byte 6"), "it should say where: {message}");
    }

    #[test]
    fn comparing_two_listings_marks_what_differs_on_the_right_side() {
        // The rule is the point: only-here is marked here, newer is marked on
        // the newer side, and a difference with no direction is marked on both.
        let left = c(r#"[
            {"name":"same.txt","size":10,"modified":1000,"is_dir":false},
            {"name":"newer-left.txt","size":10,"modified":2000,"is_dir":false},
            {"name":"only-left.txt","size":10,"modified":1000,"is_dir":false},
            {"name":"a-folder","size":0,"modified":1000,"is_dir":true}
        ]"#);
        let right = c(r#"[
            {"name":"same.txt","size":10,"modified":1000,"is_dir":false},
            {"name":"newer-left.txt","size":10,"modified":1000,"is_dir":false},
            {"name":"only-right.txt","size":10,"modified":1000,"is_dir":false},
            {"name":"a-folder","size":0,"modified":9999,"is_dir":true}
        ]"#);

        let reply = reply_of(unsafe { rcmd_compare(left.as_ptr(), right.as_ptr(), 0) });
        let names = |side: &str| -> Vec<String> {
            reply[side]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect()
        };
        let marked_left = names("left");
        let marked_right = names("right");

        assert!(
            marked_left.contains(&"only-left.txt".to_string()),
            "{marked_left:?}"
        );
        assert!(
            marked_left.contains(&"newer-left.txt".to_string()),
            "{marked_left:?}"
        );
        assert!(
            marked_right.contains(&"only-right.txt".to_string()),
            "{marked_right:?}"
        );
        // The one that agrees is marked nowhere.
        assert!(
            !marked_left.contains(&"same.txt".to_string()),
            "{marked_left:?}"
        );
        assert!(
            !marked_right.contains(&"same.txt".to_string()),
            "{marked_right:?}"
        );
        // And a folder is left alone even with a different date, because
        // "these two folders differ" is a question about their contents.
        assert!(
            !marked_left.contains(&"a-folder".to_string()),
            "{marked_left:?}"
        );
        assert!(
            !marked_right.contains(&"a-folder".to_string()),
            "{marked_right:?}"
        );
    }

    #[test]
    fn comparing_something_that_is_not_two_listings_is_an_error() {
        let good = c("[]");
        let rubbish = c("not a listing");
        let reply = reply_of(unsafe { rcmd_compare(good.as_ptr(), rubbish.as_ptr(), 0) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn duplicates_are_grouped_and_the_saving_added_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        // Three copies of one thing, two of another, and one of its own.
        let same = "x".repeat(400);
        std::fs::write(dir.path().join("a.bin"), &same).unwrap();
        std::fs::write(dir.path().join("b.bin"), &same).unwrap();
        std::fs::write(dir.path().join("sub/c.bin"), &same).unwrap();
        let pair = "y".repeat(300);
        std::fs::write(dir.path().join("d.bin"), &pair).unwrap();
        std::fs::write(dir.path().join("e.bin"), &pair).unwrap();
        std::fs::write(dir.path().join("alone.bin"), "z".repeat(200)).unwrap();

        let root = c(&dir.path().display().to_string());
        let scan = unsafe { rcmd_dupes_start(root.as_ptr(), 0, 1) };
        assert!(!scan.is_null());

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = Value::Null;
        while Instant::now() < deadline {
            last = reply_of(unsafe { rcmd_dupes_progress(scan, 0) });
            if last["finished"] == true {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(last["finished"], true, "never finished: {last:?}");

        assert_eq!(last["total"], 2, "two groups, not three files: {last:?}");
        // What keeping one of each and dropping the rest would give back:
        // two spare copies of 400 and one of 300.
        assert_eq!(last["reclaimable"], 400 * 2 + 300);
        // The one with no twin is in no group.
        let listed: Vec<String> = last["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["paths"].as_array().unwrap())
            .map(|p| p.as_str().unwrap().to_string())
            .collect();
        assert!(
            !listed.iter().any(|p| p.ends_with("alone.bin")),
            "{listed:?}"
        );
        // And a copy down a level counts, which is the point of walking.
        assert!(listed.iter().any(|p| p.ends_with("c.bin")), "{listed:?}");

        unsafe { rcmd_dupes_free(scan) };
    }

    #[test]
    fn properties_describe_the_file_that_was_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "some contents").unwrap();
        let path = c(&file.display().to_string());

        let reply = reply_of(unsafe { rcmd_properties(path.as_ptr()) });
        assert_eq!(reply["name"], "notes.txt");
        assert_eq!(reply["kind"], "file");
        assert_eq!(reply["size"], 13);
        assert_eq!(reply["is_symlink"], false);
        assert!(reply["modified"].is_i64(), "{reply:?}");
        // Windows has no Unix bits, and says so rather than inventing any.
        if cfg!(windows) {
            assert!(reply["mode"].is_null(), "{reply:?}");
        }
    }

    #[test]
    fn properties_of_something_that_is_not_there_is_an_error() {
        let missing = c(r"C:\no-such-place-4f21\nothing.txt");
        let reply = reply_of(unsafe { rcmd_properties(missing.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    /// Run a search to the end and hand back the last thing it said.
    fn search_to_end(root: &Path, query: &str) -> Value {
        let root = c(&root.display().to_string());
        let query = c(query);
        let search = unsafe { rcmd_find_start(root.as_ptr(), query.as_ptr()) };
        assert!(!search.is_null(), "the search should have started");

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = Value::Null;
        while Instant::now() < deadline {
            last = reply_of(unsafe { rcmd_find_progress(search, 0) });
            if last["finished"] == true {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { rcmd_find_free(search) };
        assert_eq!(last["finished"], true, "never finished: {last:?}");
        last
    }

    #[test]
    fn a_search_by_name_finds_what_matches_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("one.txt"), "x").unwrap();
        std::fs::write(dir.path().join("sub/two.txt"), "x").unwrap();
        std::fs::write(dir.path().join("photo.jpg"), "x").unwrap();

        let last = search_to_end(dir.path(), r#"{"pattern":"*.txt"}"#);
        let names: Vec<&str> = last["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap())
            .collect();
        assert_eq!(last["total"], 2, "{names:?}");
        assert!(names.contains(&"one.txt"), "{names:?}");
        // Found down a level too, which is the point of a search.
        assert!(names.contains(&"two.txt"), "{names:?}");
        assert!(!names.contains(&"photo.jpg"), "{names:?}");
        assert_eq!(last["truncated"], false);
    }

    #[test]
    fn a_search_inside_files_says_where_it_found_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "first line\nthe needle here\nlast\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing of interest\n").unwrap();

        let last = search_to_end(dir.path(), r#"{"pattern":"*.txt","contains":"needle"}"#);
        assert_eq!(last["total"], 1, "{last:?}");
        let hit = &last["hits"].as_array().unwrap()[0];
        assert_eq!(hit["name"], "a.txt");
        // The line number and the line itself, so the result is worth reading
        // rather than only worth opening.
        assert_eq!(hit["line"], 2);
        assert!(
            hit["excerpt"].as_str().unwrap().contains("needle"),
            "{hit:?}"
        );
    }

    #[test]
    fn hits_can_be_fetched_from_where_the_caller_got_to() {
        // So polling a search that has found four thousand files does not send
        // all four thousand back every time.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let root = c(&dir.path().display().to_string());
        let query = c(r#"{"pattern":"*.txt"}"#);
        let search = unsafe { rcmd_find_start(root.as_ptr(), query.as_ptr()) };
        assert!(!search.is_null());

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if reply_of(unsafe { rcmd_find_progress(search, 0) })["finished"] == true {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let all = reply_of(unsafe { rcmd_find_progress(search, 0) });
        assert_eq!(all["total"], 5);
        assert_eq!(all["hits"].as_array().unwrap().len(), 5);

        // From the third onwards: the total still says five, and two come back.
        let rest = reply_of(unsafe { rcmd_find_progress(search, 3) });
        assert_eq!(rest["total"], 5, "the total is of everything, not the tail");
        assert_eq!(rest["hits"].as_array().unwrap().len(), 2);

        // And past the end is empty rather than an error.
        let none = reply_of(unsafe { rcmd_find_progress(search, 99) });
        assert_eq!(none["hits"].as_array().unwrap().len(), 0);
        unsafe { rcmd_find_free(search) };
    }

    #[test]
    fn a_search_for_nothing_is_refused_rather_than_walking_the_disk() {
        // An empty pattern with no text to look for means "every file", which
        // is a walk of everything to report everything.
        let dir = tempfile::tempdir().unwrap();
        let root = c(&dir.path().display().to_string());
        for empty in [r#"{}"#, r#"{"pattern":""}"#, r#"{"pattern":"*"}"#] {
            let query = c(empty);
            assert!(
                unsafe { rcmd_find_start(root.as_ptr(), query.as_ptr()) }.is_null(),
                "{empty} asks for everything"
            );
        }
        // But a bare "*" with text to find is a real search.
        let query = c(r#"{"pattern":"*","contains":"something"}"#);
        let search = unsafe { rcmd_find_start(root.as_ptr(), query.as_ptr()) };
        assert!(!search.is_null(), "that one has something to look for");
        unsafe { rcmd_find_free(search) };
    }

    #[test]
    fn an_archive_walks_like_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("bundle.zip");
        a_zip_at(&zip);
        let path = c(&zip.display().to_string());
        let none = c("");

        let root = c("");
        let reply =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), root.as_ptr(), none.as_ptr()) });
        assert_eq!(reply["format"], "zip");
        assert_eq!(reply["locked"], false);
        let names: Vec<&str> = reply["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        // The walk-up row leads, as it does in a directory, so the front-end
        // gets about with the same keys.
        assert_eq!(names[0], "..");
        // `docs` is not an entry in this zip - it is worked out from the
        // members below it - and it still shows.
        assert!(names.contains(&"docs"), "{names:?}");
        assert!(names.contains(&"readme.txt"), "{names:?}");
        assert!(
            !names.contains(&"one.txt"),
            "not from a level down: {names:?}"
        );

        // One level in.
        let docs = c("docs");
        let reply =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), docs.as_ptr(), none.as_ptr()) });
        assert_eq!(reply["at"], "docs");
        let names: Vec<&str> = reply["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"one.txt"), "{names:?}");
        assert!(names.contains(&"deep"), "{names:?}");
        // And the way back out points at the root rather than nowhere.
        let up = reply["entries"].as_array().unwrap()[0].clone();
        assert_eq!(up["name"], "..");
        assert_eq!(up["path"], "");
    }

    #[test]
    fn a_file_that_is_not_an_archive_says_so_rather_than_asking_for_a_password() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "not an archive at all").unwrap();
        let path = c(&plain.display().to_string());
        let root = c("");
        let none = c("");

        let reply =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), root.as_ptr(), none.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");
        // The two password flags are the whole reason for this error shape, so
        // an unrelated failure has to leave both of them alone.
        assert_eq!(reply["needs_password"], false);
        assert_eq!(reply["wrong_password"], false);
    }

    #[test]
    fn the_same_archive_is_only_read_once() {
        // The cache has no effect anyone can see, so what is testable is that
        // it does not change the answer - and that a file changing on disk is
        // noticed rather than answered from the index of the old one.
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("bundle.zip");
        a_zip_at(&zip);
        let path = c(&zip.display().to_string());
        let root = c("");
        let none = c("");

        let first =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), root.as_ptr(), none.as_ptr()) });
        let again =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), root.as_ptr(), none.as_ptr()) });
        assert_eq!(first, again, "the cache must not change the answer");

        // Replaced with a different archive at the same path.
        std::thread::sleep(Duration::from_millis(20));
        {
            use std::io::Write;
            let file = std::fs::File::create(&zip).unwrap();
            let mut writer = ::zip::ZipWriter::new(file);
            let plain: ::zip::write::FileOptions<()> = ::zip::write::FileOptions::default();
            writer.start_file("something-else.txt", plain).unwrap();
            writer.write_all(b"different").unwrap();
            writer.finish().unwrap();
        }
        let after =
            reply_of(unsafe { rcmd_archive_list(path.as_ptr(), root.as_ptr(), none.as_ptr()) });
        let names: Vec<&str> = after["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"something-else.txt"),
            "a changed archive must be read again: {names:?}"
        );
        assert!(!names.contains(&"readme.txt"), "{names:?}");
    }

    #[test]
    fn extracting_nothing_is_refused_rather_than_started() {
        let request =
            c(r#"{"kind":"extract","archive":"x.zip","members":[],"from":"","destination":"."}"#);
        assert!(
            unsafe { rcmd_job_start(request.as_ptr()) }.is_null(),
            "an extract of no members names nothing to do"
        );
    }

    #[test]
    fn an_extract_copies_the_members_out() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("bundle.zip");
        a_zip_at(&zip);
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();

        // From one level down, so the level coming off the front is exercised:
        // "one.txt" should land directly in the destination and not under a
        // rebuilt "docs/".
        let last = run_to_end(
            &serde_json::json!({
                "kind": "extract",
                "archive": zip.display().to_string(),
                "members": ["docs/one.txt"],
                "from": "docs",
                "destination": out.display().to_string(),
            })
            .to_string(),
        );
        assert_eq!(
            last["failures"].as_array().map(|f| f.len()),
            Some(0),
            "{last:?}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("one.txt")).unwrap(),
            "inside docs"
        );
        assert!(
            !out.join("docs").exists(),
            "the level being viewed should not be rebuilt under the destination"
        );
    }

    #[test]
    fn the_tree_opens_down_to_where_the_pane_is() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("one").join("two").join("three");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir(dir.path().join("one").join("sibling")).unwrap();

        let target = c(&deep.display().to_string());
        let nothing = c("[]");
        let reply = reply_of(unsafe { rcmd_tree(target.as_ptr(), nothing.as_ptr(), 0, 0) });
        let nodes = reply["nodes"].as_array().expect("nodes");

        // Every ancestor of the target is present and open, so the pane's own
        // directory is on screen without anyone having to click down to it.
        let opened: Vec<&str> = nodes
            .iter()
            .filter(|n| n["expanded"] == true)
            .map(|n| n["label"].as_str().unwrap())
            .collect();
        assert!(opened.contains(&"one"), "{opened:?}");
        assert!(opened.contains(&"two"), "{opened:?}");
        // And the sibling is visible but shut.
        let sibling = nodes
            .iter()
            .find(|n| n["label"] == "sibling")
            .expect("the sibling should be listed");
        assert_eq!(sibling["expanded"], false);
        // Depth is what the front-end indents by, and it grows with the walk.
        let one = nodes.iter().find(|n| n["label"] == "one").unwrap();
        let two = nodes.iter().find(|n| n["label"] == "two").unwrap();
        assert!(
            two["depth"].as_u64() > one["depth"].as_u64(),
            "{one:?} {two:?}"
        );
    }

    #[test]
    fn the_tree_opens_whatever_the_front_end_says_is_open() {
        // This is what makes the boundary stateless: the caller holds the set
        // of open paths and hands it over, rather than the engine keeping a
        // tree alive between calls.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha").join("inner")).unwrap();
        std::fs::create_dir(dir.path().join("beta")).unwrap();

        let root = c(&dir.path().display().to_string());
        let shut = c("[]");
        let reply = reply_of(unsafe { rcmd_tree(root.as_ptr(), shut.as_ptr(), 0, 0) });
        assert!(
            !reply["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["label"] == "inner"),
            "nothing under alpha should show while alpha is shut"
        );

        let open =
            c(&serde_json::json!([dir.path().join("alpha").display().to_string()]).to_string());
        let reply = reply_of(unsafe { rcmd_tree(root.as_ptr(), open.as_ptr(), 0, 0) });
        assert!(
            reply["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["label"] == "inner"),
            "asking for alpha to be open should reveal what is under it"
        );
    }

    #[test]
    fn a_directory_with_nothing_under_it_says_so() {
        // So the front-end can stop drawing something to click on.
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        std::fs::write(empty.join("a-file.txt"), "x").unwrap();

        let target = c(&empty.display().to_string());
        let nothing = c("[]");
        let reply = reply_of(unsafe { rcmd_tree(target.as_ptr(), nothing.as_ptr(), 0, 0) });
        let node = reply["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["label"] == "empty")
            .expect("the target itself should be there");
        // A file inside is not a subdirectory, so this is still a leaf.
        assert_eq!(node["leaf"], true, "{node:?}");
    }

    #[test]
    fn tabs_that_would_read_the_same_take_their_parent_with_them() {
        let paths = c(r#"["C:\\work\\alpha\\src","C:\\work\\beta\\src","C:\\work\\notes"]"#);
        let reply = reply_of(unsafe { rcmd_tab_titles(paths.as_ptr()) });
        let titles: Vec<&str> = reply["titles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        // Two "src" would say nothing about which is which; "notes" is already
        // unique and stays short.
        assert_eq!(titles, ["alpha/src", "beta/src", "notes"]);
    }

    #[test]
    fn two_tabs_on_the_same_directory_stay_short() {
        // The rule worth crossing rather than guessing at: no amount of path
        // tells these apart, so the long form would only be noise.
        let paths = c(r#"["C:\\work\\src","C:\\work\\src"]"#);
        let reply = reply_of(unsafe { rcmd_tab_titles(paths.as_ptr()) });
        assert_eq!(
            reply["titles"].as_array().unwrap(),
            &vec![Value::from("src"), Value::from("src")]
        );
    }

    #[test]
    fn asking_about_no_tabs_is_an_empty_answer_not_a_failure() {
        let empty = c("[]");
        let reply = reply_of(unsafe { rcmd_tab_titles(empty.as_ptr()) });
        assert_eq!(reply["titles"].as_array().map(|t| t.len()), Some(0));

        let rubbish = c("not an array");
        let reply = reply_of(unsafe { rcmd_tab_titles(rubbish.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn a_place_says_whether_it_has_to_be_connected_first() {
        // The flag the sidebar needs in order to show a saved share without
        // pretending it can open it like a folder.
        let here = Place::from(&netloc::Location::local(r"C:\Users\me\code"));
        assert!(!here.network);
        assert_eq!(here.path, r"C:\Users\me\code");
        assert_eq!(
            here.name, "code",
            "named after the leaf, not the whole path"
        );

        let share = Place::from(&netloc::Location::parse(r"\\nas.local\media").unwrap());
        assert!(share.network, "a share cannot simply be listed");
        assert!(share.url.starts_with("smb://"), "{}", share.url);
    }

    #[test]
    fn the_places_can_always_be_read_even_with_nothing_saved() {
        // Read-only, so this leaves the real bookmarks file alone. A missing or
        // corrupt file has to mean "no bookmarks" rather than an error: a
        // sidebar that refused to draw would be worse than an empty one.
        let reply = reply_of(rcmd_places());
        assert!(reply["pinned"].is_array(), "{reply:?}");
        assert!(reply["recent"].is_array(), "{reply:?}");
        assert!(reply["error"].is_null());
    }

    #[test]
    fn a_place_with_no_path_is_refused_before_anything_is_written() {
        // Checked here rather than left to produce a bookmark named after
        // nothing, pointing nowhere, that the sidebar would then draw.
        for empty in ["", "   "] {
            let path = c(empty);
            let reply = reply_of(unsafe { rcmd_place_add(path.as_ptr(), 1) });
            assert!(reply["error"].is_string(), "{empty:?}: {reply:?}");
        }
        let reply = reply_of(unsafe { rcmd_place_add(std::ptr::null(), 1) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn unpinning_something_that_is_not_there_says_so() {
        // Rather than reporting success and leaving the front-end to redraw an
        // unchanged list, which reads as "done" when nothing happened.
        let name = c("no-such-place-bd7f21");
        let reply = reply_of(unsafe { rcmd_place_remove(name.as_ptr(), 1) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn a_pattern_picks_out_the_names_it_matches() {
        let pattern = c("*.txt");
        let names = c(r#"["notes.txt","photo.jpg","a.txt","txt","sub.txt.bak"]"#);
        let reply = reply_of(unsafe { rcmd_glob_match(pattern.as_ptr(), names.as_ptr()) });
        assert_eq!(
            reply["matched"].as_array().unwrap(),
            &vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(false),
            ]
        );
    }

    #[test]
    fn a_pattern_does_not_care_about_case() {
        // Selection by pattern wants the forgiving rule: nobody typing *.JPG
        // means to leave the lower-case ones behind.
        let pattern = c("*.JPG");
        let names = c(r#"["holiday.jpg","OTHER.JpG","notes.txt"]"#);
        let reply = reply_of(unsafe { rcmd_glob_match(pattern.as_ptr(), names.as_ptr()) });
        assert_eq!(
            reply["matched"].as_array().unwrap(),
            &vec![Value::Bool(true), Value::Bool(true), Value::Bool(false)]
        );
    }

    #[test]
    fn a_pattern_that_is_not_a_list_of_names_is_an_error_not_a_guess() {
        let pattern = c("*");
        let rubbish = c("not a json array");
        let reply = reply_of(unsafe { rcmd_glob_match(pattern.as_ptr(), rubbish.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");

        // And an empty list is an empty answer, not a failure.
        let empty = c("[]");
        let reply = reply_of(unsafe { rcmd_glob_match(pattern.as_ptr(), empty.as_ptr()) });
        assert_eq!(reply["matched"].as_array().map(|m| m.len()), Some(0));
    }

    #[test]
    fn a_file_is_renamed_where_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let before = dir.path().join("before.txt");
        std::fs::write(&before, "contents").unwrap();
        let path = c(&before.display().to_string());
        let name = c("after.txt");

        let reply = reply_of(unsafe { rcmd_rename(path.as_ptr(), name.as_ptr()) });
        assert_eq!(
            reply["path"].as_str(),
            Some(dir.path().join("after.txt").display().to_string().as_str())
        );
        assert!(!before.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("after.txt")).unwrap(),
            "contents"
        );

        // Onto a name already taken is refused rather than overwriting it.
        std::fs::write(dir.path().join("taken.txt"), "do not lose me").unwrap();
        let now = c(&dir.path().join("after.txt").display().to_string());
        let onto = c("taken.txt");
        let reply = reply_of(unsafe { rcmd_rename(now.as_ptr(), onto.as_ptr()) });
        assert!(reply["error"].is_string(), "{reply:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("taken.txt")).unwrap(),
            "do not lose me"
        );
    }

    #[test]
    fn a_copy_can_be_started_watched_and_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.txt"), vec![b'a'; 4096]).unwrap();
        std::fs::write(from.join("b.txt"), vec![b'b'; 4096]).unwrap();

        let request = c(&serde_json::json!({
            "kind": "copy",
            "sources": [
                from.join("a.txt").display().to_string(),
                from.join("b.txt").display().to_string(),
            ],
            "destination": to.display().to_string(),
        })
        .to_string());

        journal_somewhere_harmless();
        let job = unsafe { rcmd_job_start(request.as_ptr()) };
        assert!(!job.is_null(), "the copy should have started");

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = serde_json::Value::Null;
        while Instant::now() < deadline {
            last = reply_of(unsafe { rcmd_job_progress(job) });
            if last["finished"] == true {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(last["finished"], true, "never finished: {last:?}");
        assert_eq!(last["verb"], "Copying");
        assert_eq!(last["items_total"], 2);
        assert_eq!(
            last["failures"].as_array().map(|f| f.len()),
            Some(0),
            "{last:?}"
        );
        assert_eq!(last["fraction"], 1.0);

        unsafe { rcmd_job_free(job) };
        assert!(to.join("a.txt").exists());
        assert!(to.join("b.txt").exists());
    }

    /// Point the account at a temporary directory, once for the whole binary.
    ///
    /// These tests start real jobs through the real entry point, and jobs are
    /// recorded. Without this they would write what they did into the account
    /// of whoever ran the tests - a handful of copies of temp files, appearing
    /// among a person's own records as though they had done them.
    #[test]
    fn history_comes_back_here_first_and_narrows() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RCMD_JOURNAL_DIR", dir.path());
        let book = journal::Journal::at(dir.path(), journal::Keep::default());
        let here = dir.path().join("project");
        book.record(journal::Event::new(journal::Kind::Command, &here).note("cargo test -p core"));
        book.record(journal::Event::new(journal::Kind::Command, "/elsewhere").note("make"));

        let here_c = c(&here.display().to_string());
        let all = c("");
        let reply = reply_of(unsafe { rcmd_history(here_c.as_ptr(), 7, all.as_ptr(), 0) });
        let lines: Vec<&str> = reply
            .as_array()
            .expect("a list")
            .iter()
            .map(|row| row["line"].as_str().unwrap())
            .collect();
        assert_eq!(
            lines,
            vec!["cargo test -p core", "make"],
            "here first, then the rest"
        );

        // Narrowed by query, and by here_only.
        let query = c("cargo");
        let reply = reply_of(unsafe { rcmd_history(here_c.as_ptr(), 7, query.as_ptr(), 0) });
        assert_eq!(reply.as_array().unwrap().len(), 1);
        let reply = reply_of(unsafe { rcmd_history(here_c.as_ptr(), 7, all.as_ptr(), 1) });
        assert_eq!(reply.as_array().unwrap().len(), 1, "only this directory's");
        journal_somewhere_harmless_again();
    }

    #[test]
    fn the_generation_moves_when_this_process_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RCMD_JOURNAL_DIR", dir.path());
        let before = reply_of(rcmd_journal_generation())["generation"]
            .as_u64()
            .expect("a number");

        // Through the shared handle, which is the point of having one: a
        // write through any entry point is seen by every read.
        let book = account().expect("a journal");
        book.record(journal::Event::new(journal::Kind::Command, "/x").note("true"));

        let after = reply_of(rcmd_journal_generation())["generation"]
            .as_u64()
            .unwrap();
        assert!(after > before, "the account moved, so the number did");
        journal_somewhere_harmless_again();
    }

    #[test]
    fn folder_history_says_what_happened_here() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RCMD_JOURNAL_DIR", dir.path());
        let here = dir.path().join("docs");
        let book = journal::Journal::at(dir.path(), journal::Keep::default());
        book.record(
            journal::Event::new(journal::Kind::Copy, here.join("a.txt")).to("/backup/a.txt"),
        );

        let here_c = c(&here.display().to_string());
        let reply = reply_of(unsafe { rcmd_folder_history(here_c.as_ptr(), 7) });
        let rows = reply.as_array().expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "a.txt");
        assert_eq!(rows[0]["kind"], "Copied");
        journal_somewhere_harmless_again();
    }

    #[test]
    fn pins_toggle_through_the_boundary_and_land_in_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pinned.toml");
        std::env::set_var("RCMD_PINNED_PATH", &file);
        let cwd = c("/project");
        let line = c("cargo test %f");

        let reply = reply_of(unsafe { rcmd_pin_toggle(cwd.as_ptr(), line.as_ptr()) });
        assert_eq!(reply["pinned"], true);
        let reply = reply_of(unsafe { rcmd_pins(cwd.as_ptr()) });
        assert_eq!(
            reply.as_array().unwrap()[0],
            "cargo test %f",
            "as typed - the template survives"
        );
        assert!(file.exists(), "written as it happens");

        let reply = reply_of(unsafe { rcmd_pin_toggle(cwd.as_ptr(), line.as_ptr()) });
        assert_eq!(
            reply["pinned"], false,
            "said twice, it ends where it started"
        );
        std::env::remove_var("RCMD_PINNED_PATH");
    }

    #[test]
    fn a_visit_lands_in_its_own_file_and_survives_the_round_trip() {
        // The regression this guards: the recent list moved out of
        // `bookmarks.toml` into `recent.toml`, and `rcmd_place_add` kept
        // saving the old file - which skips the recent list by design. Every
        // visit was recorded into a file that does not hold visits, read
        // back as nothing, and the sidebar drew RECENT empty forever.
        let dir = tempfile::tempdir().unwrap();
        let marks = dir.path().join("bookmarks.toml");
        let recents = dir.path().join("recent.toml");
        std::env::set_var("RCMD_BOOKMARKS_PATH", &marks);
        std::env::set_var("RCMD_RECENT_PATH", &recents);

        let visited = c(r"C:\src\somewhere");
        let reply = reply_of(unsafe { rcmd_place_add(visited.as_ptr(), 0) });
        assert!(reply["error"].is_null(), "{reply:?}");
        assert!(recents.exists(), "a visit is written where visits live");

        // Read back fresh from disk, which is what every sidebar does.
        let reply = reply_of(rcmd_places());
        let recent = reply["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 1, "{reply:?}");
        assert_eq!(recent[0]["path"], r"C:\src\somewhere");

        // And forgetting it writes the same file, or the row comes back.
        let name = c("somewhere");
        let reply = reply_of(unsafe { rcmd_place_remove(name.as_ptr(), 0) });
        assert!(reply["error"].is_null(), "{reply:?}");
        let reply = reply_of(rcmd_places());
        assert_eq!(reply["recent"].as_array().unwrap().len(), 0, "{reply:?}");

        std::env::remove_var("RCMD_BOOKMARKS_PATH");
        std::env::remove_var("RCMD_RECENT_PATH");
    }

    #[test]
    fn an_undo_plan_crosses_whole_and_comes_back_to_be_applied() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RCMD_JOURNAL_DIR", dir.path());
        let was = dir.path().join("a.txt");
        let now_at = dir.path().join("moved.txt");
        std::fs::write(&now_at, "x").unwrap();
        let book = journal::Journal::at(dir.path(), journal::Keep::default());
        book.record(journal::Event::new(journal::Kind::Move, &was).to(&now_at));

        let reply = reply_of(rcmd_undo_plan());
        assert_eq!(reply["nothing"], false);
        let plan = &reply["plan"];
        assert_eq!(plan["steps"][0]["step"], "move_back");

        // The same JSON back, verbatim - what was approved is what runs.
        let plan_text = c(&plan.to_string());
        let failures = reply_of(unsafe { rcmd_undo_apply(plan_text.as_ptr()) });
        assert!(failures.as_array().unwrap().is_empty());
        assert!(was.exists() && !now_at.exists(), "back where it started");
        journal_somewhere_harmless_again();
    }

    #[test]
    fn garbage_handed_to_the_trash_is_an_error_not_a_panic() {
        let junk = c("this is not json");
        let reply = reply_of(unsafe { rcmd_trash_restore(junk.as_ptr()) });
        assert!(reply["error"]
            .as_str()
            .unwrap()
            .contains("not a trash item"));
        let reply = reply_of(unsafe { rcmd_trash_purge(junk.as_ptr()) });
        assert!(reply["error"].is_string());
    }

    #[test]
    fn a_session_round_trips_through_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("workspaces.toml");
        std::env::set_var("RCMD_SESSION_PATH", &file);

        let session = serde_json::json!({
            "at": 0,
            "workspaces": [{
                "name": "build",
                "left": "C:\\src",
                "right": "C:\\backup",
                "show_right": true,
                "left_view": "tree",
                "right_view": "list",
                "half": "both",
                "active": "left",
                "split": 0.4,
                "synced": true,
                "shell": "C:\\src",
                "shell_program": "pwsh"
            }]
        });
        let text = c(&session.to_string());
        let reply = reply_of(unsafe { rcmd_session_save(text.as_ptr()) });
        assert_eq!(reply["ok"], true);

        let read = reply_of(rcmd_session_read());
        assert_eq!(read["workspaces"][0]["name"], "build");
        assert_eq!(
            read["workspaces"][0]["shell_program"], "pwsh",
            "restored means the kind too"
        );
        std::env::remove_var("RCMD_SESSION_PATH");
    }

    #[test]
    fn placeholders_expand_with_the_engine_s_own_quoting() {
        let line = c("tar cf out.tar %s && cp %f %d");
        let file = c("cursor.rs");
        let marked = c("[\"a name.txt\",\"plain.rs\"]");
        let other = c("/backup");
        let reply = reply_of(unsafe {
            rcmd_expand_command(
                line.as_ptr(),
                file.as_ptr(),
                marked.as_ptr(),
                other.as_ptr(),
            )
        });
        let expanded = reply["expanded"].as_str().unwrap();
        assert!(expanded.contains("plain.rs"), "{expanded}");
        assert!(!expanded.contains("%s"), "{expanded}");

        let with = c("echo %f");
        let without = c("echo 100%x");
        assert_eq!(
            reply_of(unsafe { rcmd_has_placeholders(with.as_ptr()) })["placeholders"],
            true
        );
        assert_eq!(
            reply_of(unsafe { rcmd_has_placeholders(without.as_ptr()) })["placeholders"],
            false
        );
    }

    #[test]
    fn the_shells_are_listed_with_their_honesty_flag() {
        let reply = reply_of(rcmd_shells());
        let shells = reply.as_array().expect("a list");
        assert!(!shells.is_empty(), "some shell exists everywhere");
        for shell in shells {
            assert!(shell["program"].is_string());
            assert!(shell["journaled"].is_boolean());
        }
    }

    #[test]
    fn a_cd_for_wsl_speaks_wsl_through_the_boundary() {
        let program = c("wsl.exe");
        let path = c("C:\\src\\x");
        let reply = reply_of(unsafe { rcmd_cd_command(program.as_ptr(), path.as_ptr()) });
        assert_eq!(reply["line"], "cd '/mnt/c/src/x'");
    }

    /// Put the shared journal back somewhere harmless for whatever test the
    /// harness runs next on this thread.
    fn journal_somewhere_harmless_again() {
        let dir = std::env::temp_dir().join("rcmd-ffi-test-journal");
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("RCMD_JOURNAL_DIR", &dir);
    }

    fn journal_somewhere_harmless() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join("rcmd-ffi-test-journal");
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("RCMD_JOURNAL_DIR", &dir);
        });
    }

    /// Run a job to completion and hand back the last thing it said.
    fn run_to_end(request: &str) -> Value {
        journal_somewhere_harmless();
        let request = c(request);
        let job = unsafe { rcmd_job_start(request.as_ptr()) };
        assert!(!job.is_null(), "the job should have started: {request:?}");

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = Value::Null;
        while Instant::now() < deadline {
            last = reply_of(unsafe { rcmd_job_progress(job) });
            if last["finished"] == true {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { rcmd_job_free(job) };
        assert_eq!(last["finished"], true, "never finished: {last:?}");
        last
    }

    #[test]
    fn a_move_takes_the_file_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let source = from.join("moved.txt");
        std::fs::write(&source, "carried across").unwrap();

        let last = run_to_end(
            &serde_json::json!({
                "kind": "move",
                "sources": [source.display().to_string()],
                "destination": to.display().to_string(),
            })
            .to_string(),
        );

        assert_eq!(last["verb"], "Moving");
        assert_eq!(last["failures"].as_array().map(|f| f.len()), Some(0));
        // A move is not a copy: the original is gone.
        assert!(!source.exists(), "the source should have been taken away");
        assert_eq!(
            std::fs::read_to_string(to.join("moved.txt")).unwrap(),
            "carried across"
        );
    }

    #[test]
    fn a_delete_to_the_trash_takes_the_file_off_the_disk() {
        // Into the system's own trash, so this really does leave the pane -
        // which is what the front-end will re-read to find out.
        let dir = tempfile::tempdir().unwrap();
        let doomed = dir.path().join("goodbye.txt");
        std::fs::write(&doomed, "x").unwrap();

        let last = run_to_end(
            &serde_json::json!({
                "kind": "delete",
                "targets": [doomed.display().to_string()],
                "to_trash": true,
            })
            .to_string(),
        );

        assert_eq!(
            last["failures"].as_array().map(|f| f.len()),
            Some(0),
            "{last:?}"
        );
        assert!(!doomed.exists(), "it should be gone from here");
    }

    #[test]
    fn a_job_over_nothing_is_refused_rather_than_started() {
        // A job over an empty list finishes instantly and reports success,
        // which reads as "it worked" when what happened is that nothing was
        // selected. Refusing it puts that back on the front-end to explain.
        for empty in [
            r#"{"kind":"copy","sources":[],"destination":"."}"#,
            r#"{"kind":"move","sources":[],"destination":"."}"#,
            r#"{"kind":"delete","targets":[],"to_trash":true}"#,
        ] {
            let request = c(empty);
            assert!(
                unsafe { rcmd_job_start(request.as_ptr()) }.is_null(),
                "{empty} names nothing to do"
            );
        }
    }

    #[test]
    fn a_request_that_cannot_be_read_starts_nothing() {
        for rubbish in [
            "not json at all",
            r#"{"kind":"incinerate","targets":["x"]}"#,
            r#"{"sources":["x"],"destination":"."}"#, // no kind
            r#"{"kind":"delete","targets":["x"]}"#,   // no to_trash: which is it?
            "{}",
        ] {
            let request = c(rubbish);
            assert!(
                unsafe { rcmd_job_start(request.as_ptr()) }.is_null(),
                "{rubbish} should not have started anything"
            );
        }
    }

    #[test]
    fn asking_about_no_job_says_so_instead_of_reading_the_pointer() {
        let reply = reply_of(unsafe { rcmd_job_progress(std::ptr::null_mut()) });
        assert!(reply["error"].is_string(), "{reply:?}");
    }

    #[test]
    fn freeing_nothing_is_harmless() {
        // The front-end will do this on a path where the start failed.
        unsafe { rcmd_job_free(std::ptr::null_mut()) };
        unsafe { rcmd_string_free(std::ptr::null_mut()) };
        unsafe { rcmd_job_cancel(std::ptr::null_mut()) };
    }

    /// Poll until `wanted` shows up on the screen, or give up.
    ///
    /// A shell starting, printing a prompt and answering a command is several
    /// round trips through a pty and a real process; waiting for the text is
    /// the only honest way to test it.
    fn term_settle(term: *mut RcmdTerm, wanted: &str) -> serde_json::Value {
        let mut last = serde_json::Value::Null;
        for _ in 0..200 {
            let reply = reply_of(unsafe { rcmd_term_poll(term, 0) });
            let text: String = reply["lines"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .flat_map(|row| row["runs"].as_array().unwrap().iter())
                        .filter_map(|run| run["text"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default();
            if text.contains(wanted) {
                return reply;
            }
            last = reply;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        last
    }

    /// A real PowerShell, hooked, reporting what it ran.
    ///
    /// Windows only and slow - it starts a shell and waits for a prompt -
    /// but the hook is a script running inside somebody else's interpreter,
    /// and the only test worth having is the one that runs it.
    #[test]
    #[cfg(windows)]
    fn powershell_reports_what_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let shell = c("powershell.exe");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 100) };
        if term.is_null() {
            // No PowerShell on this machine: not a failure of the hook.
            return;
        }

        // Wait for the prompt before typing. Typing into a shell that is
        // still starting leaves half the line in the banner and the rest on
        // a continuation prompt, and nothing ever runs.
        let _ = term_settle(term, "PS ");
        std::thread::sleep(std::time::Duration::from_millis(600));

        let line = c("Write-Output rcmd-ps-marker\r");
        let _ = reply_of(unsafe { rcmd_term_write(term, line.as_ptr()) });
        let _ = term_settle(term, "rcmd-ps-marker");

        // The prompt after the command is what emits the marks, so the
        // command has to have finished and the prompt drawn again.
        let mut ran = Vec::new();
        for _ in 0..100 {
            let term_ref = unsafe { &*term };
            ran = term_ref.session.take_commands();
            if !ran.is_empty() {
                break;
            }
            let _ = reply_of(unsafe { rcmd_term_poll(term, 0) });
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // What is actually on screen, so a failure says why rather than that.
        let seen = reply_of(unsafe { rcmd_term_poll(term, 0) });
        let shown: String = seen["lines"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        row["runs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|r| r["text"].as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    )
            })
            .unwrap_or_default();
        unsafe { rcmd_term_free(term) };

        // The screen goes in the message: a hook is a script running inside
        // somebody else's interpreter, and "reported nothing" without it
        // could be a parse error, a shell still starting, or a mark written
        // as plain text - all of which this has been.
        assert!(
            !ran.is_empty(),
            "the hook reported nothing. Screen:
{shown}"
        );
        assert!(
            ran.iter().any(|r| r.line.contains("rcmd-ps-marker")),
            "{:?}",
            ran.iter().map(|r| &r.line).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_shell_starts_and_answers_through_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let shell = c("");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 80) };
        assert!(!term.is_null(), "the shell should start");

        // Something no prompt would print by accident.
        let line = c("echo rcmd-marker-7\r");
        let _ = reply_of(unsafe { rcmd_term_write(term, line.as_ptr()) });

        let reply = term_settle(term, "rcmd-marker-7");
        let rows = reply["lines"].as_array().expect("rows");
        let text: String = rows
            .iter()
            .flat_map(|row| row["runs"].as_array().unwrap().iter())
            .filter_map(|run| run["text"].as_str())
            .collect();
        assert!(text.contains("rcmd-marker-7"), "{text}");

        // The screen crossed as runs, not as cells: an 80x24 screen showing a
        // prompt and one line is a handful of runs, nowhere near 1,920.
        let runs: usize = rows
            .iter()
            .map(|row| row["runs"].as_array().unwrap().len())
            .sum();
        assert!(runs < 100, "a nearly empty screen came to {runs} runs");

        unsafe { rcmd_term_free(term) };
    }

    #[test]
    fn an_unchanged_screen_answers_with_the_sequence_and_no_grid() {
        let dir = tempfile::tempdir().unwrap();
        let shell = c("");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 80) };
        assert!(!term.is_null());

        let line = c("echo rcmd-quiet\r");
        let _ = reply_of(unsafe { rcmd_term_write(term, line.as_ptr()) });
        let settled = term_settle(term, "rcmd-quiet");
        let seq = settled["seq"].as_u64().expect("a sequence");

        // Asking again with the sequence just seen: the whole point of the
        // number. An idle shell polled thirty times a second must not ship a
        // screen thirty times a second.
        let again = reply_of(unsafe { rcmd_term_poll(term, seq) });
        assert_eq!(again["seq"], seq);
        assert!(again["lines"].is_null(), "an unchanged screen sent a grid");

        unsafe { rcmd_term_free(term) };
    }

    #[test]
    fn a_named_key_is_the_terminals_business_and_says_when_it_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let shell = c("");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 80) };
        assert!(!term.is_null());

        let up = c("Up");
        let reply = reply_of(unsafe { rcmd_term_key(term, up.as_ptr(), 0, 0) });
        assert_eq!(reply["sent"], true);

        // Ctrl with a letter is that letter's control code.
        let c_key = c("c");
        let reply = reply_of(unsafe { rcmd_term_key(term, c_key.as_ptr(), 1, 0) });
        assert_eq!(reply["sent"], true);

        // And a key this does not know falls through, so the front-end can
        // give it to the file manager instead of swallowing it.
        let f5 = c("F5");
        let reply = reply_of(unsafe { rcmd_term_key(term, f5.as_ptr(), 0, 0) });
        assert_eq!(reply["sent"], false);

        unsafe { rcmd_term_free(term) };
    }

    #[test]
    fn a_recording_says_where_it_went_and_how_much_it_caught() {
        let dir = tempfile::tempdir().unwrap();
        let shell = c("");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 80) };
        assert!(!term.is_null());

        let file = dir.path().join("session.log");
        let target = c(&file.display().to_string());
        let reply = reply_of(unsafe { rcmd_term_record(term, target.as_ptr()) });
        assert!(reply["recording"].is_string(), "{reply:?}");

        // The poll says a recording is running, which is what lets the panel
        // show it without a second thing to ask.
        let seen = reply_of(unsafe { rcmd_term_poll(term, 0) });
        assert!(seen["recording"].is_string(), "{seen:?}");

        let line = c("echo rcmd-recorded
");
        let _ = reply_of(unsafe { rcmd_term_write(term, line.as_ptr()) });
        let _ = term_settle(term, "rcmd-recorded");

        let done = reply_of(unsafe { rcmd_term_stop_record(term) });
        assert!(done["path"].is_string(), "{done:?}");
        unsafe { rcmd_term_free(term) };

        let written = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(written.contains("rcmd-recorded"), "{written}");
    }

    #[test]
    fn a_transcript_is_named_by_the_engine_so_both_front_ends_agree() {
        let title = c("cmd.exe /C \"weird: name\"");
        let stamp = c("20260730-120000");
        let reply = reply_of(unsafe { rcmd_term_transcript_name(title.as_ptr(), stamp.as_ptr()) });
        let name = reply["name"].as_str().expect("a name");
        assert!(name.contains("20260730-120000"), "{name}");
        // Nothing a filesystem would refuse.
        assert!(
            !name.contains(['/', '\\', ':', '"', '?', '*', '<', '>', '|']),
            "{name}"
        );
    }

    #[test]
    fn a_command_a_shell_ran_lands_in_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let book = tempfile::tempdir().unwrap();
        // Never the reader's own account: these tests write records, and a
        // test that polluted it would be a test that lied to its user.
        std::env::set_var("RCMD_JOURNAL_DIR", book.path());

        let shell = c("");
        let where_ = c(&dir.path().display().to_string());
        let term = unsafe { rcmd_term_open(shell.as_ptr(), where_.as_ptr(), 24, 80) };
        assert!(!term.is_null());

        let ran = c(r#"{"line":"cargo build","cwd":"C:\\src","code":1,"ms":1234}"#);
        let reply = reply_of(unsafe { rcmd_term_journal(term, ran.as_ptr()) });
        assert_eq!(reply["ok"], true, "{reply:?}");

        // Read it back the way the account reads it, rather than trusting the
        // write: the whole point is that it shows up beside the copies.
        let shown = c("commands");
        let days = reply_of(unsafe { rcmd_journal_days(shown.as_ptr()) });
        let day = days[0]["name"].as_str().expect("a day").to_string();
        let day = c(&day);
        let filter = c(r#"{"kinds":[],"failures_only":false,"text":""}"#);
        let page =
            reply_of(unsafe { rcmd_journal_read(shown.as_ptr(), day.as_ptr(), filter.as_ptr()) });
        let lines = page["lines"].as_array().expect("lines");
        // The note carries the command and the path carries the directory -
        // the same shape every other writer produces, which is what lets
        // `commands_before` and the history column read entries from any
        // front-end without knowing who wrote them.
        let line = lines
            .iter()
            .find(|l| l["note"] == "cargo build")
            .unwrap_or_else(|| panic!("the command should be in the account: {lines:?}"));

        assert_eq!(line["kind"], "Command");
        assert_eq!(line["text"], "C:\\src", "the path is where it ran");
        // And a non-zero exit marks it failed with the code as the reason,
        // as the egui front-end files its own.
        assert_eq!(line["failed"], "exit 1", "{line:?}");
        assert_eq!(line["took_ms"], 1234);

        unsafe { rcmd_term_free(term) };
        std::env::remove_var("RCMD_JOURNAL_DIR");
    }

    #[test]
    fn a_markdown_file_comes_back_as_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("README.md");
        std::fs::write(&path, "# Title\n\nSome **bold** words.\n\n- one\n- two\n").unwrap();

        let target = c(&path.display().to_string());
        let reply = reply_of(unsafe { rcmd_markdown_read(target.as_ptr(), 1 << 20) });
        assert_eq!(reply["truncated"], false);
        let blocks = reply["blocks"].as_array().expect("blocks");

        assert_eq!(blocks[0]["kind"], "heading");
        assert_eq!(blocks[0]["level"], 1);
        assert_eq!(blocks[0]["runs"][0]["text"], "Title");

        // The styling crosses as runs, so the front-end draws and does not
        // parse: "Some ", "bold", " words." with the middle one strong.
        let paragraph = &blocks[1];
        assert_eq!(paragraph["kind"], "paragraph");
        let strong = paragraph["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["style"] == "strong")
            .expect("a strong run");
        assert_eq!(strong["text"], "bold");

        let items: Vec<&serde_json::Value> =
            blocks.iter().filter(|b| b["kind"] == "list_item").collect();
        assert_eq!(items.len(), 2, "{blocks:?}");
    }

    #[test]
    fn a_document_cut_short_says_so_and_stays_valid_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.md");
        // Multi-byte characters right where the cut lands, so a byte-wise
        // slice would produce something that is not text.
        std::fs::write(&path, "æ".repeat(200)).unwrap();

        let target = c(&path.display().to_string());
        let reply = reply_of(unsafe { rcmd_markdown_read(target.as_ptr(), 51) });
        assert_eq!(reply["truncated"], true, "{reply:?}");
        assert_eq!(reply["size"], 400);
        // Whole characters only: no replacement character from a split one.
        let text: String = reply["blocks"][0]["runs"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !text.contains('\u{fffd}'),
            "cut through a character: {text}"
        );
    }

    #[test]
    fn the_schemes_cross_with_their_colours_and_their_ground() {
        let themes = reply_of(rcmd_themes());
        let themes = themes.as_array().expect("a list");
        let names: Vec<&str> = themes.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"Norton Commander"), "{names:?}");
        assert!(names.contains(&"XTree Gold"), "{names:?}");

        let nc = themes
            .iter()
            .find(|t| t["name"] == "Norton Commander")
            .unwrap();
        assert_eq!(nc["bg"], "#00009c");
        assert_eq!(nc["accent"], "#f5f55a");
        // Whether the ground is dark crosses too: on Windows it decides which
        // set of system control colours to start from, and a front-end
        // guessing would get black on black in everything it does not paint.
        assert_eq!(nc["dark"], true);
        assert!(
            themes.iter().any(|t| t["dark"] == false),
            "one light scheme"
        );
    }

    #[test]
    fn settings_round_trip_and_a_partial_save_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        std::env::set_var("RCMD_SETTINGS_PATH", &file);

        // Save two fields.
        let both = c(r#"{"theme":"dark","pane_split":0.6}"#);
        let reply = reply_of(unsafe { rcmd_settings_save(both.as_ptr()) });
        assert_eq!(reply["ok"], true, "{reply:?}");

        // Save one. The other must survive: the whole reason the save is
        // read-modify-write against the file.
        let just_height = c(r#"{"shell_height":340.0}"#);
        let reply = reply_of(unsafe { rcmd_settings_save(just_height.as_ptr()) });
        assert_eq!(reply["ok"], true, "{reply:?}");

        let read = reply_of(rcmd_settings_read());
        assert_eq!(read["theme"], "dark");
        // Through an f32 and back, so compared as one: 0.6 has no exact
        // binary form and the widened f64 shows the difference.
        assert!((read["pane_split"].as_f64().unwrap() - 0.6).abs() < 1e-6);
        assert_eq!(read["shell_height"].as_f64().unwrap(), 340.0);

        // And what this writes is the same file the engine reads, fields the
        // FFI does not know about included.
        let toml = std::fs::read_to_string(&file).unwrap();
        assert!(toml.contains("theme"), "{toml}");

        std::env::remove_var("RCMD_SETTINGS_PATH");
    }

    #[test]
    fn a_split_dragged_off_the_edge_is_clamped_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        std::env::set_var("RCMD_SETTINGS_PATH", &file);

        let silly = c(r#"{"pane_split":0.001}"#);
        let _ = reply_of(unsafe { rcmd_settings_save(silly.as_ptr()) });
        let read = reply_of(rcmd_settings_read());
        // A split saved at nothing would open the next session with a pane
        // nobody can see and no way to know why.
        assert!(read["pane_split"].as_f64().unwrap() >= 0.1);

        std::env::remove_var("RCMD_SETTINGS_PATH");
    }

    #[test]
    fn markdown_in_hand_parses_without_a_file_behind_it() {
        // What an editor needs: writing to a file to find out what the text
        // looks like would be an editor that saved on every keystroke.
        let text = c("## Half

written *so far*");
        let reply = reply_of(unsafe { rcmd_markdown_parse(text.as_ptr()) });
        let blocks = reply["blocks"].as_array().expect("blocks");
        assert_eq!(blocks[0]["kind"], "heading");
        assert_eq!(blocks[0]["level"], 2);
        assert_eq!(reply["truncated"], false);

        // And the empty document is empty rather than an error: an editor
        // starts there.
        let nothing = c("");
        let reply = reply_of(unsafe { rcmd_markdown_parse(nothing.as_ptr()) });
        assert_eq!(reply["blocks"].as_array().unwrap().len(), 0);
        assert!(reply["error"].is_null());
    }

    #[test]
    fn a_markdown_file_that_is_not_there_says_so_rather_than_looking_empty() {
        let missing = c("no-such-file.md");
        let reply = reply_of(unsafe { rcmd_markdown_read(missing.as_ptr(), 1 << 20) });
        assert!(reply["error"].is_string(), "{reply:?}");
        assert_eq!(reply["blocks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn which_names_the_markdown_viewer_answers_for() {
        assert_eq!(unsafe { rcmd_is_markdown(c("README.md").as_ptr()) }, 1);
        assert_eq!(unsafe { rcmd_is_markdown(c("NOTES.MARKDOWN").as_ptr()) }, 1);
        assert_eq!(unsafe { rcmd_is_markdown(c("main.rs").as_ptr()) }, 0);
        assert_eq!(unsafe { rcmd_is_markdown(std::ptr::null()) }, 0);
    }

    #[test]
    fn an_administrator_shell_is_a_new_window_on_windows() {
        let here = c("C:\\src");
        let reply = reply_of(unsafe { rcmd_root_shell(here.as_ptr()) });

        if cfg!(windows) {
            // A command, not a line to type: an elevated process cannot
            // inherit this one's pty, so it gets a console of its own - which
            // is exactly why it cannot be a tab in the drawer.
            assert_eq!(reply["kind"], "command", "{reply:?}");
            assert!(reply["program"].as_str().unwrap().contains("powershell"));
            assert!(!reply["args"].as_array().unwrap().is_empty());
        } else {
            // Elsewhere it is a line to type, because sudo needs a terminal
            // for its password prompt to appear on.
            assert_eq!(reply["kind"], "shell", "{reply:?}");
            assert!(reply["line"].as_str().unwrap().contains("sudo"));
        }
    }

    #[test]
    fn a_terminal_that_is_gone_says_so_instead_of_reading_the_pointer() {
        let reply = reply_of(unsafe { rcmd_term_poll(std::ptr::null_mut(), 0) });
        assert!(reply["error"].is_string(), "{reply:?}");
        // And freeing nothing is harmless, which the front-end relies on for
        // the path where opening failed.
        unsafe { rcmd_term_free(std::ptr::null_mut()) };
    }

    #[test]
    fn typing_hex_replaces_one_half_of_the_byte_at_a_time() {
        // 4f is unreachable in one keystroke - that is the whole point of
        // tracking which half the cursor is on.
        let reply = reply_of(rcmd_hex_type(0x00, '4' as u32, 0, 0));
        assert_eq!(reply["byte"], 0x40);
        // The high half was typed, so the low half comes next and the cursor
        // has not moved off this byte yet.
        assert_eq!(reply["advance"], false);
        assert_eq!(reply["low"], true);

        let reply = reply_of(rcmd_hex_type(0x40, 'f' as u32, 1, 0));
        assert_eq!(reply["byte"], 0x4f);
        assert_eq!(reply["advance"], true);
        assert_eq!(reply["low"], false);
    }

    #[test]
    fn typing_in_the_text_column_is_a_whole_byte_a_keystroke() {
        let reply = reply_of(rcmd_hex_type(0x00, 'J' as u32, 0, 1));
        assert_eq!(reply["byte"], 0x4a);
        assert_eq!(reply["advance"], true);
    }

    #[test]
    fn a_key_that_is_not_for_this_editor_says_so() {
        // Not a hex digit.
        let reply = reply_of(rcmd_hex_type(0x00, 'z' as u32, 0, 0));
        assert_eq!(reply["none"], true, "{reply:?}");

        // And in the text column, something the column could not show: it
        // would be drawn as a dot, and storing a byte the reader did not mean
        // is worse than ignoring the key.
        let reply = reply_of(rcmd_hex_type(0x00, '\u{e9}' as u32, 0, 1));
        assert_eq!(reply["none"], true, "{reply:?}");
    }

    #[test]
    fn hex_edits_overwrite_and_the_file_stays_exactly_as_long() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, b"Hello, world!").unwrap();

        let target = c(&path.display().to_string());
        // H -> J and w -> W: two bytes, far apart.
        let edits = c(r#"[{"at":0,"was":72,"now":74},{"at":7,"was":119,"now":87}]"#);
        let reply = reply_of(unsafe { rcmd_hex_write(target.as_ptr(), edits.as_ptr()) });
        assert_eq!(reply["written"], 2, "{reply:?}");

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"Jello, World!");
        // The invariant: exactly as long as it was.
        assert_eq!(bytes.len(), 13);
    }

    #[test]
    fn a_file_changed_underneath_the_editor_is_refused_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, b"Hello").unwrap();

        let target = c(&path.display().to_string());
        // The editor read 'H' at 0 - but claims 'X' was at 1, which is a
        // stand-in for the file having changed since. The first edit is
        // correct; it must not be written either.
        let edits = c(r#"[{"at":0,"was":72,"now":74},{"at":1,"was":88,"now":89}]"#);
        let reply = reply_of(unsafe { rcmd_hex_write(target.as_ptr(), edits.as_ptr()) });
        let said = reply["error"].as_str().expect("a refusal");
        assert!(said.contains("changed underneath"), "{said}");

        // Nothing was written - not even the edit whose `was` still held.
        assert_eq!(std::fs::read(&path).unwrap(), b"Hello");
    }

    #[test]
    fn an_edit_past_the_end_is_a_refusal_not_an_insert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, b"Hi").unwrap();

        let target = c(&path.display().to_string());
        let edits = c(r#"[{"at":10,"was":0,"now":65}]"#);
        let reply = reply_of(unsafe { rcmd_hex_write(target.as_ptr(), edits.as_ptr()) });
        let said = reply["error"].as_str().expect("a refusal");
        assert!(said.contains("past its end"), "{said}");
        assert_eq!(std::fs::read(&path).unwrap(), b"Hi");
    }

    #[test]
    fn no_edits_is_a_quiet_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, b"Hi").unwrap();
        let target = c(&path.display().to_string());
        let edits = c("[]");
        let reply = reply_of(unsafe { rcmd_hex_write(target.as_ptr(), edits.as_ptr()) });
        assert_eq!(reply["written"], 0);
    }

    #[test]
    fn a_quarter_turn_swaps_the_sides() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(1920, 1080, 1, 0, 0, 0, 0, 0, 0, c(".jpg").as_ptr(), 0, 0)
        });
        assert_eq!(reply["width"], 1080);
        assert_eq!(reply["height"], 1920);
        assert_eq!(reply["lossy"], true);
    }

    #[test]
    fn four_quarter_turns_are_none_at_all() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(1920, 1080, 4, 0, 0, 0, 0, 0, 0, c(".png").as_ptr(), 0, 0)
        });
        // The whole reason turns are counted rather than remembered one at a
        // time: five rotates are one rotation of the original, not five
        // resamplings of a picture that has already been rotated four times.
        assert_eq!(reply["width"], 1920);
        assert_eq!(reply["height"], 1080);
        assert_eq!(reply["lossy"], false);
    }

    #[test]
    fn an_ico_says_no_before_the_work_rather_than_after_it() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(1920, 1080, 0, 0, 0, 0, 0, 0, 0, c("ico").as_ptr(), 0, 0)
        });
        let said = reply["refuses"].as_str().expect("a refusal");
        assert!(said.contains("256"), "{said}");
        assert!(said.contains("1920x1080"), "{said}");

        // ...and takes it once it fits, which is the point of asking early.
        let reply = reply_of(unsafe {
            rcmd_image_plan(1920, 1080, 0, 0, 0, 0, 0, 128, 72, c("ico").as_ptr(), 0, 0)
        });
        assert!(reply["refuses"].is_null(), "{reply:?}");
    }

    #[test]
    fn what_a_save_would_leave_behind_is_said_out_loud() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(64, 64, 0, 0, 0, 0, 0, 0, 0, c("gif").as_ptr(), 1, 1)
        });
        let losses = reply["losses"].as_array().expect("losses");
        assert_eq!(losses.len(), 2, "{losses:?}");
        assert!(losses
            .iter()
            .any(|l| l.as_str().unwrap().contains("animation")));
        assert!(losses.iter().any(|l| l.as_str().unwrap().contains("EXIF")));

        // A file that carries neither has nothing to warn about.
        let reply = reply_of(unsafe {
            rcmd_image_plan(64, 64, 0, 0, 0, 0, 0, 0, 0, c("gif").as_ptr(), 0, 0)
        });
        assert_eq!(reply["losses"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_crop_changes_what_the_plan_comes_to() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(
                1920,
                1080,
                0,
                100,
                100,
                300,
                200,
                0,
                0,
                c("png").as_ptr(),
                0,
                0,
            )
        });
        assert_eq!(reply["width"], 300);
        assert_eq!(reply["height"], 200);

        // And the turn turns the *cropped* size, which is the order the
        // engine's Edit applies things in: crop first, then the transform.
        let reply = reply_of(unsafe {
            rcmd_image_plan(
                1920,
                1080,
                1,
                100,
                100,
                300,
                200,
                0,
                0,
                c("png").as_ptr(),
                0,
                0,
            )
        });
        assert_eq!(reply["width"], 200);
        assert_eq!(reply["height"], 300);
    }

    #[test]
    fn a_cropped_picture_can_become_an_ico_the_whole_one_could_not() {
        let reply = reply_of(unsafe {
            rcmd_image_plan(1920, 1080, 0, 0, 0, 200, 200, 0, 0, c("ico").as_ptr(), 0, 0)
        });
        assert!(reply["refuses"].is_null(), "{reply:?}");
    }

    #[test]
    fn a_drag_on_the_untouched_picture_is_just_unprojected() {
        // The picture is 100x100 source pixels drawn at 200x200 on screen from
        // (10,10): every screen unit is half a pixel.
        let reply = reply_of(rcmd_image_pick_crop(
            50.0, 50.0, 150.0, 130.0, 10.0, 10.0, 200.0, 200.0, 0, 0, 0, 0, 0, 0, 0, 100, 100,
        ));
        assert_eq!(reply["x"], 20);
        assert_eq!(reply["y"], 20);
        assert_eq!(reply["width"], 50);
        assert_eq!(reply["height"], 40);
    }

    #[test]
    fn a_drag_on_a_turned_picture_is_folded_back_to_the_source() {
        // Source 100x50, turned right, drawn 1:1 at the origin: the screen
        // shows 50 across and 100 down. A rectangle dragged at the screen's
        // top-right came from the source's bottom-right... which under one
        // clockwise turn was the source's *top-right* before turning - the
        // exact arithmetic nobody should write twice.
        let reply = reply_of(rcmd_image_pick_crop(
            30.0, 0.0, 50.0, 30.0, 0.0, 0.0, 50.0, 100.0, 0, 0, 0, 0, 1, 0, 0, 100, 50,
        ));
        // Undoing Turn::Right: x = dragged.y, y = H - (dragged.x + w).
        assert_eq!(reply["x"], 0);
        assert_eq!(reply["y"], 0);
        assert_eq!(reply["width"], 30);
        assert_eq!(reply["height"], 20);
    }

    #[test]
    fn a_stray_click_cannot_crop_a_picture_to_nothing() {
        let reply = reply_of(rcmd_image_pick_crop(
            40.0, 40.0, 40.0, 40.0, 0.0, 0.0, 100.0, 100.0, 0, 0, 0, 0, 0, 0, 0, 100, 100,
        ));
        assert_eq!(reply["none"], true, "{reply:?}");
    }

    #[test]
    fn the_other_side_of_a_resize_follows_the_shape() {
        let reply = reply_of(rcmd_image_fit(1920, 1080, 960, 0, 1));
        assert_eq!(reply["height"], 540);

        let reply = reply_of(rcmd_image_fit(1920, 1080, 0, 540, 0));
        assert_eq!(reply["width"], 960);
    }

    #[test]
    fn a_picture_never_resizes_away_to_nothing() {
        // A one-pixel-high result is still a picture; a zero-high one is a
        // file no decoder will open.
        let reply = reply_of(rcmd_image_fit(1920, 1080, 1, 0, 1));
        assert_eq!(reply["height"], 1);
    }

    #[test]
    fn a_tree_carrying_files_hands_back_rows_a_listing_could_use() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let target = c(&dir.path().display().to_string());
        let open = c(&serde_json::to_string(&[dir.path().display().to_string()]).unwrap());
        let reply = reply_of(unsafe { rcmd_tree(target.as_ptr(), open.as_ptr(), 0, 1) });
        let nodes = reply["nodes"].as_array().expect("nodes");

        let file = nodes
            .iter()
            .find(|n| n["label"] == "notes.txt")
            .expect("the file should be in the tree");

        // Every field a listing row has, so the front-end can mark it, sort it
        // into a selection and hand it to a copy without a second row type.
        assert_eq!(file["name"], "notes.txt");
        assert_eq!(file["kind"], "file");
        assert_eq!(file["is_dir"], false);
        assert_eq!(file["size"], 5);
        assert!(file["modified"].is_i64(), "{file:?}");
        // Classified the same way the listing classifies it, from the name.
        assert_eq!(file["filekind"], "document");
        // And never offered a twisty, because there is nothing under a file.
        assert_eq!(file["leaf"], true);

        let sub = nodes.iter().find(|n| n["label"] == "sub").expect("the dir");
        assert_eq!(sub["kind"], "dir");
        assert_eq!(sub["filekind"], "folder");
        // Zero the way a listing shows it: what a directory comes to is a
        // question the scan answers, not one a stat does.
        assert_eq!(sub["size"], 0);
    }

    #[test]
    fn without_the_flag_the_tree_is_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let target = c(&dir.path().display().to_string());
        let open = c(&serde_json::to_string(&[dir.path().display().to_string()]).unwrap());
        let reply = reply_of(unsafe { rcmd_tree(target.as_ptr(), open.as_ptr(), 0, 0) });
        let nodes = reply["nodes"].as_array().expect("nodes");

        assert!(nodes.iter().any(|n| n["label"] == "sub"));
        assert!(
            !nodes.iter().any(|n| n["label"] == "notes.txt"),
            "the classic tree is a way of getting somewhere, not of working"
        );
    }

    #[test]
    fn the_version_comes_back_so_a_wrong_dll_shows_up_at_once() {
        let reply = reply_of(rcmd_version());
        assert_eq!(reply["version"], env!("CARGO_PKG_VERSION"));
    }
}
