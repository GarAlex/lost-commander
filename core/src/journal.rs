// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An account of what was done to the files.
//!
//! Every file manager tells you what it is *about* to do. Almost none of them
//! can tell you what it *did* - and "which of the four hundred files did the
//! copy actually skip", asked an hour later, is a question that otherwise has
//! no answer at all.
//!
//! # The shape on disk
//!
//! One file per day, one record per line, appended and never rewritten:
//!
//! ```text
//! ~/.config/lost-commander/journal/files-2026-07-28.jsonl
//! ~/.config/lost-commander/journal/shell-2026-07-28.jsonl
//! ```
//!
//! That shape falls out of what it has to do. Browsing by date is opening one
//! file. Keeping thirty days is deleting the files older than thirty days -
//! no compaction, no rewriting, no chance of losing the good records while
//! pruning the old ones. Appending is the only write, so a run that is killed
//! halfway leaves everything it had already recorded.
//!
//! Shell commands are a second stream rather than another kind in the first,
//! because they arrive in a different order of magnitude: a build can run
//! twenty commands a minute, and mixed together they would bury the file
//! operations they were meant to sit beside.
//!
//! # Two rules
//!
//! **Recording never fails an operation.** A journal that cannot be written -
//! read-only home, full disk, no config directory at all - must not stop a
//! copy or lose a file. Every write here swallows its errors on purpose.
//!
//! **A record is what happened, not what was asked for.** Entries are written
//! after the fact, carry the failure when there was one, and are never
//! rewritten afterwards.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Which stream a record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stream {
    /// What happened to files and directories.
    #[default]
    Files,
    /// Commands handed to a shell: the line and how it ended, never output.
    Shell,
}

impl Stream {
    pub fn prefix(self) -> &'static str {
        match self {
            Stream::Files => "files",
            Stream::Shell => "shell",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Stream::Files => "Files",
            Stream::Shell => "Commands",
        }
    }
}

/// What the browser is showing.
///
/// Storage has two streams because commands arrive in a different order of
/// magnitude from file operations and mixed together they would bury them.
/// Reading is under no such obligation: "what was I doing at four o'clock" is
/// a question that needs both, and a copy followed by the `make` that consumed
/// it is one story told in two files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shown {
    /// Both, in the order things happened.
    #[default]
    All,
    Files,
    Commands,
}

/// The views, in the order they are offered.
pub const SHOWN: [Shown; 3] = [Shown::All, Shown::Files, Shown::Commands];

impl Shown {
    pub fn label(self) -> &'static str {
        match self {
            Shown::All => "All",
            Shown::Files => "Files",
            Shown::Commands => "Commands",
        }
    }

    /// The stored streams this view reads.
    pub fn streams(self) -> &'static [Stream] {
        match self {
            Shown::All => &[Stream::Files, Stream::Shell],
            Shown::Files => &[Stream::Files],
            Shown::Commands => &[Stream::Shell],
        }
    }

    /// Whether a kind can appear here, which is what the filter row offers.
    pub fn holds(self, kind: Kind) -> bool {
        self.streams().contains(&kind.stream())
    }

    /// The next view round, for the terminal front-end's one key.
    pub fn next(self) -> Shown {
        let at = SHOWN.iter().position(|&s| s == self).unwrap_or(0);
        SHOWN[(at + 1) % SHOWN.len()]
    }
}

/// What sort of thing was done.
///
/// The list the filter row offers, so it is deliberately short: these are the
/// distinctions someone looking back actually draws. "Was it deleted or was it
/// trashed" is one of them, because one is recoverable and the other is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Copy,
    Move,
    Rename,
    /// Gone for good.
    Delete,
    /// Sent to the trash, where it can be fetched back.
    Trash,
    MakeDir,
    Permissions,
    /// Contents changed: text, bytes, or a picture.
    Edit,
    /// Handed to another program to open. Not a change - but the most this
    /// program can honestly say about one made outside it.
    Open,
    /// A command handed to a shell.
    Command,
    /// Something about a shell session itself rather than a command in it:
    /// most importantly, that its commands are *not* being recorded.
    Session,
}

/// Every kind, in the order the filter offers them.
pub const KINDS: [Kind; 11] = [
    Kind::Copy,
    Kind::Move,
    Kind::Rename,
    Kind::Delete,
    Kind::Trash,
    Kind::MakeDir,
    Kind::Permissions,
    Kind::Edit,
    Kind::Open,
    Kind::Command,
    Kind::Session,
];

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Copy => "Copied",
            Kind::Move => "Moved",
            Kind::Rename => "Renamed",
            Kind::Delete => "Deleted",
            Kind::Trash => "Trashed",
            Kind::MakeDir => "Created",
            Kind::Permissions => "Permissions",
            Kind::Edit => "Edited",
            Kind::Open => "Opened",
            Kind::Command => "Command",
            Kind::Session => "Shell",
        }
    }

    /// Which stream a kind is written to.
    ///
    /// Everything about shells goes to the second stream, because commands
    /// arrive in a different order of magnitude from file operations and
    /// mixed together they would bury them.
    pub fn stream(self) -> Stream {
        match self {
            Kind::Command | Kind::Session => Stream::Shell,
            _ => Stream::Files,
        }
    }

    /// The one-word form for a filter chip.
    pub fn short(self) -> &'static str {
        match self {
            Kind::Copy => "copy",
            Kind::Move => "move",
            Kind::Rename => "rename",
            Kind::Delete => "delete",
            Kind::Trash => "trash",
            Kind::MakeDir => "mkdir",
            Kind::Permissions => "perms",
            Kind::Edit => "edit",
            Kind::Open => "open",
            Kind::Command => "command",
            Kind::Session => "shell",
        }
    }

    /// Whether this kind is worth marking as dangerous when it is listed.
    pub fn is_destructive(self) -> bool {
        matches!(self, Kind::Delete)
    }
}

/// One thing that happened to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds. Stored as a number so a record written by one version
    /// reads back the same in the next, whatever the date library does.
    pub at: i64,
    pub kind: Kind,
    /// What was acted on.
    pub path: String,
    /// Where it ended up, for the operations that move something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The detail that makes the line worth reading: `644 -> 755`,
    /// `UTF-8 -> Windows-1251`, `exit 1`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Why it did not happen. `None` means it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Which run this belonged to, for the operations that touch many files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
    /// Which shell ran it: `bash`, `zsh`, `fish`. Only the shell kinds have
    /// one, and it is worth having because the answer to "why did that behave
    /// oddly" is often "that was the other shell".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// How long it took, in milliseconds.
    ///
    /// Commands only. A file operation's duration is noise - sixteen bytes
    /// copied in under a millisecond, recorded four hundred times - and what
    /// is actually wanted there is the total for the *run*, which is on
    /// [`Done`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

impl Event {
    pub fn new(kind: Kind, path: impl AsRef<Path>) -> Event {
        Event {
            at: now(),
            kind,
            path: shown(path.as_ref()),
            to: None,
            note: String::new(),
            failed: None,
            group: None,
            shell: None,
            ms: None,
        }
    }

    /// Which shell ran it.
    pub fn by(mut self, shell: impl Into<String>) -> Event {
        let shell = shell.into();
        if !shell.is_empty() {
            self.shell = Some(shell);
        }
        self
    }

    /// How long it took.
    pub fn lasting(mut self, ms: u64) -> Event {
        self.ms = Some(ms);
        self
    }

    /// What to call this in the kind column.
    ///
    /// The shell's name where there is one: in a list of commands, "Command"
    /// on every line says nothing, and which shell ran it is exactly the
    /// thing that is not otherwise recoverable.
    pub fn label(&self) -> &str {
        match &self.shell {
            Some(shell) => shell,
            None => self.kind.label(),
        }
    }

    pub fn to(mut self, to: impl AsRef<Path>) -> Event {
        self.to = Some(shown(to.as_ref()));
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Event {
        self.note = note.into();
        self
    }

    pub fn failed(mut self, why: impl Into<String>) -> Event {
        self.failed = Some(why.into());
        self
    }

    /// Mark it failed only when it was, which keeps the call sites free of an
    /// `if` around a builder chain.
    pub fn failed_if(self, failed: bool, why: impl Into<String>) -> Event {
        match failed {
            true => self.failed(why),
            false => self,
        }
    }

    pub fn in_group(mut self, group: u64) -> Event {
        self.group = Some(group);
        self
    }

    pub fn is_failure(&self) -> bool {
        self.failed.is_some()
    }
}

/// A run that touched more than one file: a copy of a selection, a
/// synchronize, a multi-rename.
///
/// Written **before** its events, so a run killed halfway still has a heading
/// over whatever it managed - and the browser can say "42 files, and here is
/// where it stopped" rather than showing forty-two loose lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: u64,
    pub at: i64,
    pub kind: Kind,
    /// What the run was, in one line: "Copy 42 items to /backup".
    pub summary: String,
}

/// A run that ended, and how long it took altogether.
///
/// Separate from [`Group`] because the heading is written *before* the work
/// starts - so that a run killed halfway still has one - and at that point
/// there is nothing to say about how long it took. A run with no `Done` is a
/// run that did not reach its end, which is worth being able to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub id: u64,
    pub at: i64,
    /// Milliseconds from the first file to the last.
    pub ms: u64,
}

/// One line of a day file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
pub enum Record {
    Group(Group),
    Event(Event),
    Done(Done),
}

impl Record {
    /// When it happened, whichever kind of record it is.
    pub fn at(&self) -> i64 {
        match self {
            Record::Group(group) => group.at,
            Record::Event(event) => event.at,
            Record::Done(done) => done.at,
        }
    }
}

/// The most events one run will write.
///
/// A copy of a hundred thousand files should not become a hundred thousand
/// lines nobody will read and a day file nobody can open. Past this the run
/// keeps counting but stops naming, and the browser says how many went
/// unnamed - which is the honest version of stopping.
pub const MAX_EVENTS_PER_GROUP: usize = 5_000;

/// How long something took, for a person reading it back.
///
/// Precision falls away as the number grows, which is the way it is actually
/// read: milliseconds matter for a command that felt instant, and nobody
/// wants three decimal places on a twenty-minute build.
pub fn took(ms: u64) -> String {
    let seconds = ms / 1000;
    if ms == 0 {
        // Not "no time at all" - under the resolution of the measurement,
        // which is a different claim and the true one.
        "<1ms".to_string()
    } else if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{}.{}s", seconds, (ms % 1000) / 100)
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds / 60) % 60)
    }
}

/// Unix seconds, now.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A path as the journal stores it.
///
/// Lossy, deliberately. A path on Unix is bytes and need not be UTF-8, but
/// this is a record for a person to read rather than a script to replay, and
/// a readable approximation beats an unreadable exactness.
fn shown(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// A calendar day, as the file names carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Day {
    pub fn today() -> Day {
        Day::of(chrono::Local::now())
    }

    fn of(when: chrono::DateTime<chrono::Local>) -> Day {
        use chrono::Datelike;
        Day {
            year: when.year(),
            month: when.month(),
            day: when.day(),
        }
    }

    /// The day a unix timestamp falls on, in local time - which is the only
    /// answer that matches what someone remembers doing.
    pub fn of_time(at: i64) -> Day {
        use chrono::TimeZone;
        match chrono::Local.timestamp_opt(at, 0).single() {
            Some(when) => Day::of(when),
            None => Day::today(),
        }
    }

    pub fn name(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Parse the date out of a file name's `prefix-YYYY-MM-DD.jsonl`.
    pub fn from_file_name(name: &str, stream: Stream) -> Option<Day> {
        let rest = name.strip_prefix(stream.prefix())?.strip_prefix('-')?;
        let date = rest.strip_suffix(".jsonl")?;
        let mut parts = date.split('-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(Day { year, month, day })
    }

    /// How many days before `later` this is. Negative if it is after.
    pub fn days_before(&self, later: Day) -> i64 {
        later.to_ordinal() - self.to_ordinal()
    }

    /// A day number, for subtracting one date from another. The civil-from-days
    /// algorithm, run backwards.
    fn to_ordinal(self) -> i64 {
        let (year, month) = if self.month <= 2 {
            (self.year as i64 - 1, self.month as i64 + 9)
        } else {
            (self.year as i64, self.month as i64 - 3)
        };
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let day_of_year = (153 * month + 2) / 5 + self.day as i64 - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// `28 July 2026`, for a heading.
    pub fn describe(&self) -> String {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let month = MONTHS
            .get(self.month.saturating_sub(1) as usize)
            .copied()
            .unwrap_or("?");
        format!("{} {month} {}", self.day, self.year)
    }
}

/// The clock time of an event, `14:32:05`, for the line it is drawn on.
pub fn clock(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(at, 0).single() {
        Some(when) => {
            use chrono::Timelike;
            format!(
                "{:02}:{:02}:{:02}",
                when.hour(),
                when.minute(),
                when.second()
            )
        }
        None => "??:??:??".to_string(),
    }
}

/// How long records are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep(pub u32);

impl Default for Keep {
    fn default() -> Self {
        Keep(30)
    }
}

impl Keep {
    /// Zero means keep everything: a retention setting that silently deleted
    /// the lot at its lowest value would be a trap, and "forever" is a real
    /// answer someone wants.
    pub fn forever(&self) -> bool {
        self.0 == 0
    }

    pub fn describe(&self) -> String {
        match self.0 {
            0 => "for ever".to_string(),
            1 => "for a day".to_string(),
            days => format!("for {days} days"),
        }
    }
}

/// Where the records are kept, and the only thing that writes them.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
    pub keep: Keep,
    /// The account, in memory: one slot per stream, filled the first time a
    /// stream is asked for and appended to on every write from then on.
    ///
    /// Shared by every clone rather than owned by each, because a clone
    /// crosses to the worker thread that records a copy - a cache the worker
    /// could not reach would be stale before the copy finished. The files on
    /// disk are unchanged by any of this; the cache is only ever what the
    /// files would say.
    cache: std::sync::Arc<[std::sync::Mutex<Option<Vec<Record>>>; 2]>,
    /// Moved forward on every change to the account, so a front-end can ask
    /// "anything new?" once a frame instead of re-reading to find out. Which
    /// stream changed is not said: the question is asked at human speed and
    /// a spare re-filter is cheaper than a second counter.
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Stream {
    fn slot(self) -> usize {
        match self {
            Stream::Files => 0,
            Stream::Shell => 1,
        }
    }
}

impl Journal {
    /// Beside the settings file, which is the one directory this program
    /// already owns on every platform.
    pub fn default_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("lost-commander").join("journal"))
    }

    pub fn at(dir: impl Into<PathBuf>, keep: Keep) -> Journal {
        Journal {
            dir: dir.into(),
            keep,
            cache: std::sync::Arc::new([std::sync::Mutex::new(None), std::sync::Mutex::new(None)]),
            generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many times the account has changed since this journal was made.
    ///
    /// A front-end that redraws every frame keeps its filtered view of the
    /// history by comparing this, which turns "re-read once a second in case
    /// something happened" into "re-filter when something did".
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn touched(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Run `f` over every record of a stream, oldest first, from memory.
    ///
    /// The first call reads the stream's day files once; every later call is
    /// the memory, kept in step by [`Journal::write`]. A borrow rather than a
    /// clone, because the whole point is not to copy the account per query.
    pub fn with_records<R>(&self, stream: Stream, f: impl FnOnce(&[Record]) -> R) -> R {
        let Ok(mut slot) = self.cache[stream.slot()].lock() else {
            // A panic while the lock was held. The files are still the
            // truth, so answer from them rather than not at all.
            return f(&self.read_everything(stream));
        };
        if slot.is_none() {
            *slot = Some(self.read_everything(stream));
        }
        f(slot.as_deref().unwrap_or_default())
    }

    /// Every day file of a stream, oldest first.
    fn read_everything(&self, stream: Stream) -> Vec<Record> {
        let mut records = Vec::new();
        for day in self.days(stream).into_iter().rev() {
            records.extend(self.read(stream, day));
        }
        records
    }

    /// Forget the memory, for the operations that rewrite the files.
    fn drop_cache(&self) {
        for slot in self.cache.iter() {
            if let Ok(mut slot) = slot.lock() {
                *slot = None;
            }
        }
        self.touched();
    }

    fn file(&self, stream: Stream, day: Day) -> PathBuf {
        self.dir
            .join(format!("{}-{}.jsonl", stream.prefix(), day.name()))
    }

    /// Append one record. Never fails, and never says so.
    ///
    /// A journal that could stop a copy would be worse than no journal: the
    /// whole point is to be a record of the work, not a participant in it.
    pub fn write(&self, stream: Stream, record: &Record) {
        let at = match record {
            Record::Group(group) => group.at,
            Record::Event(event) => event.at,
            Record::Done(done) => done.at,
        };
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.file(stream, Day::of_time(at));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
        // Into the memory as well - even when the disk refused, because the
        // thing this records *happened*, and the session should be able to
        // answer for itself either way.
        if let Ok(mut slot) = self.cache[stream.slot()].lock() {
            if let Some(records) = slot.as_mut() {
                records.push(record.clone());
            }
        }
        self.touched();
    }

    pub fn record(&self, event: Event) {
        self.write(event.kind.stream(), &Record::Event(event));
    }

    pub fn open_group(&self, group: Group) {
        self.write(Stream::Files, &Record::Group(group));
    }

    /// Say that a run reached its end, and how long it took.
    pub fn close_group(&self, id: u64, ms: u64) {
        self.write(Stream::Files, &Record::Done(Done { id, at: now(), ms }));
    }

    /// Every day that has records, newest first.
    pub fn days(&self, stream: Stream) -> Vec<Day> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut days: Vec<Day> = entries
            .flatten()
            .filter_map(|entry| Day::from_file_name(&entry.file_name().to_string_lossy(), stream))
            .collect();
        days.sort_unstable();
        days.reverse();
        days
    }

    /// Everything recorded on one day.
    ///
    /// A line that will not parse is skipped rather than failing the read: a
    /// half-written last line - the program killed mid-append - must not cost
    /// the rest of the day.
    pub fn read(&self, stream: Stream, day: Day) -> Vec<Record> {
        let Ok(text) = std::fs::read_to_string(self.file(stream, day)) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Everything one view holds for one day.
    ///
    /// Reading both files and letting [`arrange`] sort the result is all
    /// "All" needs: it already orders by when things happened and matches a
    /// run's files to it by identifier rather than by adjacency.
    pub fn read_shown(&self, shown: Shown, day: Day) -> Vec<Record> {
        shown
            .streams()
            .iter()
            .flat_map(|stream| self.read(*stream, day))
            .collect()
    }

    /// Every day this view has anything on, newest first.
    pub fn days_shown(&self, shown: Shown) -> Vec<Day> {
        let mut days: Vec<Day> = shown
            .streams()
            .iter()
            .flat_map(|stream| self.days(*stream))
            .collect();
        days.sort_unstable();
        days.dedup();
        days.reverse();
        days
    }

    /// Delete the day files older than the retention setting.
    ///
    /// Returns how many files went, so the caller can say so.
    pub fn sweep(&self, today: Day) -> usize {
        // The files are about to change under the memory, so the memory goes:
        // the next question re-reads what is actually there.
        self.drop_cache();
        if self.keep.forever() {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut swept = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let day = Day::from_file_name(&name, Stream::Files)
                .or_else(|| Day::from_file_name(&name, Stream::Shell));
            let Some(day) = day else { continue };
            if day.days_before(today) >= self.keep.0 as i64
                && std::fs::remove_file(entry.path()).is_ok()
            {
                swept += 1;
            }
        }
        swept
    }

    /// Throw the lot away. Only the journal's own files, never anything else
    /// that happens to be in the directory.
    pub fn clear(&self) -> usize {
        self.drop_cache();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let ours = Day::from_file_name(&name, Stream::Files).is_some()
                || Day::from_file_name(&name, Stream::Shell).is_some();
            if ours && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// What the browser shows: a run and what it did, or a single thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A run, with everything recorded under it.
    ///
    /// `took` is `None` for a run that never reached its end - killed, or
    /// still going - which is a difference worth being able to see.
    Run {
        group: Group,
        events: Vec<Event>,
        took: Option<u64>,
    },
    /// Something that stood on its own.
    One(Event),
}

impl Row {
    pub fn at(&self) -> i64 {
        match self {
            Row::Run { group, .. } => group.at,
            Row::One(event) => event.at,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Row::Run { group, .. } => group.kind,
            Row::One(event) => event.kind,
        }
    }

    pub fn failures(&self) -> usize {
        match self {
            Row::Run { events, .. } => events.iter().filter(|e| e.is_failure()).count(),
            Row::One(event) => usize::from(event.is_failure()),
        }
    }

    /// How many files this row accounts for.
    pub fn items(&self) -> usize {
        match self {
            Row::Run { events, .. } => events.len(),
            Row::One(_) => 1,
        }
    }

    /// How long it took, where that is a thing worth saying.
    ///
    /// A run has a total; a single file operation does not - sixteen bytes
    /// copied in under a millisecond is not a fact about anything. A command
    /// does, because waiting for one is most of what using a shell is.
    pub fn took(&self) -> Option<u64> {
        match self {
            Row::Run { took, .. } => *took,
            Row::One(event) => event.ms,
        }
    }
}

/// Turn a day's records into rows, newest first.
///
/// Events that name a group go under it; events that do not stand alone. An
/// event whose group is missing from the file - the heading fell on the other
/// side of midnight - is kept as its own row rather than dropped, because a
/// record that exists must not vanish because its heading did.
pub fn arrange(records: Vec<Record>) -> Vec<Row> {
    let mut groups: Vec<Group> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    let mut done: Vec<Done> = Vec::new();
    for record in records {
        match record {
            Record::Group(group) => groups.push(group),
            Record::Event(event) => events.push(event),
            Record::Done(one) => done.push(one),
        }
    }

    let mut rows: Vec<Row> = groups
        .iter()
        .map(|group| Row::Run {
            took: done.iter().find(|d| d.id == group.id).map(|d| d.ms),
            group: group.clone(),
            events: Vec::new(),
        })
        .collect();

    for event in events {
        let placed = event.group.and_then(|id| {
            rows.iter_mut().position(|row| match row {
                Row::Run { group, .. } => group.id == id,
                Row::One(_) => false,
            })
        });
        match placed {
            Some(at) => {
                if let Row::Run { events, .. } = &mut rows[at] {
                    events.push(event);
                }
            }
            None => rows.push(Row::One(event)),
        }
    }

    rows.sort_by_key(|row| std::cmp::Reverse(row.at()));
    rows
}

/// What the browser is showing: which kinds, and whether only the failures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Empty means everything, which is what an untouched filter row means.
    pub kinds: Vec<Kind>,
    pub failures_only: bool,
    /// Matched against the paths, case-insensitively.
    pub text: String,
}

impl Filter {
    pub fn is_open(&self) -> bool {
        self.kinds.is_empty() && !self.failures_only && self.text.trim().is_empty()
    }

    pub fn toggle(&mut self, kind: Kind) {
        match self.kinds.iter().position(|k| *k == kind) {
            Some(at) => {
                self.kinds.remove(at);
            }
            None => self.kinds.push(kind),
        }
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }

    fn allows_kind(&self, kind: Kind) -> bool {
        self.kinds.is_empty() || self.kinds.contains(&kind)
    }

    /// Whether an entry matches what was typed in the search box.
    ///
    /// Everything the line *shows* is searched, not the path alone. That is
    /// the rule that makes the box predictable: what is on screen is what can
    /// be found. It also happens to be the only rule that answers the
    /// questions worth asking of an account - "when did I run anything with
    /// `--force` in it", "what did I open in the image editor", "what did I
    /// convert to Windows-1251" - none of which are questions about a path.
    fn matches_text(&self, event: &Event) -> bool {
        let needle = self.text.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let anywhere = [
            Some(event.path.as_str()),
            event.to.as_deref(),
            Some(event.note.as_str()),
            event.failed.as_deref(),
            event.shell.as_deref(),
        ];
        anywhere
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(&needle))
            // The kind as it is written in the column, so "opened" and
            // "renamed" find what they say.
            || event.label().to_lowercase().contains(&needle)
    }

    /// The same question of a run's heading.
    fn heading_matches(&self, group: &Group) -> bool {
        let needle = self.text.trim().to_lowercase();
        needle.is_empty() || group.summary.to_lowercase().contains(&needle)
    }

    /// Keep the rows the filter allows, trimming a run's events to those that
    /// match rather than dropping the whole run.
    ///
    /// A run whose events all fail the text filter is dropped; a run with one
    /// matching file is kept, showing that one - which is what "find where
    /// that file went" needs.
    pub fn apply(&self, rows: Vec<Row>) -> Vec<Row> {
        rows.into_iter()
            .filter_map(|row| match row {
                Row::One(event) => {
                    let keep = self.allows_kind(event.kind)
                        && (!self.failures_only || event.is_failure())
                        && self.matches_text(&event);
                    keep.then_some(Row::One(event))
                }
                Row::Run {
                    group,
                    events,
                    took,
                } => {
                    if !self.allows_kind(group.kind) {
                        return None;
                    }
                    // A run whose *heading* matches keeps all of its files.
                    // Searching for "backup" and finding "Copy 42 items to
                    // /backup" should show the forty-two, not hide them
                    // because their own paths say nothing about a backup.
                    let by_heading = self.heading_matches(&group);
                    let kept: Vec<Event> = events
                        .into_iter()
                        .filter(|event| !self.failures_only || event.is_failure())
                        .filter(|event| by_heading || self.matches_text(event))
                        .collect();
                    // A run that recorded nothing at all is still worth
                    // showing under an open filter - it is the record that the
                    // run happened.
                    if kept.is_empty() && !self.is_open() {
                        return None;
                    }
                    Some(Row::Run {
                        group,
                        events: kept,
                        took,
                    })
                }
            })
            .collect()
    }
}

/// One drawable line of the account.
///
/// The graphical view can fold a run away behind a triangle; a terminal list
/// is one flat column, so there a run is its heading followed by its files
/// indented under it. Indices rather than borrows, so the list can be built
/// once while the rows stay owned by whoever owns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A run's heading.
    Heading { row: usize },
    /// One file, belonging to the run above it.
    Under { row: usize, event: usize },
    /// One file that stood on its own.
    Alone { row: usize },
}

impl Line {
    pub fn row(&self) -> usize {
        match self {
            Line::Heading { row } | Line::Under { row, .. } | Line::Alone { row } => *row,
        }
    }

    pub fn is_heading(&self) -> bool {
        matches!(self, Line::Heading { .. })
    }
}

/// Flatten rows into lines, headings and all.
/// A command that was run, as much as is needed to offer it again.
///
/// Deliberately not [`crate::shellhook::Ran`], which carries an exit code and
/// a duration: those come from the hook, and a shell without one - `cmd`,
/// which is the machine's own answer on Windows - reports neither. Filling
/// them with zeroes would say every command succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Past {
    pub line: String,
    /// Where it ran. The same words in another directory are about other
    /// work.
    pub cwd: PathBuf,
}

/// Commands run before now, newest first, ready to be offered back.
///
/// Ordered by what a reader is most likely to want: the ones run *here*
/// before the ones run anywhere else. That ordering is the whole point of
/// recording where a command ran - `cargo test` in this directory and
/// `cargo test` in another are the same words about different work, and the
/// one you want is nearly always the one from where you are standing.
///
/// Deduplicated by line, keeping the newest, because a history that offers
/// the same command four times makes you press the key four times to get
/// past it.
///
/// Works whatever shell is running. The line is known before it is handed
/// over, so this needs no hook - which matters, because the machine's own
/// shell on Windows has none.
/// The tail of a stream that is at most this many days old.
///
/// A slice rather than a new list: records come out of [`Journal::with_records`]
/// oldest first, so "the last week" is everything after one cut, found by
/// binary search because the order is already chronological.
pub fn since(records: &[Record], days: i64) -> &[Record] {
    let today = Day::today();
    let start =
        records.partition_point(|record| Day::of_time(record.at()).days_before(today) >= days);
    &records[start..]
}

pub fn commands_before(records: &[Record], here: &Path) -> Vec<Past> {
    let mut here_first: Vec<Past> = Vec::new();
    let mut elsewhere: Vec<Past> = Vec::new();

    for record in records.iter().rev() {
        let Record::Event(event) = record else {
            continue;
        };
        if event.kind != Kind::Command || event.note.trim().is_empty() {
            continue;
        }
        let ran = Past {
            line: event.note.clone(),
            cwd: PathBuf::from(&event.path),
        };
        if Path::new(&event.path) == here {
            &mut here_first
        } else {
            &mut elsewhere
        }
        .push(ran);
    }

    here_first.append(&mut elsewhere);
    let mut seen: Vec<String> = Vec::new();
    here_first.retain(|ran| {
        if seen.contains(&ran.line) {
            return false;
        }
        seen.push(ran.line.clone());
        true
    });
    here_first
}

/// Where the panels would have to be to see what an entry describes.
///
/// A record says what happened and to what; this says where to stand to look
/// at it now. For a copy or a move that is two directories - the one it came
/// from and the one it went to - which is the question anybody actually asks
/// afterwards: what do both ends look like now?
///
/// Directories, not the files themselves: a panel shows a directory, and the
/// file may well not be there any more. Whether either still exists is the
/// caller's question - a path may have been deleted since, or be on a disk
/// nobody has mounted today - and this deliberately does not ask, so it stays
/// a pure function of the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub left: PathBuf,
    /// The other end, for the operations that have one.
    pub right: Option<PathBuf>,
}

/// The directory a recorded path was in.
///
/// A command records the directory it ran in and has no parent to take; for
/// everything else the record names a file and the panel wants the directory
/// holding it.
fn directory_of(kind: Kind, path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    if kind == Kind::Command || kind == Kind::MakeDir {
        return Some(path);
    }
    match path.parent() {
        // A path with no parent is a root, and a root is a directory.
        None => Some(path),
        Some(parent) if parent.as_os_str().is_empty() => Some(path),
        Some(parent) => Some(parent.to_path_buf()),
    }
}

/// One thing that was done to something living in a directory.
///
/// The account read the other way round: the journal screen asks "what did I
/// do today", and this asks "what has been done to the things *here*" - which
/// is the question you have standing in a folder wondering why it looks like
/// this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Happening {
    pub at: i64,
    pub kind: Kind,
    /// The name inside this directory that it was about.
    pub name: String,
    /// The other end, when there is one and it is somewhere else: where a
    /// copy came from, or where a move went.
    pub other: Option<PathBuf>,
    /// Set when this end is where the thing *arrived*, so a row can say
    /// "from" rather than "to".
    pub incoming: bool,
    /// What it became, when the other end has a different name: a rename, or
    /// a copy that was given a new name. Without this a rename reads as the
    /// old name alone, which is the half of it nobody needs.
    pub became: Option<String>,
    pub note: String,
    pub failed: Option<String>,
}

/// The directory a path lives in - not the one it *is*.
///
/// Unlike [`directory_of`], a made directory belongs to its parent here: it
/// appeared in that listing, and that is where somebody watching would have
/// seen it happen.
fn holder_of(path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    PathBuf::from(path).parent().map(Path::to_path_buf)
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// What was done to the contents of one directory, newest first.
///
/// Both ends count: a file copied *into* here belongs in this folder's story
/// as much as one copied out of it, and the row says which way it went.
///
/// Commands are left out. A command is recorded against the directory it ran
/// in rather than a file, so treating its path the same way would file every
/// command under the folder above the one it ran in - and commands have their
/// own list, from [`commands_before`].
pub fn happened_in(records: &[Record], here: &Path) -> Vec<Happening> {
    let mut out: Vec<Happening> = Vec::new();
    for record in records.iter().rev() {
        let Record::Event(event) = record else {
            continue;
        };
        if event.kind == Kind::Command {
            continue;
        }
        let from_here = holder_of(&event.path).as_deref() == Some(here);
        let arrived = event
            .to
            .as_deref()
            .and_then(holder_of)
            .is_some_and(|holder| holder == here);
        if !from_here && !arrived {
            continue;
        }

        // The end that is here names the row; the other end, if it is
        // somewhere else, is what the row can point at.
        let (name, other) = if from_here {
            (
                name_of(&event.path),
                event.to.as_deref().and_then(holder_of),
            )
        } else {
            (
                name_of(event.to.as_deref().unwrap_or(&event.path)),
                holder_of(&event.path),
            )
        };
        // The name at the other end, when it is a different name. A copy
        // that kept its name says nothing here, and should not.
        let far_end = if from_here {
            event.to.as_deref().map(name_of)
        } else {
            Some(name_of(&event.path))
        };
        out.push(Happening {
            at: event.at,
            kind: event.kind,
            became: far_end.filter(|other| *other != name),
            name,
            other: other.filter(|elsewhere| elsewhere != here),
            incoming: arrived && !from_here,
            note: event.note.clone(),
            failed: event.failed.clone(),
        });
    }
    out
}

/// Where to stand to look at what this entry describes.
pub fn scene_of(event: &Event) -> Option<Scene> {
    let left = directory_of(event.kind, &event.path)?;
    let right = event
        .to
        .as_deref()
        .and_then(|to| directory_of(event.kind, to))
        // The two ends of a copy within one directory are one place, and
        // sending both panels there says nothing twice.
        .filter(|right| *right != left);
    Some(Scene { left, right })
}

pub fn lines(rows: &[Row]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (row, entry) in rows.iter().enumerate() {
        match entry {
            Row::One(_) => lines.push(Line::Alone { row }),
            Row::Run { events, .. } => {
                lines.push(Line::Heading { row });
                for event in 0..events.len() {
                    lines.push(Line::Under { row, event });
                }
            }
        }
    }
    lines
}

/// The event one line is about, if it is about one.
pub fn event_at<'a>(rows: &'a [Row], line: &Line) -> Option<&'a Event> {
    match line {
        Line::Heading { .. } => None,
        Line::Alone { row } => match rows.get(*row)? {
            Row::One(event) => Some(event),
            Row::Run { .. } => None,
        },
        Line::Under { row, event } => match rows.get(*row)? {
            Row::Run { events, .. } => events.get(*event),
            Row::One(_) => None,
        },
    }
}

/// Counts for the line above the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub rows: usize,
    pub items: usize,
    pub failures: usize,
}

pub fn tally(rows: &[Row]) -> Tally {
    Tally {
        rows: rows.len(),
        items: rows.iter().map(Row::items).sum(),
        failures: rows.iter().map(Row::failures).sum(),
    }
}

/// A fresh group id.
///
/// The clock in microseconds, which is unique enough for something that only
/// has to tell one run apart from another within one day file.
pub fn new_group_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_answers_from_memory_once_read() {
        let (_dir, journal) = journal();
        journal.record(Event::new(Kind::Copy, "/a"));
        assert_eq!(journal.with_records(Stream::Files, |r| r.len()), 1);

        // Take the files away behind its back: memory is what answers now,
        // and a write keeps feeding it.
        for entry in std::fs::read_dir(journal.dir()).unwrap().flatten() {
            std::fs::remove_file(entry.path()).unwrap();
        }
        journal.record(Event::new(Kind::Copy, "/b"));
        assert_eq!(
            journal.with_records(Stream::Files, |r| r.len()),
            2,
            "one read at the start, appends from then on - never a re-read per query"
        );
    }

    #[test]
    fn a_clone_on_another_thread_feeds_the_same_cache() {
        // Job::spawn_recorded hands a clone of the journal to the worker
        // thread that runs a copy. A cache owned per clone would leave the
        // front-end's copy stale the moment a job wrote a record.
        let (_dir, journal) = journal();
        assert_eq!(journal.with_records(Stream::Files, |r| r.len()), 0);

        let worker = journal.clone();
        std::thread::spawn(move || {
            worker.record(Event::new(Kind::Copy, "/from-the-worker"));
        })
        .join()
        .unwrap();

        assert_eq!(
            journal.with_records(Stream::Files, |r| r.len()),
            1,
            "the worker's record is in the front-end's memory"
        );
    }

    #[test]
    fn the_generation_moves_when_the_account_does_and_only_then() {
        let (_dir, journal) = journal();
        let before = journal.generation();

        journal.with_records(Stream::Files, |_| ());
        journal.days(Stream::Files);
        assert_eq!(journal.generation(), before, "reading is not a change");

        journal.record(Event::new(Kind::Copy, "/a"));
        let after_write = journal.generation();
        assert!(after_write > before);

        journal.clear();
        assert!(
            journal.generation() > after_write,
            "clearing is a change too"
        );
    }

    #[test]
    fn sweeping_drops_the_memory_along_with_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path(), Keep(7));
        let mut old = Event::new(Kind::Copy, "/old");
        old.at = now() - 30 * 86_400;
        journal.write(Stream::Files, &Record::Event(old));
        journal.record(Event::new(Kind::Copy, "/new"));
        assert_eq!(journal.with_records(Stream::Files, |r| r.len()), 2);

        journal.sweep(Day::today());
        assert_eq!(
            journal.with_records(Stream::Files, |r| r.len()),
            1,
            "the memory re-read what the sweep left"
        );
    }

    #[test]
    fn since_cuts_by_age_through_one_binary_search() {
        let mut records = Vec::new();
        for days_ago in [30, 8, 6, 0] {
            let mut event = Event::new(Kind::Command, "/somewhere");
            event.at = now() - days_ago * 86_400;
            records.push(Record::Event(event));
        }

        let week = since(&records, 7);
        assert_eq!(week.len(), 2, "six days ago and today");
        assert_eq!(since(&records, 365).len(), 4);
        assert!(since(&records, 0).is_empty());
    }

    fn journal() -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path().join("journal"), Keep::default());
        (dir, journal)
    }

    fn day(year: i32, month: u32, day: u32) -> Day {
        Day { year, month, day }
    }

    #[test]
    fn a_record_written_reads_back_the_same() {
        let (_dir, journal) = journal();
        let event = Event::new(Kind::Copy, "/from/a.txt")
            .to("/to/a.txt")
            .note("1.2K");
        journal.record(event.clone());

        let today = Day::of_time(event.at);
        let back = journal.read(Stream::Files, today);
        assert_eq!(back, vec![Record::Event(event)]);
    }

    #[test]
    fn commands_go_to_their_own_stream() {
        // Mixed together, a build's twenty commands a minute would bury the
        // file operations they were meant to sit beside.
        let (_dir, journal) = journal();
        let command = Event::new(Kind::Command, "/work")
            .note("cargo build")
            .note("exit 0");
        let copy = Event::new(Kind::Copy, "/a").to("/b");
        journal.record(command);
        journal.record(copy.clone());

        let today = Day::today();
        assert_eq!(
            journal.read(Stream::Files, today),
            vec![Record::Event(copy)]
        );
        assert_eq!(journal.read(Stream::Shell, today).len(), 1);
        assert_eq!(journal.days(Stream::Files), vec![today]);
        assert_eq!(journal.days(Stream::Shell), vec![today]);
    }

    #[test]
    fn the_two_streams_can_be_read_back_as_one_story() {
        // Kept apart on disk so a build does not bury the file work; put back
        // together for reading, because "what was I doing at four o'clock"
        // needs both, and a copy followed by the make that consumed it is one
        // story told in two files.
        let (_dir, journal) = journal();
        journal.record(Event {
            at: 100,
            ..Event::new(Kind::Copy, "/src/a.c").to("/build/a.c")
        });
        journal.record(Event {
            at: 200,
            ..Event::new(Kind::Command, "/build").note("make")
        });
        journal.record(Event {
            at: 300,
            ..Event::new(Kind::Delete, "/build/a.o")
        });

        let day = Day::of_time(100);
        let rows = arrange(journal.read_shown(Shown::All, day));
        let kinds: Vec<Kind> = rows
            .iter()
            .map(|row| match row {
                Row::One(event) => event.kind,
                Row::Run { group, .. } => group.kind,
            })
            .collect();
        assert_eq!(
            kinds,
            vec![Kind::Delete, Kind::Command, Kind::Copy],
            "newest first, and interleaved by when they happened"
        );

        // The narrower views still show only their own.
        assert_eq!(arrange(journal.read_shown(Shown::Files, day)).len(), 2);
        assert_eq!(arrange(journal.read_shown(Shown::Commands, day)).len(), 1);
    }

    #[test]
    fn a_view_offers_the_kinds_it_can_actually_hold() {
        // Offering a filter that can only ever match nothing is offering a
        // way to see nothing.
        assert!(Shown::Files.holds(Kind::Copy));
        assert!(!Shown::Files.holds(Kind::Command));
        assert!(Shown::Commands.holds(Kind::Command));
        assert!(Shown::Commands.holds(Kind::Session));
        assert!(!Shown::Commands.holds(Kind::Copy));
        assert!(KINDS.into_iter().all(|kind| Shown::All.holds(kind)));
    }

    #[test]
    fn the_days_of_a_mixed_view_are_the_days_of_either() {
        let (_dir, journal) = journal();
        // Two different days, one in each stream.
        journal.record(Event {
            at: 1_700_000_000,
            ..Event::new(Kind::Copy, "/a")
        });
        journal.record(Event {
            at: 1_700_500_000,
            ..Event::new(Kind::Command, "/w").note("ls")
        });

        let files = Day::of_time(1_700_000_000);
        let commands = Day::of_time(1_700_500_000);
        assert_eq!(journal.days_shown(Shown::Files), vec![files]);
        assert_eq!(journal.days_shown(Shown::Commands), vec![commands]);

        let both = journal.days_shown(Shown::All);
        assert_eq!(both.len(), 2, "got {both:?}");
        assert!(both[0] > both[1], "newest first");
    }

    #[test]
    fn a_day_in_both_streams_is_offered_once() {
        let (_dir, journal) = journal();
        journal.record(Event::new(Kind::Copy, "/a"));
        journal.record(Event::new(Kind::Command, "/w").note("ls"));
        assert_eq!(journal.days_shown(Shown::All), vec![Day::today()]);
    }

    #[test]
    fn a_command_is_labelled_with_the_shell_that_ran_it() {
        // "Command" on every line of a list of commands says nothing. Which
        // shell ran it is the thing that cannot be worked out afterwards -
        // and "why did that behave oddly" is often "that was the other one".
        let event = Event::new(Kind::Command, "/work").note("ls").by("zsh");
        assert_eq!(event.label(), "zsh");
        assert_eq!(event.shell.as_deref(), Some("zsh"));

        // A file operation has no shell, and says what it did instead.
        let copy = Event::new(Kind::Copy, "/a").to("/b");
        assert_eq!(copy.label(), "Copied");
        assert_eq!(copy.shell, None);

        // An empty name is no name, not a blank column.
        assert_eq!(Event::new(Kind::Command, "/w").by("").label(), "Command");
    }

    #[test]
    fn a_run_carries_a_total_and_a_single_file_does_not() {
        // Per-file durations would be sixteen bytes copied in under a
        // millisecond, four hundred times over. The total for the run is the
        // number anybody actually wants.
        let (_dir, journal) = journal();
        let id = new_group_id();
        journal.open_group(Group {
            id,
            at: now(),
            kind: Kind::Copy,
            summary: "Copy 2 item(s) to /to".to_string(),
        });
        journal.record(Event::new(Kind::Copy, "/a").in_group(id));
        journal.record(Event::new(Kind::Copy, "/b").in_group(id));
        journal.close_group(id, 2_500);

        let rows = arrange(journal.read(Stream::Files, Day::today()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].took(), Some(2_500));
        let Row::Run { events, .. } = &rows[0] else {
            panic!("not a run")
        };
        assert!(events.iter().all(|event| event.ms.is_none()));
    }

    #[test]
    fn a_run_that_never_finished_has_no_total() {
        // Killed halfway, or the program closed under it. The heading is
        // written before the work starts precisely so that this case still
        // has one, and the missing total is how it shows.
        let (_dir, journal) = journal();
        let id = new_group_id();
        journal.open_group(Group {
            id,
            at: now(),
            kind: Kind::Copy,
            summary: "Copy 400 item(s) to /to".to_string(),
        });
        journal.record(Event::new(Kind::Copy, "/a").in_group(id));

        let rows = arrange(journal.read(Stream::Files, Day::today()));
        assert_eq!(rows[0].took(), None);
    }

    #[test]
    fn a_finish_record_survives_the_round_trip() {
        let (_dir, journal) = journal();
        journal.close_group(77, 1_234);
        let back = journal.read(Stream::Files, Day::today());
        let Some(Record::Done(done)) = back.first() else {
            panic!("got {back:?}")
        };
        assert_eq!((done.id, done.ms), (77, 1_234));
    }

    #[test]
    fn a_duration_is_said_the_way_it_is_read() {
        // Precision falls away as the number grows: milliseconds matter for
        // something that felt instant, and nobody wants three decimal places
        // on a twenty-minute build.
        assert_eq!(took(0), "<1ms", "under the resolution, not zero");
        assert_eq!(took(999), "999ms");
        assert_eq!(took(1_000), "1.0s");
        assert_eq!(took(2_460), "2.4s");
        assert_eq!(took(9_999), "9.9s");
        assert_eq!(took(10_000), "10s");
        assert_eq!(took(59_999), "59s");
        assert_eq!(took(60_000), "1m 00s");
        assert_eq!(took(3_599_000), "59m 59s");
        assert_eq!(took(3_600_000), "1h 00m");
        assert_eq!(took(7_830_000), "2h 10m");
    }
    #[test]
    fn the_views_come_round_in_order() {
        assert_eq!(Shown::All.next(), Shown::Files);
        assert_eq!(Shown::Files.next(), Shown::Commands);
        assert_eq!(Shown::Commands.next(), Shown::All);
    }

    #[test]
    fn a_half_written_line_does_not_cost_the_rest_of_the_day() {
        let (_dir, journal) = journal();
        journal.record(Event::new(Kind::Copy, "/one"));
        journal.record(Event::new(Kind::Copy, "/two"));

        // As a kill mid-append would leave it.
        let path = journal.file(Stream::Files, Day::today());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"record\":\"event\",\"at\":1,\"kind\":\"co");
        std::fs::write(&path, text).unwrap();

        let back = journal.read(Stream::Files, Day::today());
        assert_eq!(back.len(), 2, "the two good records survive");
    }

    #[test]
    fn nothing_recorded_is_not_an_error() {
        let (_dir, journal) = journal();
        assert!(journal.read(Stream::Files, Day::today()).is_empty());
        assert!(journal.days(Stream::Files).is_empty());
        assert_eq!(journal.clear(), 0);
        assert_eq!(journal.sweep(Day::today()), 0);
    }

    #[test]
    fn a_journal_that_cannot_be_written_does_not_complain() {
        // The rule that matters: a read-only home must not stop a copy. There
        // is nothing to assert but the absence of a panic and of a Result.
        //
        // The path has to be one the OS actually refuses, and each refuses for
        // its own reason: /proc is a kernel filesystem nothing may add to,
        // while `?` is not a legal character in a Windows filename. A Unix
        // path used on Windows would not do - `/proc/nowhere` there is just
        // `C:\proc\nowhere`, which is created happily and tests nothing.
        #[cfg(windows)]
        let nowhere = r"C:\lost-commander-nowhere?\at\all";
        #[cfg(not(windows))]
        let nowhere = "/proc/nowhere/at/all";

        let journal = Journal::at(nowhere, Keep::default());
        journal.record(Event::new(Kind::Copy, "/a"));
        assert!(journal.read(Stream::Files, Day::today()).is_empty());
    }

    #[test]
    fn the_days_with_records_are_listed_newest_first() {
        let (_dir, journal) = journal();
        std::fs::create_dir_all(journal.dir()).unwrap();
        for name in [
            "files-2026-07-26.jsonl",
            "files-2026-07-28.jsonl",
            "files-2026-07-27.jsonl",
            "shell-2026-07-28.jsonl",
            "notes.txt",
            "files-nonsense.jsonl",
        ] {
            std::fs::write(journal.dir().join(name), "").unwrap();
        }
        assert_eq!(
            journal.days(Stream::Files),
            vec![day(2026, 7, 28), day(2026, 7, 27), day(2026, 7, 26)]
        );
        assert_eq!(journal.days(Stream::Shell), vec![day(2026, 7, 28)]);
    }

    #[test]
    fn a_file_name_gives_up_its_date_or_is_not_ours() {
        assert_eq!(
            Day::from_file_name("files-2026-07-28.jsonl", Stream::Files),
            Some(day(2026, 7, 28))
        );
        assert_eq!(
            Day::from_file_name("shell-1999-12-31.jsonl", Stream::Shell),
            Some(day(1999, 12, 31))
        );
        // The other stream's file is not this stream's day.
        assert_eq!(
            Day::from_file_name("shell-2026-07-28.jsonl", Stream::Files),
            None
        );
        for wrong in [
            "files-2026-07.jsonl",
            "files-2026-13-01.jsonl",
            "files-2026-07-32.jsonl",
            "files-2026-07-28.txt",
            "settings.toml",
            "",
        ] {
            assert_eq!(Day::from_file_name(wrong, Stream::Files), None, "{wrong}");
        }
    }

    #[test]
    fn one_date_subtracted_from_another_counts_days() {
        assert_eq!(day(2026, 7, 28).days_before(day(2026, 7, 28)), 0);
        assert_eq!(day(2026, 7, 27).days_before(day(2026, 7, 28)), 1);
        assert_eq!(day(2026, 6, 28).days_before(day(2026, 7, 28)), 30);
        // Across a year, and across a leap day.
        assert_eq!(day(2025, 7, 28).days_before(day(2026, 7, 28)), 365);
        assert_eq!(day(2024, 2, 28).days_before(day(2024, 3, 1)), 2);
        assert_eq!(day(2023, 2, 28).days_before(day(2023, 3, 1)), 1);
        // And backwards.
        assert_eq!(day(2026, 7, 29).days_before(day(2026, 7, 28)), -1);
    }

    #[test]
    fn sweeping_takes_the_old_days_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path(), Keep(7));
        for name in [
            "files-2026-07-28.jsonl", // today
            "files-2026-07-22.jsonl", // six days ago
            "files-2026-07-21.jsonl", // seven - out
            "shell-2026-07-01.jsonl", // long gone
            "keep-me.txt",            // not ours
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }

        assert_eq!(journal.sweep(day(2026, 7, 28)), 2);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "files-2026-07-22.jsonl",
                "files-2026-07-28.jsonl",
                "keep-me.txt"
            ]
        );
    }

    #[test]
    fn keeping_for_ever_sweeps_nothing() {
        // A retention setting whose lowest value silently deleted everything
        // would be a trap. Zero means for ever.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path(), Keep(0));
        std::fs::write(dir.path().join("files-2001-01-01.jsonl"), "x").unwrap();
        assert!(journal.keep.forever());
        assert_eq!(journal.sweep(day(2026, 7, 28)), 0);
        assert!(dir.path().join("files-2001-01-01.jsonl").exists());
        assert_eq!(Keep(0).describe(), "for ever");
        assert_eq!(Keep(1).describe(), "for a day");
        assert_eq!(Keep(30).describe(), "for 30 days");
    }

    #[test]
    fn clearing_takes_the_journals_own_files_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path(), Keep::default());
        std::fs::write(dir.path().join("files-2026-07-28.jsonl"), "x").unwrap();
        std::fs::write(dir.path().join("shell-2026-07-28.jsonl"), "x").unwrap();
        std::fs::write(dir.path().join("important.txt"), "x").unwrap();

        assert_eq!(journal.clear(), 2);
        assert!(dir.path().join("important.txt").exists());
        assert!(!dir.path().join("files-2026-07-28.jsonl").exists());
    }

    #[test]
    fn a_run_gathers_its_files_under_one_heading() {
        let group = Group {
            id: 7,
            at: 1000,
            kind: Kind::Copy,
            summary: "Copy 2 items to /backup".to_string(),
        };
        let records = vec![
            Record::Group(group.clone()),
            Record::Event(Event::new(Kind::Copy, "/a").in_group(7)),
            Record::Event(Event::new(Kind::Copy, "/b").in_group(7)),
            Record::Event(Event::new(Kind::Rename, "/loose")),
        ];

        let rows = arrange(records);
        assert_eq!(rows.len(), 2, "one run and one loose event");
        let run = rows
            .iter()
            .find(|row| matches!(row, Row::Run { .. }))
            .expect("a run");
        assert_eq!(run.items(), 2);
        assert_eq!(run.kind(), Kind::Copy);
        let Row::Run { group: heading, .. } = run else {
            panic!("not a run")
        };
        assert_eq!(heading.summary, "Copy 2 items to /backup");
    }

    #[test]
    fn an_event_whose_heading_is_missing_is_still_shown() {
        // The heading fell on the other side of midnight, or the run was
        // killed before it was written. A record that exists must not vanish
        // because its heading did.
        let rows = arrange(vec![Record::Event(
            Event::new(Kind::Copy, "/orphan").in_group(99),
        )]);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::One(_)));
    }

    #[test]
    fn rows_come_back_newest_first() {
        let mut older = Event::new(Kind::Copy, "/old");
        older.at = 100;
        let mut newer = Event::new(Kind::Copy, "/new");
        newer.at = 200;
        let rows = arrange(vec![Record::Event(older), Record::Event(newer)]);
        assert_eq!(rows[0].at(), 200);
        assert_eq!(rows[1].at(), 100);
    }

    #[test]
    fn the_filter_narrows_by_kind_by_failure_and_by_name() {
        let group = Group {
            id: 1,
            at: 500,
            kind: Kind::Copy,
            summary: "Copy 3 items".to_string(),
        };
        let rows = arrange(vec![
            Record::Group(group),
            Record::Event(Event::new(Kind::Copy, "/photos/beach.jpg").in_group(1)),
            Record::Event(Event::new(Kind::Copy, "/photos/hill.jpg").in_group(1)),
            Record::Event(
                Event::new(Kind::Copy, "/photos/broken.jpg")
                    .in_group(1)
                    .failed("no room"),
            ),
            Record::Event(Event::new(Kind::Delete, "/notes.txt")),
        ]);

        let open = Filter::default();
        assert!(open.is_open());
        assert_eq!(tally(&open.apply(rows.clone())).items, 4);

        let mut by_kind = Filter::default();
        by_kind.toggle(Kind::Delete);
        assert!(by_kind.has(Kind::Delete));
        let only_deletes = by_kind.apply(rows.clone());
        assert_eq!(only_deletes.len(), 1);
        assert_eq!(only_deletes[0].kind(), Kind::Delete);

        // Failures only trims a run to the file that failed rather than
        // dropping the run - the run is where you look for it.
        let failures = Filter {
            failures_only: true,
            ..Default::default()
        };
        let kept = failures.apply(rows.clone());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].items(), 1);
        assert_eq!(kept[0].failures(), 1);

        // And by name, which is how "where did that file go" is answered.
        let named = Filter {
            text: "BEACH".to_string(),
            ..Default::default()
        };
        let found = named.apply(rows);
        assert_eq!(found.len(), 1);
        let Row::Run { events, .. } = &found[0] else {
            panic!("not a run")
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "/photos/beach.jpg");
    }

    #[test]
    fn the_search_box_looks_at_everything_the_line_shows() {
        // A path is the least interesting thing on most of these lines. The
        // rule is that what is on screen is what can be found - which is also
        // the only rule that answers the questions worth asking.
        let rows = arrange(vec![
            Record::Event(
                Event::new(Kind::Command, "/work")
                    .note("cargo build --release")
                    .by("zsh")
                    .lasting(9_000),
            ),
            Record::Event(
                Event::new(Kind::Command, "/work")
                    .note("rm -rf target")
                    .by("bash")
                    .failed("exit 1"),
            ),
            Record::Event(Event::new(Kind::Open, "/photos/holiday.raw").note("GIMP (gimp)")),
            Record::Event(Event::new(Kind::Edit, "/notes.txt").note("UTF-8 -> Windows-1251")),
        ]);

        let found = |needle: &str| {
            Filter {
                text: needle.to_string(),
                ..Default::default()
            }
            .apply(rows.clone())
        };

        // By what was run, which is a note and not a path.
        assert_eq!(found("--release").len(), 1);
        // By which shell ran it.
        assert_eq!(found("zsh").len(), 1);
        // By how it failed.
        assert_eq!(found("exit 1").len(), 1);
        // By what a file was opened with - the question that makes recording
        // an open worth doing at all.
        assert_eq!(found("gimp").len(), 1, "case does not matter");
        // By what an edit did.
        assert_eq!(found("windows-1251").len(), 1);
        // By the kind as the column writes it.
        assert_eq!(found("opened").len(), 1);
        // And still by path.
        assert_eq!(found("/photos").len(), 1);
        // Something on no line finds nothing.
        assert!(found("nowhere").is_empty());
    }

    #[test]
    fn a_run_found_by_its_heading_keeps_all_of_its_files() {
        // Searching for "backup" and finding "Copy 42 items to /backup"
        // should show the forty-two, not hide them because their own paths
        // say nothing about a backup.
        let rows = arrange(vec![
            Record::Group(Group {
                id: 1,
                at: 500,
                kind: Kind::Copy,
                summary: "Copy 3 item(s) to /backup".to_string(),
            }),
            Record::Event(Event::new(Kind::Copy, "/photos/a.jpg").in_group(1)),
            Record::Event(Event::new(Kind::Copy, "/photos/b.jpg").in_group(1)),
            Record::Event(Event::new(Kind::Copy, "/photos/c.jpg").in_group(1)),
        ]);

        let found = Filter {
            text: "backup".to_string(),
            ..Default::default()
        }
        .apply(rows.clone());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].items(), 3, "the whole run, not an empty heading");

        // A search that matches one file still narrows to that file.
        let narrowed = Filter {
            text: "b.jpg".to_string(),
            ..Default::default()
        }
        .apply(rows);
        assert_eq!(narrowed[0].items(), 1);
    }

    #[test]
    fn an_open_records_what_it_was_handed_to() {
        // Not a change - this program cannot see what another one does to a
        // file - but the most it can honestly say about one made outside it.
        let event = Event::new(Kind::Open, "/photos/holiday.raw").note("GIMP (gimp)");
        assert_eq!(event.kind.stream(), Stream::Files);
        assert_eq!(event.label(), "Opened");
        assert_eq!(event.kind.short(), "open");
        assert!(!event.kind.is_destructive());
    }

    #[test]
    fn a_run_that_recorded_nothing_still_shows_under_an_open_filter() {
        // Killed before it wrote a single file. The heading is the record
        // that it happened at all.
        let rows = arrange(vec![Record::Group(Group {
            id: 1,
            at: 10,
            kind: Kind::Copy,
            summary: "Copy 900 items".to_string(),
        })]);
        assert_eq!(rows.len(), 1);
        assert_eq!(Filter::default().apply(rows.clone()).len(), 1);

        let narrowed = Filter {
            text: "anything".to_string(),
            ..Default::default()
        };
        assert!(narrowed.apply(rows).is_empty());
    }

    #[test]
    fn what_is_on_the_list_is_counted() {
        let rows = arrange(vec![
            Record::Event(Event::new(Kind::Copy, "/a")),
            Record::Event(Event::new(Kind::Copy, "/b").failed("gone")),
        ]);
        let counted = tally(&rows);
        assert_eq!(counted.rows, 2);
        assert_eq!(counted.items, 2);
        assert_eq!(counted.failures, 1);
    }

    #[test]
    fn a_run_becomes_a_heading_and_a_line_for_each_file() {
        let rows = arrange(vec![
            Record::Group(Group {
                id: 1,
                at: 500,
                kind: Kind::Copy,
                summary: "Copy 2 items".to_string(),
            }),
            Record::Event(Event::new(Kind::Copy, "/a").in_group(1)),
            Record::Event(Event::new(Kind::Copy, "/b").in_group(1)),
        ]);
        let drawn = lines(&rows);
        assert_eq!(drawn.len(), 3, "a heading and its two files");
        assert!(drawn[0].is_heading());
        assert!(!drawn[1].is_heading());

        // A heading is about a run, not about a file.
        assert!(event_at(&rows, &drawn[0]).is_none());
        assert_eq!(event_at(&rows, &drawn[1]).unwrap().path, "/a");
        assert_eq!(event_at(&rows, &drawn[2]).unwrap().path, "/b");

        // And one that stood alone is one line.
        let alone = arrange(vec![Record::Event(Event::new(Kind::Rename, "/c"))]);
        let drawn = lines(&alone);
        assert_eq!(drawn.len(), 1);
        assert!(!drawn[0].is_heading());
        assert_eq!(event_at(&alone, &drawn[0]).unwrap().path, "/c");
    }

    #[test]
    fn a_day_says_itself_in_words() {
        assert_eq!(day(2026, 7, 28).describe(), "28 July 2026");
        assert_eq!(day(2026, 7, 28).name(), "2026-07-28");
        assert_eq!(day(2026, 12, 1).name(), "2026-12-01");
    }

    #[test]
    fn group_ids_differ_between_runs() {
        let first = new_group_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(first, new_group_id());
    }

    #[test]
    fn a_copy_puts_both_ends_on_screen() {
        // The question anybody asks after a copy is what both ends look like
        // now, and that is two directories rather than one.
        let event = Event::new(Kind::Copy, "/src/notes/a.txt").to("/backup/2026/a.txt");
        let scene = scene_of(&event).expect("a scene");
        assert_eq!(scene.left, PathBuf::from("/src/notes"));
        assert_eq!(scene.right, Some(PathBuf::from("/backup/2026")));
    }

    #[test]
    fn a_copy_inside_one_directory_is_one_place() {
        // Sending both panels to the same directory says nothing twice.
        let event = Event::new(Kind::Copy, "/src/a.txt").to("/src/a.copy.txt");
        let scene = scene_of(&event).expect("a scene");
        assert_eq!(scene.left, PathBuf::from("/src"));
        assert_eq!(scene.right, None);
    }

    #[test]
    fn a_command_ran_in_a_directory_rather_than_on_a_file() {
        // Its path is already where to stand; taking a parent would send the
        // panel one level above where the command ran.
        let event = Event::new(Kind::Command, "/work/project").note("cargo test");
        let scene = scene_of(&event).expect("a scene");
        assert_eq!(scene.left, PathBuf::from("/work/project"));
        assert_eq!(scene.right, None);
    }

    #[test]
    fn a_deleted_file_still_says_where_it_was() {
        // The file has gone; the directory it was in is exactly what somebody
        // wants to look at afterwards.
        let event = Event::new(Kind::Delete, "/src/old/gone.txt");
        assert_eq!(
            scene_of(&event).expect("a scene").left,
            PathBuf::from("/src/old")
        );
    }

    #[test]
    fn a_record_with_no_path_has_nowhere_to_send_anybody() {
        let event = Event::new(Kind::Command, "");
        assert!(scene_of(&event).is_none());
    }

    #[test]
    fn what_was_run_here_is_offered_first() {
        // The point of recording where a command ran: `cargo test` here and
        // `cargo test` somewhere else are the same words about different
        // work, and the one you want is nearly always the one from where you
        // are standing.
        let records = vec![
            Record::Event(Event::new(Kind::Command, "/elsewhere").note("make")),
            Record::Event(Event::new(Kind::Command, "/work").note("cargo build")),
            Record::Event(Event::new(Kind::Command, "/work").note("cargo test")),
        ];
        let past = commands_before(&records, Path::new("/work"));
        let lines: Vec<&str> = past.iter().map(|p| p.line.as_str()).collect();
        assert_eq!(lines, vec!["cargo test", "cargo build", "make"]);
    }

    #[test]
    fn the_same_command_is_offered_once() {
        // A history that offers the same line four times makes you press the
        // key four times to get past it.
        let records = vec![
            Record::Event(Event::new(Kind::Command, "/work").note("ls")),
            Record::Event(Event::new(Kind::Command, "/work").note("ls")),
            Record::Event(Event::new(Kind::Command, "/work").note("ls")),
        ];
        assert_eq!(commands_before(&records, Path::new("/work")).len(), 1);
    }

    #[test]
    fn only_commands_and_only_ones_with_a_line() {
        let records = vec![
            Record::Event(Event::new(Kind::Copy, "/a").to("/b")),
            Record::Event(Event::new(Kind::Command, "/work")),
        ];
        assert!(commands_before(&records, Path::new("/work")).is_empty());
    }

    #[test]
    fn what_happened_here_is_newest_first_and_names_the_file() {
        let records = vec![
            Record::Event(Event::new(Kind::MakeDir, "/work/build")),
            Record::Event(Event::new(Kind::Delete, "/work/old.txt")),
        ];
        let here = happened_in(&records, Path::new("/work"));
        let names: Vec<&str> = here.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["old.txt", "build"]);
        // A made directory belongs to the folder it appeared in, not to
        // itself - that listing is where somebody would have seen it happen.
        assert_eq!(here[1].kind, Kind::MakeDir);
    }

    #[test]
    fn both_ends_of_a_copy_are_part_of_a_folder_s_story() {
        let records = vec![Record::Event(
            Event::new(Kind::Copy, "/src/report.txt").to("/work/report.txt"),
        )];

        // Where it came from: it left here, and the row can point at where
        // it went.
        let out = happened_in(&records, Path::new("/src"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "report.txt");
        assert!(!out[0].incoming);
        assert_eq!(out[0].other, Some(PathBuf::from("/work")));

        // Where it arrived: the same event, read from the other end. A file
        // appearing in a folder is the thing you most want explained.
        let into = happened_in(&records, Path::new("/work"));
        assert_eq!(into.len(), 1);
        assert!(into[0].incoming);
        assert_eq!(into[0].other, Some(PathBuf::from("/src")));
    }

    #[test]
    fn a_copy_within_one_folder_points_nowhere_else() {
        let records = vec![Record::Event(
            Event::new(Kind::Copy, "/work/a.txt").to("/work/b.txt"),
        )];
        let here = happened_in(&records, Path::new("/work"));
        assert_eq!(here.len(), 1);
        assert_eq!(here[0].other, None, "both ends are this folder");
    }

    #[test]
    fn a_folder_s_story_is_about_its_own_contents() {
        let records = vec![
            Record::Event(Event::new(Kind::Delete, "/work/sub/deep.txt")),
            Record::Event(Event::new(Kind::Delete, "/elsewhere/other.txt")),
            // A command is filed against the directory it ran in, so reading
            // its path the same way would put every command in the folder
            // above the one it ran in.
            Record::Event(Event::new(Kind::Command, "/work/sub").note("make")),
        ];
        assert!(happened_in(&records, Path::new("/work")).is_empty());
        assert_eq!(happened_in(&records, Path::new("/work/sub")).len(), 1);
    }

    #[test]
    fn a_failure_is_kept_because_it_is_the_answer_more_often_than_not() {
        let records = vec![Record::Event(
            Event::new(Kind::Delete, "/work/locked.txt").failed("in use"),
        )];
        let here = happened_in(&records, Path::new("/work"));
        assert_eq!(here[0].failed.as_deref(), Some("in use"));
    }
}
