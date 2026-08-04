// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Application state and key handling.
//!
//! Key handling is deliberately kept out of the render loop so it can be
//! driven directly from tests: `App::on_key` is a pure state transition.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use lost_commander_core::apps;
use lost_commander_core::compare;
use lost_commander_core::diff;
use lost_commander_core::dupes;
use lost_commander_core::elevate::{self, Elevation};
use lost_commander_core::encoding;
use lost_commander_core::find;
use lost_commander_core::fsops;
use lost_commander_core::hex;
use lost_commander_core::journal;
use lost_commander_core::mount;
use lost_commander_core::netloc::{Bookmarks, Location};
use lost_commander_core::open;
use lost_commander_core::panel::{Panel, SortBy};
use lost_commander_core::perms::{self, What, Who};
use lost_commander_core::progress::{self, Answer, Job, Operation};
use lost_commander_core::rename;
use lost_commander_core::tabs::Tabs;

pub const PREVIEW_LIMIT: usize = 2 * 1024 * 1024;

/// A path's last component, for a message that has no room for the rest.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    MakeDir,
    Rename(PathBuf),
    CopyTo(Vec<PathBuf>),
    MoveTo(Vec<PathBuf>),
    /// Add a bookmark from a typed URL or path.
    AddBookmark,
    /// Mark, or unmark, everything matching a mask.
    SelectPattern {
        select: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    Delete {
        targets: Vec<PathBuf>,
        /// To the trash, where it can be got back.
        to_trash: bool,
    },
    /// Opening this would run it rather than show it - see [`open::runs_code`].
    Run(PathBuf),
}

/// Which part of the find form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindField {
    Named,
    Containing,
    Results,
}

impl FindField {
    /// Tab walks the form; the results are only reachable once there are some.
    pub fn next(self, has_results: bool) -> FindField {
        match self {
            FindField::Named => FindField::Containing,
            FindField::Containing if has_results => FindField::Results,
            FindField::Containing => FindField::Named,
            FindField::Results => FindField::Named,
        }
    }
}

/// Which box of the multi-rename form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameField {
    Name,
    Extension,
    Find,
    Replace,
    Case,
}

impl RenameField {
    pub const ALL: [RenameField; 5] = [
        RenameField::Name,
        RenameField::Extension,
        RenameField::Find,
        RenameField::Replace,
        RenameField::Case,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RenameField::Name => "name",
            RenameField::Extension => "extension",
            RenameField::Find => "replace",
            RenameField::Replace => "with",
            RenameField::Case => "case",
        }
    }

    /// Tab walks the form and comes back round to the start.
    pub fn next(self) -> RenameField {
        let at = RenameField::ALL
            .iter()
            .position(|f| *f == self)
            .unwrap_or(0);
        RenameField::ALL[(at + 1) % RenameField::ALL.len()]
    }

    pub fn prev(self) -> RenameField {
        let at = RenameField::ALL
            .iter()
            .position(|f| *f == self)
            .unwrap_or(0);
        RenameField::ALL[(at + RenameField::ALL.len() - 1) % RenameField::ALL.len()]
    }
}

/// The two lists on the connections screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnTab {
    Saved,
    Recent,
    /// The drives and folders this machine already has.
    ///
    /// Without them there is no way to reach a second drive except to know
    /// its letter and type it - and on Windows there is no root to walk up to
    /// that would reveal one, since `C:` and `D:` are two trees rather than
    /// two directories.
    System,
}

impl ConnTab {
    pub fn other(self) -> ConnTab {
        match self {
            ConnTab::Saved => ConnTab::Recent,
            ConnTab::Recent => ConnTab::System,
            ConnTab::System => ConnTab::Saved,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InputDialog {
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub action: InputAction,
}

#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Input(InputDialog),
    Confirm(ConfirmDialog),
    Viewer {
        title: String,
        lines: Vec<String>,
        scroll: usize,
        /// Kept so `e` can read the same file another way. Guessing wrong is
        /// a screenful of replacement characters, which from the outside looks
        /// exactly like a corrupt file - the only way to tell is to try.
        path: PathBuf,
        /// What the bytes were read as. `None` until something is forced,
        /// which is what makes "as found" a state rather than a guess.
        forced: Option<encoding::Encoding>,
        detected: encoding::Detected,
    },
    /// The account of what was done: a day at a time, filtered.
    Journal {
        shown: journal::Shown,
        /// The days that have records, newest first.
        days: Vec<journal::Day>,
        /// Which of them is showing.
        at: usize,
        /// The day as read, before the filter.
        rows: Vec<journal::Row>,
        filter: journal::Filter,
        cursor: usize,
        /// Whether typing is going into the search box rather than being
        /// read as commands. `/` opens it, Escape closes it.
        searching: bool,
    },
    /// The saved network locations / bookmarks screen.
    Connections {
        tab: ConnTab,
        cursor: usize,
    },
    /// Which shell runs underneath the panels.
    ///
    /// Worth a picker rather than only a line in a configuration file,
    /// because on Windows the machine's own answer is `cmd`, `cmd` has no
    /// seam for the hook, and a reader would otherwise have no way to
    /// discover that the shared directory needs a different shell - or that
    /// there is a shared directory at all.
    Shells {
        shells: Vec<String>,
        cursor: usize,
    },
    /// A long-running copy/move/delete is in flight.
    Progress,
    /// Find files by name, and by what is inside them.
    Find {
        query: find::Query,
        root: PathBuf,
        /// Which box is being typed into, or the results list.
        field: FindField,
        cursor: usize,
    },
    /// Files that are the same file twice, and which copies to let go of.
    Duplicates {
        root: PathBuf,
        options: dupes::Options,
        groups: Vec<dupes::Group>,
        /// Which row of the flattened list the cursor is on. The list is a
        /// group heading and then its copies, so a row is one or the other.
        cursor: usize,
    },
    /// A file that is not text, shown as bytes.
    ///
    /// Carries where the file is rather than the file: only the rows on
    /// screen are ever read, so this opens as fast on a gigabyte as on a byte.
    Bytes {
        name: String,
        dump: hex::Dump,
        /// The first row on screen.
        scroll: u64,
        /// `false` until `F4`, and the dump is read-only until it is true.
        /// A hex view you can type into by accident is a hex view that
        /// corrupts a file by accident.
        editing: bool,
        cursor: hex::Cursor,
        edits: hex::Edits,
        /// An offset being typed, or `None` when nobody is typing one.
        ///
        /// Kept as text: half an offset is not a number yet, and a field
        /// that refused the keystroke which would finish it would be
        /// unusable.
        goto: Option<String>,
    },
    /// Two files, line by line.
    Difference {
        left: PathBuf,
        right: PathBuf,
        diff: Box<diff::Diff>,
        /// The first row on screen.
        scroll: usize,
    },
    /// Make two trees agree: what differs, which way it would go, and a key
    /// that carries it out.
    ///
    /// The pairs are owned here rather than read from the scan each redraw,
    /// because the directions are edited - a list that came back from the
    /// worker every frame would forget them.
    Sync {
        left: PathBuf,
        right: PathBuf,
        options: compare::Options,
        show: compare::Show,
        pairs: Vec<compare::Pair>,
        cursor: usize,
        /// The comparison stopped at [`compare::MAX_PAIRS`] with tree left
        /// over. Said out loud: a list that quietly stops short reads as a
        /// list of everything there is.
        capped: bool,
    },
    /// New names for the whole selection at once.
    ///
    /// The plan is kept beside the rules rather than worked out while drawing,
    /// because working it out stats every target name - once per keystroke is
    /// nothing, and once per redraw would not be.
    MultiRename {
        rules: rename::Rules,
        sources: Vec<rename::Source>,
        changes: Vec<rename::Change>,
        field: RenameField,
        /// The first preview line shown, for a selection longer than the box.
        scroll: usize,
    },
    /// What a file is, and what it is allowed to be.
    Properties {
        was: Box<perms::Properties>,
        now: Box<perms::Properties>,
        /// Which of the nine boxes the cursor is on.
        cursor: usize,
    },
    /// A copy or move is blocked on a file that is already there.
    ///
    /// The worker is asleep until this is answered, so every key here is an
    /// answer - there is no dismissing it.
    Overwrite {
        conflict: progress::Conflict,
    },
    /// The key list, which outgrew a short terminal and so can be walked.
    Help {
        scroll: usize,
    },
    /// "Open with...": pick an application, or type a command.
    ///
    /// The list is read once when this opens rather than on every keystroke -
    /// a few hundred small files is cheap once and not per character.
    OpenWith {
        target: PathBuf,
        applications: Vec<apps::Application>,
        /// Narrows the list while anything matches; a command when nothing does.
        typed: String,
        cursor: usize,
        /// Ask the system to authorise it first - see `elevate`.
        as_admin: bool,
    },
}

impl Mode {
    /// Used by the test suite to assert that dialogs opened and closed.
    #[allow(dead_code)]
    pub fn is_normal(&self) -> bool {
        matches!(self, Mode::Normal)
    }
}

/// What submits a line to a shell: a carriage return, byte 13.
///
/// Written as the number rather than as an escape so the character never has
/// to appear in this file. A stray one in a source file is invisible in every
/// editor and rejected by the compiler with a message about a byte nobody can
/// see.
const ENTER: &[u8] = &[13];

/// The command line out of a journal entry, and where it ran.
///
/// `None` for anything that is not a command: a copy or a rename is a record
/// of something that happened, not a line anybody typed, and offering to
/// "run it again" would be offering to do something the reader never wrote.
fn command_to_reuse(event: &journal::Event) -> Option<(String, String)> {
    if event.kind != journal::Kind::Command || event.note.trim().is_empty() {
        return None;
    }
    Some((event.note.clone(), event.path.clone()))
}

/// What to write into the shell to run `line` in `cwd`.
///
/// One line for a shell that reports where it is, two for one that does not.
///
/// The second is `cd`, and it is what Far Manager does on Windows for the
/// same reason: `cmd` has no seam to hook, so nothing can be asked of it, and
/// half-sharing a directory is worse than not sharing one. Without it, a `cd`
/// typed into the shell moves it somewhere the panel never learns about, and
/// every command afterwards runs somewhere other than the prompt on the
/// command line says it will. The panel is the answer instead.
///
/// A hooked shell is left alone: it reports where it goes, both sides follow,
/// and sending it back would undo a `cd` the reader meant.
fn command_lines(program: &str, cwd: &std::path::Path, line: &str) -> Vec<String> {
    if lost_commander_core::shellhook::journals(program) {
        return vec![line.to_string()];
    }
    vec![
        lost_commander_core::shell::cd_command(program, cwd),
        line.to_string(),
    ]
}

pub struct App {
    pub left: Tabs,
    pub right: Tabs,
    pub active: Side,
    pub mode: Mode,
    /// The account of what has been done, or nothing where it is off.
    pub journal: Option<journal::Journal>,
    pub status: String,
    pub status_is_error: bool,
    pub should_quit: bool,
    /// Set when a privileged command needs to be run with the TUI suspended,
    /// so its password prompt has the terminal to itself.
    pub pending_shell: Option<String>,
    /// The shell running underneath the panels, once anything has needed one.
    ///
    /// One shell for the whole session rather than one per command, which is
    /// what makes `cd` mean anything: a fresh shell per command starts in
    /// whatever directory it is given and forgets everything the last one
    /// did. Started on first use, because most of what this program does
    /// needs no shell at all and a pty costs a process.
    /// The drives and folders this machine offers.
    ///
    /// Read once at startup: finding them asks each drive letter whether it
    /// answers, and doing that on every keystroke is a thing you would hear a
    /// spinning disk doing.
    pub system_places: Vec<lost_commander_core::places::Place>,
    pub shell: Option<lost_commander_core::pty::PtySession>,
    /// Which shell to run, when the reader has chosen one.
    ///
    /// `None` takes the machine's own answer. That answer is `cmd` on
    /// Windows whatever `$SHELL` says, and `cmd` has no seam for the hook -
    /// so the directory the panels and the shell share is a thing a Windows
    /// reader has to opt into by naming a shell that does. It is in
    /// `settings.toml` as `shell`.
    pub shell_program: Option<String>,
    /// Whether the shell's screen is what is on show, rather than the panels.
    pub showing_shell: bool,
    /// Set by Ctrl-Z: give the terminal back and stop, until resumed.
    ///
    /// The main loop does it, for the same reason as the shell screen.
    pub pending_suspend: bool,
    /// Whether the keyboard is in a pane's tree half rather than its files.
    ///
    /// One flag per side. Two lists in one pane is two cursors, and a reader
    /// who cannot tell which one an arrow key moves will not trust either.
    pub on_tree: [bool; 2],
    /// The command being typed, along the bottom of the panels.
    ///
    /// Norton Commander's arrangement, and Midnight Commander's after it: the
    /// panels are drawn over a shell rather than instead of one, so what you
    /// type when you are not pressing a function key is a command.
    pub command: String,
    /// Set when the user asks to edit a file; the main loop suspends the TUI,
    /// runs $EDITOR, and clears this.
    pub pending_edit: Option<PathBuf>,
    /// Saved local and network locations.
    pub bookmarks: Bookmarks,
    /// Where bookmarks are persisted. `None` disables saving, which keeps the
    /// test suite away from the real user configuration.
    pub bookmarks_path: Option<PathBuf>,
    /// The copy/move/delete currently running on a worker thread.
    pub job: Option<Job>,
    /// Network shares attached this session, as (browsable path, location).
    ///
    /// Kept so that a bookmark taken *inside* a share records the network
    /// location rather than a mount path that will not exist next time.
    pub active_mounts: Vec<(PathBuf, Location)>,
    /// How a file is handed to the desktop.
    ///
    /// A field rather than a direct call so the test suite can watch what
    /// would be opened without any real application starting.
    /// The search in flight, if any.
    pub search: Option<find::Search>,
    /// The directory comparison in flight, if any.
    pub scan: Option<compare::Scan>,
    /// The duplicate hunt in flight, if any.
    pub hunt: Option<dupes::Scan>,
    /// What was last searched for, so reopening the form does not start over.
    pub last_query: find::Query,
    pub opener: open::Opener,
    /// How a chosen application is started - the "Open with..." counterpart
    /// to `opener`, and injectable for the same reason.
    pub launcher: open::Launcher,
}

impl App {
    pub fn new(left: PathBuf, right: PathBuf) -> Self {
        let mut app = Self::detached(left, right);
        app.bookmarks = Bookmarks::load();
        app.bookmarks_path = Bookmarks::config_path();
        // The account, and a sweep of what has aged out of it. Once at
        // startup: it is the only moment the program is certainly not in the
        // middle of writing to it.
        let settings = lost_commander_core::config::Settings::load();
        app.journal = settings.journal();
        // A chosen shell was being read from the file and then ignored here,
        // so the setting the graphical view honours did nothing in this one.
        app.shell_program = settings.shell.clone();
        if let Some(journal) = &app.journal {
            journal.sweep(journal::Day::today());
        }
        // Seed after loading, or the stored history would overwrite it.
        app.seed_recent();
        app
    }

    /// Where the panels start counts as visited, so Recent always includes the
    /// directory you are actually looking at.
    fn seed_recent(&mut self) {
        let right = self.right.cwd().to_path_buf();
        let left = self.left.cwd().to_path_buf();
        self.bookmarks.push_recent(Location::local(right));
        self.bookmarks.push_recent(Location::local(left));
    }

    /// An app with no connection to the on-disk bookmark file.
    pub fn detached(left: PathBuf, right: PathBuf) -> Self {
        let mut app = Self::bare(left, right);
        app.seed_recent();
        app
    }

    fn bare(left: PathBuf, right: PathBuf) -> Self {
        App {
            left: Tabs::new(Panel::new(left)),
            right: Tabs::new(Panel::new(right)),
            active: Side::Left,
            // Bare means bare: a test must not write into the real account,
            // so this is only wired up by `new`.
            journal: None,
            mode: Mode::Normal,
            // The first thing on screen, and until now the first thing never
            // shown. It names Ctrl-Q as well as F10 because F10 is the one
            // key here that a terminal may keep for itself.
            status: String::from("F1 help   Tab switches panels   F10 or Ctrl-Q quits"),
            status_is_error: false,
            should_quit: false,
            on_tree: [false, false],
            command: String::new(),
            pending_edit: None,
            pending_shell: None,
            system_places: lost_commander_core::places::system_places(),
            shell: None,
            shell_program: None,
            showing_shell: false,
            pending_suspend: false,
            bookmarks: Bookmarks::default(),
            bookmarks_path: None,
            job: None,
            active_mounts: Vec::new(),
            search: None,
            scan: None,
            hunt: None,
            last_query: find::Query::default(),
            opener: Box::new(open::open),
            launcher: Box::new(open::launch),
        }
    }

    /// Describe a path as a saveable location.
    ///
    /// Inside a share attached this session this yields the network location
    /// (with the sub-path appended) instead of the local mount path, so the
    /// bookmark still works after a reboot.
    pub fn location_for(&self, path: &Path) -> Location {
        for (mount, location) in &self.active_mounts {
            let Ok(relative) = path.strip_prefix(mount) else {
                continue;
            };
            let mut out = location.clone();
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.is_empty() {
                out.path = format!("{}/{}", out.path.trim_end_matches('/'), relative);
            }
            out.name = out.default_name();
            return out;
        }
        Location::local(path)
    }

    /// Note where the panels are now, so the Recent list reflects it.
    pub fn record_visits(&mut self, before: (PathBuf, PathBuf)) {
        let after = (
            self.left.cwd().to_path_buf(),
            self.right.cwd().to_path_buf(),
        );
        if after.0 != before.0 {
            let location = self.location_for(&after.0);
            self.bookmarks.push_recent(location);
        }
        if after.1 != before.1 {
            let location = self.location_for(&after.1);
            self.bookmarks.push_recent(location);
        }
    }

    /// Persist bookmarks and history; called when quitting.
    pub fn persist_on_exit(&mut self) {
        self.persist_bookmarks();
    }

    // ---- background operations ---------------------------------------------

    pub fn job_is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Note one thing that happened, if an account is being kept.
    ///
    /// Every single-file operation goes through here rather than reaching for
    /// the journal itself, so "is there an account at all" is asked in one
    /// place and each call site reads as one line.
    fn note(&self, event: journal::Event) {
        if let Some(journal) = &self.journal {
            journal.record(event);
        }
    }

    fn start_job(&mut self, operation: Operation) {
        if self.job.is_some() {
            self.error("Another operation is already running");
            return;
        }
        self.job = match &self.journal {
            Some(journal) => Some(Job::spawn_recorded(operation, journal.clone())),
            None => Some(Job::spawn(operation)),
        };
        self.mode = Mode::Progress;
    }

    /// Notice what something else did to the directories on screen.
    ///
    /// A file manager whose listing is only right until anything else touches
    /// the disk is one you have to remember to press `Ctrl-R` in. Not while an
    /// operation is running: that re-reads both panels when it finishes, and a
    /// listing changing under a copy would be the copy's own writes reported
    /// back as news.
    pub fn poll_directories(&mut self) {
        if self.job.is_some() {
            return;
        }
        for panel in [self.left.current_mut(), self.right.current_mut()] {
            if panel.poll_changes() {
                if let Some(tree) = panel.tree.as_mut() {
                    tree.refresh();
                }
            }
        }
    }

    /// Called from the event loop; retires the job once the worker is done.
    pub fn poll_job(&mut self) {
        let Some(job) = &self.job else {
            return;
        };

        // The worker sleeps on a collision until it is answered, so the
        // question has to get onto the screen before anything else.
        if let Some(conflict) = job.asking() {
            if !matches!(self.mode, Mode::Overwrite { .. }) {
                self.mode = Mode::Overwrite { conflict };
            }
            return;
        }

        if !job.is_finished() {
            return;
        }

        let snapshot = job.snapshot();
        let past = job.operation.past_tense();
        let mut job = self.job.take().expect("checked above");
        job.join();

        if matches!(self.mode, Mode::Progress) {
            self.mode = Mode::Normal;
        }

        self.active_panel_mut().clear_marks();
        self.reload_both();

        if snapshot.cancelled {
            self.error(format!(
                "Cancelled after {} of {} item(s)",
                snapshot.items_done, snapshot.items_total
            ));
        } else if snapshot.failures.is_empty() {
            self.info(snapshot.outcome(past));
        } else {
            self.error(format!(
                "{past} {}/{}; failed: {}",
                snapshot.items_done,
                snapshot.items_total,
                snapshot.failures.join("; ")
            ));
        }
    }

    pub fn cancel_job(&mut self) {
        if let Some(job) = &self.job {
            job.request_cancel();
            self.info("Cancelling...");
        }
    }

    /// Block until any running job stops; used when quitting.
    pub fn finish_job(&mut self) {
        if let Some(job) = &mut self.job {
            job.join();
        }
        self.poll_job();
    }

    fn persist_bookmarks(&mut self) {
        let Some(path) = self.bookmarks_path.clone() else {
            return;
        };
        if let Err(e) = self.bookmarks.save_to(&path) {
            self.error(format!("Could not save bookmarks: {e}"));
        }
    }

    // ---- network locations -------------------------------------------------

    pub fn open_connections(&mut self) {
        self.mode = Mode::Connections {
            tab: ConnTab::Saved,
            cursor: 0,
        };
    }

    /// Save the active panel's directory so it can be jumped to later.
    pub fn bookmark_current_dir(&mut self) {
        let location = self.location_for(&self.active_panel().cwd.clone());
        let name = location.name.clone();
        self.bookmarks.add(location);
        self.persist_bookmarks();
        if !self.status_is_error {
            self.info(format!("Bookmarked \"{name}\""));
        }
    }

    pub fn remove_bookmark(&mut self, index: usize) {
        match self.bookmarks.remove(index) {
            Some(removed) => {
                self.persist_bookmarks();
                if !self.status_is_error {
                    self.info(format!("Removed \"{}\"", removed.name));
                }
            }
            None => self.error("Nothing to remove"),
        }
    }

    /// Detach a mounted network location, if it is currently attached.
    pub fn disconnect_bookmark(&mut self, index: usize) {
        let Some(location) = self.bookmarks.locations.get(index).cloned() else {
            self.error("No such bookmark");
            return;
        };
        if !location.protocol.is_network() {
            self.error("Not a network location");
            return;
        }

        let roots = mount::candidate_roots(mount::Platform::current());
        match mount::find_mount_in(&roots, &location) {
            Some(path) => match mount::disconnect(&path) {
                Ok(()) => self.info(format!("Disconnected {}", location.to_url())),
                Err(reason) => self.error(format!("Disconnect failed: {reason}")),
            },
            None => self.error(format!("{} is not mounted", location.to_url())),
        }
    }

    /// Attach the location if necessary and point the active panel at it.
    ///
    /// Mounting blocks the UI while the OS works; that is acceptable for the
    /// few seconds it usually takes, but it is the first thing to move onto a
    /// worker thread if it proves annoying.
    pub fn connect_bookmark(&mut self, index: usize) {
        let Some(location) = self.bookmarks.locations.get(index).cloned() else {
            self.error("No such bookmark");
            return;
        };

        match mount::connect(&location) {
            Ok(path) => {
                self.mode = Mode::Normal;
                if location.protocol.is_network() {
                    self.active_mounts.retain(|(p, _)| p != &path);
                    self.active_mounts.push((path.clone(), location.clone()));
                }
                self.active_panel_mut().chdir(path.clone());
                if let Some(error) = self.active_panel().error.clone() {
                    self.error(format!("{}: {error}", path.display()));
                } else {
                    self.info(format!("Connected to {}", location.to_url()));
                }
            }
            Err(reason) => self.error(reason),
        }
    }

    pub fn panel(&self, side: Side) -> &Panel {
        match side {
            Side::Left => self.left.current(),
            Side::Right => self.right.current(),
        }
    }

    pub fn active_panel(&self) -> &Panel {
        self.panel(self.active)
    }

    pub fn active_panel_mut(&mut self) -> &mut Panel {
        match self.active {
            Side::Left => self.left.current_mut(),
            Side::Right => self.right.current_mut(),
        }
    }

    pub fn other_panel(&self) -> &Panel {
        match self.active {
            Side::Left => self.right.current(),
            Side::Right => self.left.current(),
        }
    }

    fn info(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = false;
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = true;
    }

    pub fn reload_both(&mut self) {
        self.left.reload();
        self.right.reload();
    }

    // ---- entry points for the function keys -------------------------------

    pub fn switch_panel(&mut self) {
        self.active = match self.active {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
    }

    pub fn swap_panels(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
    }

    // ---- tabs ---------------------------------------------------------------

    pub fn tabs(&self, side: Side) -> &Tabs {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub fn tabs_mut(&mut self, side: Side) -> &mut Tabs {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn active_tabs_mut(&mut self) -> &mut Tabs {
        self.tabs_mut(self.active)
    }

    /// `Ctrl-T`: another tab, on the directory this one is showing.
    pub fn new_tab(&mut self) {
        let panel = self.tabs(self.active).duplicate();
        self.active_tabs_mut().open(panel);
        let where_ = self.active_panel().cwd.display().to_string();
        self.info(format!("New tab: {where_}"));
    }

    /// Alt-Enter in the journal: open where an entry happened, as new tabs.
    ///
    /// New tabs rather than moving the panels, because looking at what a
    /// command did should not cost you the place you were working in - and a
    /// copy has two ends, so it opens two: the directory it came from on the
    /// left and the one it went to on the right.
    ///
    /// A path may simply not be there any more. The file was deleted, the
    /// directory was the point of the deletion, or it is on a disk nobody has
    /// mounted today. Whatever is still there is opened and whatever is not
    /// is named, because "nothing happened" is the one answer that leaves a
    /// reader with no idea which of those it was.
    pub fn open_scene(&mut self, scene: &journal::Scene) {
        let mut missing: Vec<String> = Vec::new();
        let mut opened = 0;

        for (side, path) in [
            (Side::Left, Some(scene.left.clone())),
            (Side::Right, scene.right.clone()),
        ] {
            let Some(path) = path else { continue };
            if !path.is_dir() {
                missing.push(path.display().to_string());
                continue;
            }
            let mut panel = match side {
                Side::Left => self.left.duplicate(),
                Side::Right => self.right.duplicate(),
            };
            panel.chdir(path);
            match side {
                Side::Left => self.left.open(panel),
                Side::Right => self.right.open(panel),
            }
            opened += 1;
        }

        if opened > 0 {
            self.active = Side::Left;
        }
        match (opened, missing.len()) {
            (0, _) => self.error(format!("Gone: {}", missing.join(", "))),
            (_, 0) => self.info(format!("Opened {opened} tab(s) where that happened")),
            (_, _) => self.info(format!(
                "Opened {opened} tab(s); gone: {}",
                missing.join(", ")
            )),
        }
    }

    /// `Ctrl-W`: close the tab on show.
    pub fn close_tab(&mut self) {
        if self.active_tabs_mut().close() {
            self.info("Tab closed");
        } else {
            // Closing the last tab would leave the pane showing nothing, and a
            // pane with nothing in it is not a thing this program has.
            self.error("That is the only tab in this pane");
        }
    }

    /// `Alt-W`: keep the tab on show and close the rest.
    pub fn close_other_tabs(&mut self) {
        match self.active_tabs_mut().close_others() {
            0 => self.error("There is only this tab"),
            n => self.info(format!(
                "Closed {n} other {}",
                if n == 1 { "tab" } else { "tabs" }
            )),
        }
    }

    pub fn next_tab(&mut self) {
        self.active_tabs_mut().next();
    }

    pub fn previous_tab(&mut self) {
        self.active_tabs_mut().prev();
    }

    /// `Shift-F6`: send this tab to the other pane, as `F6` sends a file.
    ///
    /// The tab goes whole - its cursor, its marks, its sort order - because a
    /// tab that arrived as a bare path would have lost the reason you wanted
    /// it over there.
    pub fn move_tab_across(&mut self) {
        let Some(panel) = self.active_tabs_mut().take() else {
            self.error("That is the only tab in this pane");
            return;
        };
        let where_ = panel.cwd.display().to_string();
        let other = match self.active {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
        self.tabs_mut(other).accept(panel);
        // Following it across is what you meant by moving it: the tab is the
        // thing you were working in, and it is now over there.
        self.active = other;
        self.info(format!("Moved to the other pane: {where_}"));
    }

    pub fn open_selected(&mut self) {
        let is_dir = self.active_panel().selected().map(|e| e.is_dir());
        match is_dir {
            Some(true) => {
                self.active_panel_mut().enter();
            }
            // An archive is a folder that happens to be one file.
            Some(false)
                if !self.active_panel().in_archive()
                    && self
                        .active_panel()
                        .selected()
                        .is_some_and(|e| lost_commander_core::archive::is_archive(&e.path)) =>
            {
                let path = self.active_panel().selected().unwrap().path.clone();
                match self.active_panel_mut().open_archive(&path, None) {
                    Ok(()) => {
                        let what = self
                            .active_panel()
                            .inside
                            .as_ref()
                            .map(|inside| (inside.members.len(), inside.format));
                        if let Some((count, format)) = what {
                            self.info(format!("{count} item(s), {format}"));
                        }
                    }
                    Err(e) => self.error(format!("Could not open {}: {e}", path.display())),
                }
            }
            // A file goes to whatever application the desktop has registered
            // for it, which is what "open" means everywhere else. `F3` is
            // still the built-in viewer for when that is what you wanted.
            Some(false) => self.open_with_desktop(),
            None => {}
        }
    }

    /// Hand the file under the cursor to the desktop's handler.
    ///
    /// Anything that would be *run* rather than shown asks first. Not a
    /// refusal - a file manager whose Enter key cannot start a program is
    /// missing something the original had - but one keystroke of distance
    /// between the cursor landing on `setup.exe` and `setup.exe` running.
    pub fn open_with_desktop(&mut self) {
        let Some(entry) = self.active_panel().selected() else {
            return;
        };
        let (path, name) = (entry.path.clone(), entry.name.clone());

        let executable = open::is_executable(&path);
        let targets = [(path.clone(), executable)];
        if let Some(message) = open::open_warning(mount::Platform::current(), &targets) {
            self.mode = Mode::Confirm(ConfirmDialog {
                title: "Run".into(),
                message,
                action: ConfirmAction::Run(path),
            });
            return;
        }
        self.launch(&path, &name);
    }

    fn launch(&mut self, path: &Path, name: &str) {
        match (self.opener)(path) {
            Ok(()) => self.info(format!("Opened {name}")),
            Err(e) => self.error(e),
        }
    }

    /// `Ctrl-P`: open the file with an application of your choosing.
    ///
    /// The graphical view puts this on `Shift-Enter`, which a terminal cannot
    /// rely on: most terminals send a plain `Enter` for it, so the two would
    /// be indistinguishable here. `Ctrl-P` always arrives.
    pub fn open_with(&mut self) {
        let target = self
            .active_panel()
            .selected()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing to open");
            return;
        };

        // Where the system has its own chooser, that is the chooser.
        if let Some(command) = apps::chooser_command(mount::Platform::current(), &target) {
            match (self.launcher)(&command) {
                Ok(()) => self.info("Open with..."),
                Err(e) => self.error(e),
            }
            return;
        }

        self.mode = Mode::OpenWith {
            applications: apps::applications_for(&target),
            target,
            typed: String::new(),
            cursor: 0,
            as_admin: false,
        };
    }

    /// Start what the chooser settled on.
    fn run_chosen(&mut self) {
        let Mode::OpenWith {
            target,
            applications,
            typed,
            cursor,
            as_admin,
        } = &self.mode
        else {
            return;
        };
        let (target, cursor, as_admin) = (target.clone(), *cursor, *as_admin);
        let name = target.file_name().unwrap_or_default().to_string_lossy();

        let Some(chosen) = apps::choice(applications, typed, cursor) else {
            self.error("Nothing chosen");
            return;
        };
        let (label, command) = match chosen {
            apps::Chosen::App(app) => (
                app.name.clone(),
                apps::open_with_command(mount::Platform::current(), app, &target),
            ),
            apps::Chosen::Command(typed) => (typed.to_string(), apps::exec_command(typed, &target)),
        };

        self.mode = Mode::Normal;
        let Some(command) = command else {
            self.error(format!("{label} is not a command"));
            return;
        };

        if as_admin {
            let display = elevate::display_here();
            let elevation = elevate::elevate(
                mount::Platform::current(),
                &command,
                display.as_ref().map(|(d, x)| (d.as_str(), x.as_str())),
                &lost_commander_core::preview::on_disk,
            );
            self.run_elevated(elevation, &format!("{name} with {label}"));
            return;
        }

        match (self.launcher)(&command) {
            Ok(()) => self.info(format!("Opened {name} with {label}")),
            Err(e) => self.error(e),
        }
    }

    /// Carry out an elevation, whichever of its two shapes it turned out to be.
    ///
    /// Nothing here grants privilege. A `Command` spawns the system's own
    /// authorisation prompt; a `Shell` line needs a terminal for its password
    /// prompt, and this front-end is in one but has the screen - so the line
    /// goes to `pending_shell` and the main loop runs it with the TUI
    /// suspended, exactly as `F4` hands over to `$EDITOR`.
    pub fn run_elevated(&mut self, elevation: Elevation, said: &str) {
        match elevation {
            Elevation::Command(command) => match (self.launcher)(&command) {
                Ok(()) => self.info(format!("Authorising: {said}")),
                Err(e) => self.error(e),
            },
            Elevation::Shell(line) => {
                self.pending_shell = Some(line);
                self.info(format!("Running: {said}"));
            }
            Elevation::Refused(reason) => self.error(reason),
        }
    }

    /// `Shift-F4`: edit a file you do not own.
    ///
    /// Not "run the editor as root" - see `elevate::edit_as_root` for why that
    /// is the wrong tool even though it is the obvious one.
    pub fn edit_as_admin(&mut self) {
        let target = self
            .active_panel()
            .selected()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing to edit");
            return;
        };
        let editor = crate::editor_command();
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let said = format!("{editor} {name}, as administrator");
        let elevation = elevate::edit_as_root(mount::Platform::current(), &editor, &target);
        self.run_elevated(elevation, &said);
    }

    /// `Alt-F7` / `Ctrl-F`: find files under the active panel.
    pub fn open_find(&mut self) {
        self.mode = Mode::Find {
            query: self.last_query.clone(),
            root: self.active_panel().cwd.clone(),
            field: FindField::Named,
            cursor: 0,
        };
    }

    fn on_key_find(&mut self, key: KeyEvent) {
        let has_results = self.search.as_ref().map(|s| s.count()).unwrap_or(0);
        let Mode::Find {
            query,
            root,
            field,
            cursor,
        } = &mut self.mode
        else {
            return;
        };

        match key.code {
            KeyCode::Esc => {
                // The thread stops with the form rather than walking a disk
                // for a list nobody is looking at.
                self.search = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Tab => *field = field.next(has_results > 0),
            KeyCode::Up if *field == FindField::Results => *cursor = cursor.saturating_sub(1),
            KeyCode::Down if *field == FindField::Results => {
                *cursor = (*cursor + 1).min(has_results.saturating_sub(1))
            }
            KeyCode::F(3) => query.case_sensitive = !query.case_sensitive,
            KeyCode::F(4) => query.include_hidden = !query.include_hidden,
            KeyCode::Enter => {
                if *field == FindField::Results && has_results > 0 {
                    let path = self
                        .search
                        .as_ref()
                        .and_then(|s| s.snapshot().hits.get(*cursor).cloned())
                        .map(|hit| hit.path);
                    if let Some(path) = path {
                        self.search = None;
                        self.mode = Mode::Normal;
                        self.go_to(&path);
                    }
                    return;
                }
                if query.is_empty() {
                    return;
                }
                let (query, root) = (query.clone(), root.clone());
                self.last_query = query.clone();
                self.search = Some(find::Search::spawn(root, query));
                *cursor = 0;
            }
            KeyCode::Backspace => match field {
                FindField::Named => {
                    query.pattern.pop();
                }
                FindField::Containing => {
                    query.contains.pop();
                }
                FindField::Results => {}
            },
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => match field {
                FindField::Named => query.pattern.push(c),
                FindField::Containing => query.contains.push(c),
                FindField::Results => {}
            },
            _ => {}
        }
    }

    /// `Shift-F3`: two files, line by line.
    ///
    /// Which two is [`diff::choose`]'s answer: mark a pair in one pane, or put
    /// one under each pane's cursor.
    pub fn open_difference(&mut self) {
        let chosen = match diff::choose(
            self.left.current(),
            self.right.current(),
            self.active == Side::Left,
        ) {
            Ok(chosen) => chosen,
            Err(reason) => {
                self.error(reason);
                return;
            }
        };
        match diff::open(&chosen) {
            Ok(difference) => {
                if difference.is_identical() {
                    // Worth saying rather than showing: a window of unchanged
                    // lines is a puzzle, and "they are the same" is the answer
                    // to what was actually asked.
                    self.info(format!(
                        "{} and {} are identical",
                        name_of(&chosen.left),
                        name_of(&chosen.right)
                    ));
                    return;
                }
                self.mode = Mode::Difference {
                    left: chosen.left,
                    right: chosen.right,
                    diff: Box::new(difference),
                    scroll: 0,
                };
            }
            Err(refusal) => self.error(refusal.message()),
        }
    }

    fn on_key_difference(&mut self, key: KeyEvent) {
        let Mode::Difference { diff, scroll, .. } = &mut self.mode else {
            return;
        };
        let last = diff.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Down => *scroll = (*scroll + 1).min(last),
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => *scroll = (*scroll + 20).min(last),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
            KeyCode::Home => *scroll = 0,
            KeyCode::End => *scroll = last,
            // The whole point of the window: walk what differs, rather than
            // scrolling past a thousand lines that do not.
            KeyCode::Tab | KeyCode::Char('n') => {
                if let Some(at) = diff.next_change(*scroll) {
                    *scroll = at;
                }
            }
            KeyCode::BackTab | KeyCode::Char('p') => {
                if let Some(at) = diff.previous_change(*scroll) {
                    *scroll = at;
                }
            }
            _ => {}
        }
    }

    /// `Alt-U`: files under this pane that are the same file twice.
    ///
    /// U because C, D and S already belong to the comparison family and the
    /// mnemonic space for "duplicate" was spoken for by all three.
    pub fn open_duplicates(&mut self) {
        let root = self.active_panel().cwd.clone();
        let options = dupes::Options::default();
        self.hunt = Some(dupes::Scan::spawn(root.clone(), options.clone()));
        self.mode = Mode::Duplicates {
            root,
            options,
            groups: Vec::new(),
            cursor: 0,
        };
    }

    /// Bring in whatever the hunt has finished, so its list can be ticked.
    pub fn collect_hunt(&mut self) {
        let finished = self.hunt.as_ref().map(|scan| scan.is_finished());
        if finished != Some(true) {
            return;
        }
        let found = self.hunt.take().map(|scan| scan.snapshot());
        let truncated = found.as_ref().map(|f| f.truncated).unwrap_or(false);
        if let (Some(found), Mode::Duplicates { groups, cursor, .. }) = (found, &mut self.mode) {
            *groups = found.groups;
            *cursor = 0;
        }
        if truncated {
            // The cap is a memory guard, not a judgement about the tree - and
            // a list that quietly stops reads as "that is all there is".
            self.error(format!(
                "The first {} sets - there were more",
                dupes::MAX_GROUPS
            ));
        }
    }

    fn on_key_duplicates(&mut self, key: KeyEvent) {
        self.collect_hunt();
        let running = self.hunt.is_some();
        let mut restart = false;
        let (mut go, mut remove) = (None, None);
        {
            let Mode::Duplicates {
                options,
                groups,
                cursor,
                ..
            } = &mut self.mode
            else {
                return;
            };
            let lines = dupes::lines(groups);
            let last = lines.len().saturating_sub(1);

            match key.code {
                KeyCode::Esc => {
                    // The thread stops with the window rather than reading a
                    // disk for a list nobody is looking at.
                    self.hunt = None;
                    self.mode = Mode::Normal;
                    return;
                }
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => *cursor = (*cursor + 1).min(last),
                KeyCode::PageUp => *cursor = cursor.saturating_sub(10),
                KeyCode::PageDown => *cursor = (*cursor + 10).min(last),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = last,
                KeyCode::Char(' ') => {
                    if let Some(line) = lines.get(*cursor).copied() {
                        dupes::toggle_at(groups, line);
                    }
                }
                // Go to a copy, which is how you find out what it is before
                // deciding anything about it.
                KeyCode::Enter => {
                    if let Some(dupes::Line::Copy { group, copy }) = lines.get(*cursor).copied() {
                        go = groups
                            .get(group)
                            .and_then(|set| set.copies.get(copy))
                            .map(|c| c.path.clone());
                    }
                }
                KeyCode::F(4) => {
                    options.include_hidden = !options.include_hidden;
                    restart = true;
                }
                KeyCode::F(8) | KeyCode::Delete if !running => {
                    let targets = dupes::to_remove(groups);
                    if !targets.is_empty() {
                        remove = Some(targets);
                    }
                }
                KeyCode::F(2) if !running => restart = true,
                _ => {}
            }
        }

        if restart {
            let Mode::Duplicates {
                root,
                options,
                groups,
                cursor,
                ..
            } = &mut self.mode
            else {
                return;
            };
            groups.clear();
            *cursor = 0;
            self.hunt = Some(dupes::Scan::spawn(root.clone(), options.clone()));
        }
        if let Some(path) = go {
            self.hunt = None;
            self.mode = Mode::Normal;
            self.go_to(&path);
        }
        if let Some(targets) = remove {
            self.hunt = None;
            // Through the ordinary delete, which means the trash and means
            // being asked first - a list worked out by a rule is exactly the
            // list you want a second look at.
            self.mode = Mode::Confirm(ConfirmDialog {
                title: "Delete duplicates".into(),
                message: format!(
                    "Delete {} extra {}, keeping one of each?",
                    targets.len(),
                    if targets.len() == 1 { "copy" } else { "copies" }
                ),
                action: ConfirmAction::Delete {
                    targets,
                    to_trash: true,
                },
            });
        }
    }

    /// `Alt-C`: mark what differs between the two panes.
    ///
    /// No dialog and no walk - it compares the two listings already on screen,
    /// which is what makes it instant and what makes it stop at the top level.
    /// The recursive question is what `Alt-S` is for.
    pub fn compare_folders(&mut self) {
        let case = compare::case_sensitive(mount::Platform::current());
        let (marked_left, marked_right) =
            compare::mark_differences(self.left.current_mut(), self.right.current_mut(), case);
        if marked_left + marked_right == 0 {
            self.info("These two agree");
        } else {
            self.info(format!(
                "Marked {marked_left} on the left and {marked_right} on the right"
            ));
        }
    }

    /// `Alt-S`: what differs between the two trees, and which way it would go.
    pub fn open_sync(&mut self) {
        let left = self.left.cwd().to_path_buf();
        let right = self.right.cwd().to_path_buf();
        if left == right {
            self.error("Both panes are showing the same directory");
            return;
        }
        let options = compare::Options::default();
        self.scan = Some(compare::Scan::spawn(
            left.clone(),
            right.clone(),
            options.clone(),
        ));
        self.mode = Mode::Sync {
            left,
            right,
            options,
            show: compare::Show::differences_only(),
            pairs: Vec::new(),
            cursor: 0,
            capped: false,
        };
    }

    /// Bring in whatever the comparison has finished, so its list can be
    /// edited. Called from the redraw loop as well as from the key handler,
    /// because a scan finishes on its own schedule rather than on a key press.
    pub fn collect_scan(&mut self) {
        let finished = self.scan.as_ref().map(|scan| scan.is_finished());
        if finished != Some(true) {
            return;
        }
        let found = self.scan.take().map(|scan| scan.snapshot());
        if let (
            Some(found),
            Mode::Sync {
                pairs,
                cursor,
                capped,
                ..
            },
        ) = (found, &mut self.mode)
        {
            *pairs = found.pairs;
            *cursor = 0;
            *capped = found.truncated;
        }
    }

    fn on_key_sync(&mut self, key: KeyEvent) {
        self.collect_scan();
        let running = self.scan.is_some();
        let mut restart = false;
        let mut run = None;
        {
            let Mode::Sync {
                left,
                right,
                options,
                show,
                pairs,
                cursor,
                ..
            } = &mut self.mode
            else {
                return;
            };
            let showing: Vec<usize> = (0..pairs.len())
                .filter(|&i| show.allows(pairs[i].state))
                .collect();

            match key.code {
                KeyCode::Esc => {
                    // The thread stops with the form rather than walking two
                    // disks for a list nobody is looking at.
                    self.scan = None;
                    self.mode = Mode::Normal;
                    return;
                }
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => *cursor = (*cursor + 1).min(showing.len().saturating_sub(1)),
                KeyCode::PageUp => *cursor = cursor.saturating_sub(10),
                KeyCode::PageDown => *cursor = (*cursor + 10).min(showing.len().saturating_sub(1)),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = showing.len().saturating_sub(1),
                // The row under the cursor turns: right, left, leave it alone.
                KeyCode::Char(' ') => {
                    if let Some(&index) = showing.get(*cursor) {
                        pairs[index].turn();
                    }
                }
                // A thousand differences is not a thousand key presses. These
                // point every row on screen at once - and only the ones on
                // screen, so a filter narrows what "every" means. The tally
                // line under the list is the answer to "and now what?", so
                // there is nothing else to say afterwards.
                KeyCode::Right => {
                    compare::turn_all(
                        pairs,
                        &showing,
                        compare::Bulk::All(compare::Direction::ToRight),
                    );
                }
                KeyCode::Left => {
                    compare::turn_all(
                        pairs,
                        &showing,
                        compare::Bulk::All(compare::Direction::ToLeft),
                    );
                }
                KeyCode::Char('-') => {
                    compare::turn_all(
                        pairs,
                        &showing,
                        compare::Bulk::All(compare::Direction::Skip),
                    );
                }
                KeyCode::Char('*') => {
                    compare::turn_all(pairs, &showing, compare::Bulk::Suggested);
                }
                KeyCode::F(2) => restart = true,
                KeyCode::F(3) => {
                    options.by_content = !options.by_content;
                    restart = true;
                }
                KeyCode::F(4) => {
                    options.include_hidden = !options.include_hidden;
                    restart = true;
                }
                KeyCode::F(6) => {
                    options.recursive = !options.recursive;
                    restart = true;
                }
                // The files that already agree are not why the window is
                // open, so they start hidden and this brings them back.
                KeyCode::Char('=') => {
                    show.same = !show.same;
                    *cursor = 0;
                }
                KeyCode::F(5) if !running => {
                    let tasks = compare::tasks(pairs, left, right);
                    if !tasks.is_empty() {
                        run = Some(tasks);
                    }
                }
                _ => {}
            }
        }

        if restart {
            let Mode::Sync {
                left,
                right,
                options,
                pairs,
                cursor,
                capped,
                ..
            } = &mut self.mode
            else {
                return;
            };
            pairs.clear();
            *cursor = 0;
            *capped = false;
            self.scan = Some(compare::Scan::spawn(
                left.clone(),
                right.clone(),
                options.clone(),
            ));
        }
        if let Some(tasks) = run {
            self.scan = None;
            self.start_job(Operation::Sync { tasks });
        }
    }

    /// `Ctrl-N`: new names for the whole selection at once.
    pub fn open_multi_rename(&mut self) {
        let sources: Vec<rename::Source> = self
            .active_panel()
            .action_entries()
            .iter()
            .map(|entry| rename::Source::from_entry(entry))
            .collect();
        if sources.is_empty() {
            self.error("Nothing to rename");
            return;
        }
        let rules = rename::Rules::default();
        let changes = rename::plan(
            mount::Platform::current(),
            &sources,
            &rules,
            &lost_commander_core::preview::on_disk,
        );
        self.mode = Mode::MultiRename {
            rules,
            sources,
            changes,
            field: RenameField::Name,
            scroll: 0,
        };
    }

    fn on_key_multi_rename(&mut self, key: KeyEvent) {
        let mut go = None;
        {
            let Mode::MultiRename {
                rules,
                sources,
                changes,
                field,
                scroll,
            } = &mut self.mode
            else {
                return;
            };
            let before = rules.clone();

            match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    return;
                }
                KeyCode::Tab => *field = field.next(),
                KeyCode::BackTab => *field = field.prev(),
                // The preview scrolls; the form is walked with Tab, so the
                // arrows are free for the list, which is the part that gets
                // long enough to need them.
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = (*scroll + 1).min(changes.len().saturating_sub(1)),
                KeyCode::F(3) => rules.case_sensitive = !rules.case_sensitive,
                KeyCode::Left if *field == RenameField::Case => rules.case = rules.case.prev(),
                KeyCode::Right if *field == RenameField::Case => rules.case = rules.case.next(),
                KeyCode::Enter => {
                    let (moving, _) = rename::tally(changes);
                    if moving == 0 {
                        return;
                    }
                    go = Some(std::mem::take(changes));
                }
                KeyCode::Backspace => match field {
                    RenameField::Name => {
                        rules.name.pop();
                    }
                    RenameField::Extension => {
                        rules.extension.pop();
                    }
                    RenameField::Find => {
                        rules.find.pop();
                    }
                    RenameField::Replace => {
                        rules.replace.pop();
                    }
                    RenameField::Case => {}
                },
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => match field {
                    RenameField::Name => rules.name.push(c),
                    RenameField::Extension => rules.extension.push(c),
                    RenameField::Find => rules.find.push(c),
                    RenameField::Replace => rules.replace.push(c),
                    // Nothing to type here, so the space bar cycles it, which
                    // is what a space does to a checkbox everywhere else.
                    RenameField::Case if c == ' ' => rules.case = rules.case.next(),
                    RenameField::Case => {}
                },
                _ => {}
            }

            if *rules != before {
                *changes = rename::plan(
                    mount::Platform::current(),
                    sources,
                    rules,
                    &lost_commander_core::preview::on_disk,
                );
                *scroll = 0;
            }
        }

        if let Some(plan) = go {
            self.mode = Mode::Normal;
            self.run_multi_rename(&plan);
        }
    }

    /// Carry out a rename plan and say what came of it.
    fn run_multi_rename(&mut self, changes: &[rename::Change]) {
        let applied = rename::apply(changes);
        self.left.reload();
        self.right.reload();
        // The marks pointed at names that no longer exist. A reload puts back
        // the ones whose names happen to match, which after a rename means an
        // arbitrary few of them - so drop the lot instead.
        self.active_panel_mut().clear_marks();
        match applied.failures.split_first() {
            None => self.info(format!(
                "Renamed {} {}",
                applied.renamed,
                if applied.renamed == 1 {
                    "file"
                } else {
                    "files"
                }
            )),
            Some((first, rest)) => {
                let more = if rest.is_empty() {
                    String::new()
                } else {
                    format!(" (and {} more)", rest.len())
                };
                self.error(format!(
                    "Renamed {}, but {}: {}{more}",
                    applied.renamed, first.name, first.message
                ))
            }
        }
    }

    /// Go to a result: the panel moves to its directory, cursor on the file.
    ///
    /// Not "open it" - a search is how you find *where* something is, and
    /// landing next to it leaves the next thing you do open.
    pub fn go_to(&mut self, path: &Path) {
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            self.error("Nowhere to go");
            return;
        };
        self.active_panel_mut().chdir(parent);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let index = self
            .active_panel()
            .entries
            .iter()
            .position(|entry| entry.name == name);
        match index {
            Some(index) => {
                self.active_panel_mut().cursor_to(index);
                self.info(format!("Found {name}"));
            }
            None => self.error(format!("{name} is no longer there")),
        }
    }

    /// `Alt-Enter`: what this file is, and what it is allowed to be.
    pub fn open_properties(&mut self) {
        let target = self
            .active_panel()
            .selected()
            .filter(|entry| !entry.is_parent())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing selected");
            return;
        };
        match perms::read(&target) {
            Ok(properties) => {
                self.mode = Mode::Properties {
                    was: Box::new(properties.clone()),
                    now: Box::new(properties),
                    cursor: 0,
                }
            }
            Err(e) => self.error(format!("{}: {e}", target.display())),
        }
    }

    /// The nine permission boxes, in reading order.
    pub fn permission_at(index: usize) -> (Who, What) {
        let who = Who::ALL[(index / 3).min(2)];
        let what = What::ALL[index % 3];
        (who, what)
    }

    fn on_key_properties(&mut self, key: KeyEvent) {
        let Mode::Properties { now, cursor, .. } = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Left => *cursor = cursor.saturating_sub(1),
            KeyCode::Right => *cursor = (*cursor + 1).min(8),
            KeyCode::Up => *cursor = cursor.saturating_sub(3),
            KeyCode::Down => *cursor = (*cursor + 3).min(8),
            KeyCode::Char(' ') => {
                if let Some(mode) = &mut now.mode {
                    let (who, what) = Self::permission_at(*cursor);
                    let on = mode.is_set(who, what);
                    mode.set(who, what, !on);
                }
            }
            // The three special bits, which have no place in the grid.
            KeyCode::Char('u') => Self::toggle_special(now, perms::SETUID),
            KeyCode::Char('g') => Self::toggle_special(now, perms::SETGID),
            KeyCode::Char('t') => Self::toggle_special(now, perms::STICKY),
            KeyCode::Enter => {
                let Mode::Properties { was, now, .. } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    return;
                };
                self.apply_properties(&was, &now);
            }
            _ => {}
        }
    }

    fn toggle_special(now: &mut perms::Properties, special: u32) {
        if let Some(mode) = &mut now.mode {
            let on = mode.has(special);
            mode.set_special(special, !on);
        }
    }

    /// Write back only what was actually changed.
    ///
    /// A dialog that wrote every field back would touch files nobody edited.
    pub fn apply_properties(&mut self, was: &perms::Properties, now: &perms::Properties) {
        let mut wrote = Vec::new();
        let mut failed = None;

        if now.mode != was.mode {
            if let Some(mode) = now.mode {
                match perms::set_mode(&now.path, mode) {
                    Ok(()) => {
                        let before = was.mode.map(|m| m.octal()).unwrap_or_default();
                        self.note(
                            journal::Event::new(journal::Kind::Permissions, &now.path)
                                .note(format!("{before} -> {}", mode.octal())),
                        );
                        wrote.push(format!("permissions {}", mode.octal()));
                    }
                    Err(e) => failed = Some(e.to_string()),
                }
            }
        } else if now.readonly != was.readonly {
            // Only where there are no permission bits: on Unix the read-only
            // flag *is* the write bits, and writing both would fight.
            match perms::set_readonly(&now.path, now.readonly) {
                Ok(()) => {
                    self.note(
                        journal::Event::new(journal::Kind::Permissions, &now.path).note(
                            if now.readonly {
                                "read-only"
                            } else {
                                "writable"
                            },
                        ),
                    );
                    wrote.push("read-only".to_string());
                }
                Err(e) => failed = Some(e.to_string()),
            }
        }

        self.active_panel_mut().reload();
        match (failed, wrote.is_empty()) {
            (Some(e), _) => self.error(format!("{}: {e}", now.name())),
            (None, true) => self.info("Nothing changed"),
            (None, false) => self.info(format!("{}: set {}", now.name(), wrote.join(", "))),
        }
    }

    /// `Ctrl-E`: a shell with administrator privileges, where the panel is.
    pub fn root_shell(&mut self) {
        let cwd = self.active_panel().cwd.clone();
        let said = format!("root shell in {}", cwd.display());
        let elevation = elevate::root_shell(mount::Platform::current(), &cwd);
        self.run_elevated(elevation, &said);
    }

    /// Every key is an answer: the worker is asleep waiting for one, and a
    /// screen that could be dismissed without answering would leave the copy
    /// stopped for good. Escape means Cancel rather than "go away".
    fn on_key_overwrite(&mut self, key: KeyEvent) {
        let answer = match key.code {
            KeyCode::Char('o') | KeyCode::Char('O') => Answer::Overwrite,
            KeyCode::Char('a') | KeyCode::Char('A') => Answer::OverwriteAll,
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => Answer::Skip,
            KeyCode::Char('k') | KeyCode::Char('K') => Answer::SkipAll,
            // A rule rather than an answer: from here on the newer file wins
            // and the rest are skipped, without asking again.
            KeyCode::Char('n') | KeyCode::Char('N') => Answer::OnlyNewer,
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Esc => Answer::Cancel,
            _ => return,
        };
        if let Some(job) = &self.job {
            job.answer(answer);
        }
        self.mode = Mode::Progress;
    }

    fn on_key_open_with(&mut self, key: KeyEvent) {
        let Mode::OpenWith {
            applications,
            typed,
            cursor,
            as_admin,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let last = apps::matching(applications, typed).len().saturating_sub(1);

        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => *cursor = cursor.saturating_sub(1),
            KeyCode::Down => *cursor = (*cursor + 1).min(last),
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = last,
            KeyCode::Enter => self.run_chosen(),
            // Ctrl-A rather than a letter, since every letter here is typing.
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                *as_admin = !*as_admin;
            }
            KeyCode::Backspace => {
                typed.pop();
                *cursor = 0;
            }
            // Everything printable narrows the list, since the box is also
            // where a command that is not on it gets typed.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                typed.push(c);
                *cursor = 0;
            }
            _ => {}
        }
    }

    pub fn view_selected(&mut self) {
        let Some(entry) = self.active_panel().selected() else {
            return;
        };
        if entry.is_dir() {
            self.error("Cannot view a directory");
            return;
        }
        let path = entry.path.clone();
        let title = entry.name.clone();
        // A binary handed to a text viewer comes out as a screenful of
        // replacement characters, which looks like a bug rather than like a
        // binary. Shown as bytes it is at least the truth.
        if hex::is_binary(&path).unwrap_or(false) {
            match hex::Dump::open(&path) {
                Ok(dump) => {
                    self.mode = Mode::Bytes {
                        name: title,
                        dump,
                        scroll: 0,
                        editing: false,
                        cursor: hex::Cursor::default(),
                        edits: hex::Edits::default(),
                        goto: None,
                    };
                    return;
                }
                Err(e) => {
                    self.error(format!("View failed: {e}"));
                    return;
                }
            }
        }
        match fsops::read_preview(&path, PREVIEW_LIMIT, None) {
            Ok((lines, detected)) => {
                self.mode = Mode::Viewer {
                    title,
                    lines,
                    scroll: 0,
                    path,
                    forced: None,
                    detected,
                }
            }
            Err(e) => self.error(format!("View failed: {e}")),
        }
    }

    /// Read the file on show as the next encoding along.
    ///
    /// Cycles rather than opening a chooser: there are seven of them, the
    /// answer is obvious the moment the text stops being nonsense, and one
    /// key that can be pressed until it looks right beats a form.
    fn view_next_encoding(&mut self, back: bool) {
        let Mode::Viewer {
            path,
            forced,
            detected,
            ..
        } = &self.mode
        else {
            return;
        };
        let current = forced.unwrap_or(detected.encoding);
        let at = encoding::ALL
            .iter()
            .position(|e| *e == current)
            .unwrap_or(0);
        let next = if back {
            (at + encoding::ALL.len() - 1) % encoding::ALL.len()
        } else {
            (at + 1) % encoding::ALL.len()
        };
        let wanted = encoding::ALL[next];
        let path = path.clone();

        match fsops::read_preview(&path, PREVIEW_LIMIT, Some(wanted)) {
            Ok((new_lines, _)) => {
                if let Mode::Viewer {
                    lines,
                    forced,
                    scroll,
                    ..
                } = &mut self.mode
                {
                    *lines = new_lines;
                    *forced = Some(wanted);
                    // The same byte offset means a different line count in a
                    // different encoding, so a scroll position kept from the
                    // last one can be past the end.
                    *scroll = (*scroll).min(lines.len().saturating_sub(1));
                }
            }
            Err(e) => self.error(format!("View failed: {e}")),
        }
    }

    pub fn edit_selected(&mut self) {
        let Some(entry) = self.active_panel().selected() else {
            return;
        };
        if entry.is_dir() {
            self.error("Cannot edit a directory");
            return;
        }
        self.pending_edit = Some(entry.path.clone());
    }

    pub fn request_copy(&mut self) {
        let targets = self.active_panel().action_targets();
        if targets.is_empty() {
            self.error("Nothing selected");
            return;
        }
        let destination = self.other_panel().cwd.display().to_string();
        self.mode = Mode::Input(InputDialog {
            title: format!("Copy {}", describe(&targets)),
            prompt: "to directory:".into(),
            value: destination,
            action: InputAction::CopyTo(targets),
        });
    }

    pub fn request_move(&mut self) {
        let targets = self.active_panel().action_targets();
        if targets.is_empty() {
            self.error("Nothing selected");
            return;
        }
        let destination = self.other_panel().cwd.display().to_string();
        self.mode = Mode::Input(InputDialog {
            title: format!("Move {}", describe(&targets)),
            prompt: "to directory:".into(),
            value: destination,
            action: InputAction::MoveTo(targets),
        });
    }

    pub fn request_rename(&mut self) {
        let Some(entry) = self.active_panel().selected() else {
            return;
        };
        if entry.is_parent() {
            self.error("Cannot rename \"..\"");
            return;
        }
        let path = entry.path.clone();
        let value = entry.name.clone();
        self.mode = Mode::Input(InputDialog {
            title: "Rename".into(),
            prompt: "new name:".into(),
            value,
            action: InputAction::Rename(path),
        });
    }

    pub fn request_mkdir(&mut self) {
        self.mode = Mode::Input(InputDialog {
            title: "Create directory".into(),
            prompt: "name:".into(),
            value: String::new(),
            action: InputAction::MakeDir,
        });
    }

    pub fn request_delete(&mut self) {
        self.request_delete_with(true);
    }

    /// `Shift-F8` / `Shift-Del`: delete for good, skipping the trash.
    pub fn request_delete_forever(&mut self) {
        self.request_delete_with(false);
    }

    fn request_delete_with(&mut self, to_trash: bool) {
        let targets = self.active_panel().action_targets();
        if targets.is_empty() {
            self.error("Nothing selected");
            return;
        }
        // The two are different acts and the wording says which one it is:
        // "cannot be undone" was true of every delete before and is now only
        // true of this one.
        let (title, message) = if to_trash {
            (
                "Move to trash",
                format!("Move {} to the trash?", describe(&targets)),
            )
        } else {
            (
                "Delete for good",
                format!(
                    "Delete {} for good? This does not go to the trash and cannot be undone.",
                    describe(&targets)
                ),
            )
        };
        self.mode = Mode::Confirm(ConfirmDialog {
            title: title.into(),
            message,
            action: ConfirmAction::Delete { targets, to_trash },
        });
    }

    /// The grey `+` and `-`: mark or unmark everything matching a mask.
    fn request_select_pattern(&mut self, select: bool) {
        self.mode = Mode::Input(InputDialog {
            title: if select { "Select" } else { "Deselect" }.into(),
            prompt: format!(
                "Files to {} (mask, e.g. *.txt):",
                if select { "select" } else { "deselect" }
            ),
            // What the original offered, so Enter alone is "all of them".
            value: "*".to_string(),
            action: InputAction::SelectPattern { select },
        });
    }

    pub fn cycle_sort(&mut self) {
        let next = match self.active_panel().sort_by {
            SortBy::Name => SortBy::Ext,
            SortBy::Ext => SortBy::Size,
            SortBy::Size => SortBy::Time,
            SortBy::Time => SortBy::Name,
        };
        self.active_panel_mut().set_sort(next);
        self.info(format!("Sorted by {}", next.label()));
    }

    // ---- dialog completion -------------------------------------------------

    fn submit_input(&mut self) {
        let Mode::Input(dialog) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let value = dialog.value.trim().to_string();
        if value.is_empty() {
            self.error("Cancelled: empty input");
            return;
        }

        match dialog.action {
            InputAction::AddBookmark => match Location::parse(&value) {
                Ok(location) => {
                    let name = location.name.clone();
                    self.bookmarks.add(location);
                    self.persist_bookmarks();
                    if !self.status_is_error {
                        self.info(format!("Saved \"{name}\""));
                    }
                }
                Err(reason) => self.error(format!("Bad location: {reason}")),
            },
            InputAction::SelectPattern { select } => {
                let changed = self.active_panel_mut().mark_matching(&value, select);
                let marked = self.active_panel().marked_count();
                self.info(format!(
                    "{changed} {}, {marked} marked",
                    if select { "selected" } else { "deselected" }
                ));
            }
            InputAction::MakeDir => {
                let parent = self.active_panel().cwd.clone();
                match fsops::create_dir(&parent, &value) {
                    Ok(path) => {
                        self.note(journal::Event::new(journal::Kind::MakeDir, &path));
                        let name = value.clone();
                        self.active_panel_mut().reload();
                        // Put the cursor on what was just created.
                        let index = self
                            .active_panel()
                            .entries
                            .iter()
                            .position(|e| e.name == name);
                        if let Some(i) = index {
                            self.active_panel_mut().cursor_to(i);
                        }
                        self.info(format!("Created {}", path.display()));
                    }
                    Err(e) => self.error(format!("Create failed: {e}")),
                }
            }
            InputAction::Rename(path) => match fsops::rename(&path, &value) {
                Ok(to) => {
                    self.note(journal::Event::new(journal::Kind::Rename, &path).to(&to));
                    self.active_panel_mut().reload();
                    self.info(format!("Renamed to {value}"));
                }
                Err(e) => self.error(format!("Rename failed: {e}")),
            },
            // Copies and moves can take a while, so they run on a worker
            // thread behind the progress dialog.
            InputAction::CopyTo(targets) => match self.resolve_dir(&value) {
                Ok(destination) => self.start_job(Operation::Copy {
                    sources: targets,
                    destination,
                }),
                Err(message) => self.error(message),
            },
            InputAction::MoveTo(targets) => match self.resolve_dir(&value) {
                Ok(destination) => self.start_job(Operation::Move {
                    sources: targets,
                    destination,
                }),
                Err(message) => self.error(message),
            },
        }
    }

    /// Interpret dialog input as a directory, relative to the active panel.
    fn resolve_dir(&self, raw: &str) -> Result<PathBuf, String> {
        let candidate = PathBuf::from(raw);
        let path = if candidate.is_absolute() {
            candidate
        } else {
            self.active_panel().cwd.join(candidate)
        };
        if path.is_dir() {
            Ok(path)
        } else {
            Err(format!("Not a directory: {}", path.display()))
        }
    }

    fn confirm(&mut self, accepted: bool) {
        let Mode::Confirm(dialog) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        if !accepted {
            self.info("Cancelled");
            return;
        }
        match dialog.action {
            ConfirmAction::Delete { targets, to_trash } => {
                self.start_job(Operation::Delete { targets, to_trash })
            }
            ConfirmAction::Run(path) => {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                self.launch(&path, &name);
            }
        }
    }

    // ---- key dispatch ------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        // Any key that ends up changing a panel's directory feeds the Recent
        // list; hooking it here catches every route (Enter, Backspace, the
        // tree, bookmarks) without threading a call through each of them.
        let before = (
            self.left.cwd().to_path_buf(),
            self.right.cwd().to_path_buf(),
        );
        // Cleared before the key is handled, not after: whatever it says next
        // is about what is about to happen, and the last message was about
        // the previous keystroke. Anything set during the dispatch survives.
        self.status.clear();
        self.status_is_error = false;
        self.dispatch_key(key);
        // Whatever that did, the cursor has to end on a row the file half is
        // drawing. Under a tree it draws files only, so a cursor left on a
        // directory would be a selection nobody can see - and F5 would copy
        // something the reader never pointed at.
        self.snap_to_a_visible_row();
        self.record_visits(before.clone());
        // If the panel moved, the shell goes with it. Hooked here for the
        // same reason `record_visits` is: this catches every route - Enter,
        // Backspace, the tree, a bookmark - without a call threaded through
        // each of them.
        let now = (
            self.left.cwd().to_path_buf(),
            self.right.cwd().to_path_buf(),
        );
        if now != before {
            self.tell_the_shell();
        }
    }

    /// Give a key to the shell, and say whether it took it.
    ///
    /// Everything except Ctrl-O, which is how you get back: while the shell
    /// is on show it *is* the program, and a file manager reserving keys out
    /// of a running shell would break whatever is running in it. Ctrl-O is
    /// the one toll, and it is the same key that got you here.
    fn shell_key(&mut self, key: KeyEvent) -> bool {
        if !self.showing_shell {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('o')) {
            self.toggle_shell_view();
            return true;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Named, not coded: the engine holds the table saying what each key
        // sends, so this window and the graphical one agree about it.
        let name = match key.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Esc => "Escape".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Insert => "Insert".to_string(),
            KeyCode::Delete => "Delete".to_string(),
            _ => return true,
        };
        if let Some(bytes) = lost_commander_core::termview::key_bytes(&name, ctrl, alt) {
            if let Some(shell) = self.shell.as_mut() {
                shell.write(&bytes);
            }
        }
        true
    }

    fn dispatch_key(&mut self, key: KeyEvent) {
        if self.shell_key(key) {
            return;
        }
        match &mut self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::Input(_) => self.on_key_input(key),
            Mode::Confirm(_) => self.on_key_confirm(key),
            Mode::Viewer { .. } => self.on_key_viewer(key),
            Mode::Bytes { .. } => self.on_key_bytes(key),
            Mode::Journal { .. } => self.on_key_journal(key),
            Mode::Connections { .. } => self.on_key_connections(key),
            Mode::Shells { .. } => self.on_key_shells(key),
            Mode::OpenWith { .. } => self.on_key_open_with(key),
            Mode::Overwrite { .. } => self.on_key_overwrite(key),
            Mode::Find { .. } => self.on_key_find(key),
            Mode::Sync { .. } => self.on_key_sync(key),
            Mode::Duplicates { .. } => self.on_key_duplicates(key),
            Mode::Difference { .. } => self.on_key_difference(key),
            Mode::MultiRename { .. } => self.on_key_multi_rename(key),
            Mode::Properties { .. } => self.on_key_properties(key),
            // The only thing you can do to a running job is stop it.
            Mode::Progress => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.cancel_job();
                }
            }
            Mode::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) | KeyCode::Char('q') => {
                    self.mode = Mode::Normal
                }
                // The list is longer than a short terminal, so it moves.
                // Held here to the length of the list, and to what is actually
                // off-screen where it is drawn - which is the only place that
                // knows how much of it fits.
                KeyCode::Down => *scroll = (*scroll + 1).min(crate::ui::HELP.len()),
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll = (*scroll + 10).min(crate::ui::HELP.len()),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::Home => *scroll = 0,
                _ => {}
            },
        }
    }

    // ---- tree mode ---------------------------------------------------------

    pub fn toggle_tree(&mut self) {
        let index = self.side_index();
        if self.active_panel().in_tree_mode() {
            self.active_panel_mut().leave_tree_mode();
            self.on_tree[index] = false;
            self.info("Tree closed");
        } else {
            self.active_panel_mut().enter_tree_mode();
            // Asking for the tree puts the keyboard in it. Having to press
            // Tab to reach the thing you just asked for reads as the key not
            // having worked.
            self.on_tree[index] = true;
            self.info("Tree above, files below. Enter opens one, Escape comes back, Alt-T closes");
        }
    }

    /// Tree-specific keys. Returns true when the key was consumed, so keys the
    /// tree does not care about (Tab, F10, ...) still reach the normal handler.
    /// Put the cursor on a row the file half is actually showing.
    fn snap_to_a_visible_row(&mut self) {
        for side in [Side::Left, Side::Right] {
            let panel = match side {
                Side::Left => self.left.current(),
                Side::Right => self.right.current(),
            };
            if !panel.in_tree_mode() {
                continue;
            }
            let visible =
                |entry: &lost_commander_core::entry::Entry| !entry.is_dir() && !entry.is_parent();
            if panel.entries.get(panel.cursor).is_some_and(visible) {
                continue;
            }
            let cursor = panel.cursor;
            // Nearest first, downwards on a tie: jumping to the top of the
            // listing every time would throw away where the reader was.
            let next = panel
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| visible(entry))
                .min_by_key(|(index, _)| (index.abs_diff(cursor), usize::from(*index < cursor)))
                .map(|(index, _)| index);
            if let Some(index) = next {
                match side {
                    Side::Left => self.left.current_mut().cursor_to(index),
                    Side::Right => self.right.current_mut().cursor_to(index),
                }
            }
        }
    }

    /// The shell, started if this is the first thing to want one.
    ///
    /// In the directory the panel is showing, which is where a reader typing
    /// a command means it to run. Hooked where the shell has a seam for it -
    /// `PtySession::spawn` does that itself - which is what makes
    /// `shell_cwd` able to answer later.
    fn shell_now(&mut self) -> bool {
        if self.shell.is_some() {
            return true;
        }
        let program = self
            .shell_program
            .clone()
            .unwrap_or_else(|| lost_commander_core::shell::current_shell().0);
        let cwd = self.active_panel().cwd.clone();
        match lost_commander_core::pty::PtySession::spawn(&program, &cwd, 24, 80) {
            Ok(session) => {
                self.shell = Some(session);
                true
            }
            Err(e) => {
                self.error(format!("Could not start {program}: {e}"));
                false
            }
        }
    }

    /// Hand a line to the shell and show it working.
    pub fn send_to_shell(&mut self, line: &str) {
        if !self.shell_now() {
            return;
        }
        let program = self.shell_choice();
        let cwd = self.active_panel().cwd.clone();
        let lines = command_lines(&program, &cwd, line);
        if let Some(shell) = self.shell.as_mut() {
            for line in &lines {
                shell.write(line.as_bytes());
                shell.write(ENTER);
            }
        }
        // Onto the shell's screen, because that is where the answer will
        // appear. Watching a command run is the normal case; Ctrl-O comes
        // back to the panels.
        self.showing_shell = true;
    }

    // With a shell that cannot say where it is, the panel is the answer.

    /// Ctrl-O: swap between the panels and the shell underneath them.
    pub fn toggle_shell_view(&mut self) {
        if !self.showing_shell && !self.shell_now() {
            return;
        }
        self.showing_shell = !self.showing_shell;
        if !self.showing_shell {
            self.follow_the_shell();
            self.info("Panels. Ctrl-O returns to the shell.");
        }
    }

    /// Move the panel to wherever the shell has got to.
    ///
    /// The shell reports where it is through the hook, so this is reading an
    /// answer rather than guessing at one. Only when it has actually moved,
    /// and only for the active panel: a `cd` is something the reader did, and
    /// following it is what makes the command line and the panels one program
    /// instead of two things sharing a window.
    pub fn follow_the_shell(&mut self) {
        let Some(where_) = self.shell.as_ref().and_then(|s| s.shell_cwd()) else {
            return;
        };
        if where_ == self.active_panel().cwd || !where_.is_dir() {
            return;
        }
        self.active_panel_mut().chdir(where_.clone());
        self.info(format!("Followed the shell to {}", where_.display()));
    }

    /// Tell the shell where the panel has gone.
    ///
    /// The other direction, and safe to do without asking because it only
    /// happens while the panels have the keyboard - which means the shell is
    /// sitting at a prompt with nothing half-typed for this to interrupt.
    pub fn tell_the_shell(&mut self) {
        let program = self.shell_choice();
        // Only a shell that can answer. One that cannot is sent to the
        // panel's directory before each command instead, and writing a `cd`
        // into its screen every time somebody moved would fill it with
        // commands nobody typed.
        if !lost_commander_core::shellhook::journals(&program) {
            return;
        }
        let Some(shell) = self.shell.as_mut() else {
            return;
        };
        let cwd = match self.active {
            Side::Left => self.left.current().cwd.clone(),
            Side::Right => self.right.current().cwd.clone(),
        };
        if shell.shell_cwd().as_deref() == Some(cwd.as_path()) {
            return;
        }
        // Quoted the way *this* shell quotes. `cd 'C:\src'` is an error in
        // cmd, which has no single quotes and reads them as part of the name
        // - and cmd will not cross drives without `/d`, which is the first
        // thing a file manager asks it to do.
        let line = lost_commander_core::shell::cd_command(&program, &cwd);
        shell.write(line.as_bytes());
        shell.write(ENTER);
    }

    /// Alt-O: choose the shell that runs underneath the panels.
    pub fn open_shell_picker(&mut self) {
        let mut shells = lost_commander_core::shell::discover_shells();
        // Whatever is in use belongs on the list even if the search missed
        // it - somebody who named a shell by hand should see it, not wonder
        // where it went.
        let current = self.shell_choice();
        if !shells.contains(&current) {
            shells.insert(0, current.clone());
        }
        let cursor = shells.iter().position(|s| *s == current).unwrap_or(0);
        self.mode = Mode::Shells { shells, cursor };
    }

    /// The shell that would run now.
    pub fn shell_choice(&self) -> String {
        self.shell_program
            .clone()
            .unwrap_or_else(|| lost_commander_core::shell::current_shell().0)
    }

    fn on_key_shells(&mut self, key: KeyEvent) {
        let Mode::Shells { shells, cursor } = &mut self.mode else {
            return;
        };
        let last = shells.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => *cursor = cursor.saturating_sub(1),
            KeyCode::Down => *cursor = (*cursor + 1).min(last),
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = last,
            KeyCode::Esc | KeyCode::F(10) => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let chosen = shells.get(*cursor).cloned();
                self.mode = Mode::Normal;
                if let Some(chosen) = chosen {
                    self.use_shell(chosen);
                }
            }
            _ => {}
        }
    }

    /// Run this shell from now on, and remember it.
    ///
    /// The one already running is dropped rather than kept: a reader who has
    /// just chosen a different shell means the next command to go to it, and
    /// two shells with one command line would be a coin toss.
    fn use_shell(&mut self, program: String) {
        self.shell_program = Some(program.clone());
        self.shell = None;
        self.showing_shell = false;

        let mut settings = lost_commander_core::config::Settings::load();
        settings.shell = Some(program.clone());
        if let Err(e) = settings.save() {
            self.error(format!("Chose {program}, but could not save it: {e}"));
            return;
        }
        let name = lost_commander_core::shell::program_name(&program);
        if lost_commander_core::shellhook::journals(&program) {
            self.info(format!(
                "{name} from now on. It reports where it is, so the panel and the shell share a directory."
            ));
        } else {
            self.info(format!(
                "{name} from now on - {}",
                lost_commander_core::shellhook::why_not()
            ));
        }
    }

    /// Ctrl-C: stop whatever is going on, and if nothing is, leave.
    ///
    /// In that order, because the reflex means "stop" and what wants stopping
    /// is whatever is most immediate. A copy running is the loudest thing on
    /// screen; a half-typed command is the next; and with neither, there is
    /// nothing to interrupt except the program itself, which is what the
    /// keystroke means everywhere else in a terminal.
    fn interrupt(&mut self) {
        if self.job_is_running() {
            self.cancel_job();
            return;
        }
        if !self.command.is_empty() {
            self.command.clear();
            self.info("Command cleared. Ctrl-C again to quit.");
            return;
        }
        self.should_quit = true;
    }

    fn side_index(&self) -> usize {
        match self.active {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    /// Escape in the file half: back up to the tree above it.
    ///
    /// The other half of what Enter does coming down, and the reason Tab is
    /// left alone - Tab means the other pane, here and everywhere else, and a
    /// key whose meaning depends on state is one you stop trusting.
    ///
    /// Returns whether it took the key.
    fn back_up_to_the_tree(&mut self) -> bool {
        let index = self.side_index();
        if self.active_panel().in_tree_mode() && !self.on_tree[index] {
            self.on_tree[index] = true;
            self.info("The tree. Enter opens a directory, Alt-T closes.");
            return true;
        }
        false
    }

    fn handle_tree_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('t') {
            self.toggle_tree();
            return true;
        }

        let Some(tree) = self.active_panel_mut().tree.as_mut() else {
            return false;
        };
        let cursor = tree.cursor;

        match key.code {
            KeyCode::Up => tree.move_cursor(-1),
            KeyCode::Down => tree.move_cursor(1),
            KeyCode::PageUp => tree.move_cursor(-15),
            KeyCode::PageDown => tree.move_cursor(15),
            KeyCode::Home => tree.cursor_home(),
            KeyCode::End => tree.cursor_end(),
            KeyCode::Right | KeyCode::Char('+') => tree.expand(cursor),
            KeyCode::Char(' ') => tree.toggle(cursor),
            KeyCode::Left | KeyCode::Char('-') => {
                // Collapse if open, otherwise step out to the parent.
                let expanded = tree.selected().map(|n| n.expanded).unwrap_or(false);
                if expanded {
                    tree.collapse(cursor);
                } else if let Some(parent) = tree.parent_of(cursor) {
                    tree.cursor = parent;
                }
            }
            KeyCode::Enter => {
                // Move the panel to the highlighted directory and *keep* the
                // tree. Closing it made this a directory chooser rather than
                // a tree you work in - and walking from one directory to the
                // next tagging files as you go is the whole reason to have
                // one.
                if let Some(path) = tree.selected_path() {
                    // And down into what was just opened: Enter means "show
                    // me what is in here", and the answer is the half below.
                    let index = match self.active {
                        Side::Left => 0,
                        Side::Right => 1,
                    };
                    self.on_tree[index] = false;
                    self.active_panel_mut().chdir(path.clone());
                    if let Some(error) = self.active_panel().error.clone() {
                        self.error(format!("{}: {error}", path.display()));
                    } else {
                        self.info(format!("Moved to {}", path.display()));
                    }
                }
            }
            KeyCode::Esc => {
                self.active_panel_mut().leave_tree_mode();
                self.on_tree[self.side_index()] = false;
                self.info("Tree closed");
            }
            KeyCode::Char('r') if ctrl => tree.refresh(),
            _ => return false,
        }
        true
    }

    /// Give a key to the command line, and say whether it took it.
    ///
    /// The rule is Midnight Commander's, and it is the one that makes a
    /// command line and a file panel share a keyboard without either being
    /// crippled: **an empty command line means you are working the panels.**
    /// Space marks a file when there is nothing typed and is a space once
    /// there is; the same for the selection keys. Backspace goes up a
    /// directory until there is something to delete.
    ///
    /// Letters are the exception in the other direction: they always type.
    /// `q` used to quit, and does not any more - F10 does, as it always has,
    /// and as it does in both of the programs this follows. A `q` that quit
    /// would make `qemu` unwritable.
    fn command_key(&mut self, code: KeyCode) -> bool {
        let empty = self.command.is_empty();
        match code {
            KeyCode::Char(' ') if empty => false,
            KeyCode::Char('+') | KeyCode::Char('-') | KeyCode::Char('*') if empty => false,
            KeyCode::Char(character) => {
                self.command.push(character);
                true
            }
            KeyCode::Backspace if !empty => {
                self.command.pop();
                true
            }
            KeyCode::Enter if !empty => {
                self.run_command();
                true
            }
            // Escape clears what is typed rather than leaving it to be
            // deleted a character at a time.
            KeyCode::Esc if !empty => {
                self.command.clear();
                true
            }
            _ => false,
        }
    }

    /// Hand the typed line to a shell, in the directory being shown.
    ///
    /// Through the same suspend-and-restore the editor and privileged
    /// commands use: the command gets the real terminal, so it can be
    /// interactive, print colour, and be read afterwards - none of which is
    /// true of output captured into a box.
    fn run_command(&mut self) {
        let line = std::mem::take(&mut self.command);
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        // Written down, like everything else this program does on the
        // reader's behalf. The graphical view records what its shell runs by
        // asking the shell; here the line is known before it is handed over,
        // which is the one case where nothing has to be inferred.
        if let Some(journal) = &self.journal {
            let where_ = self.active_panel().cwd.display().to_string();
            journal.record(journal::Event::new(journal::Kind::Command, where_).note(&line));
        }
        // Into the shell that is already running, not a fresh one. A new
        // shell per command starts wherever it is put and forgets everything
        // the last one did, which makes `cd` a command that appears to work
        // and then has no effect on anything after it.
        self.send_to_shell(&line);
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        // The tree takes navigation keys first; anything it ignores falls
        // through to the usual bindings.
        // Only while the tree half has the keyboard: the files below it are
        // an ordinary listing, and an arrow key there means what it means
        // everywhere else.
        if self.active_panel().in_tree_mode()
            && self.on_tree[self.side_index()]
            && self.handle_tree_key(key)
        {
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // The command line takes ordinary typing, as it does in Norton and
        // Midnight Commander. Function keys and anything with Ctrl or Alt
        // behind it still work the panels, which is why those are the
        // bindings for everything that matters.
        if !ctrl && !alt && self.command_key(key.code) {
            return;
        }

        match key.code {
            KeyCode::Tab => self.switch_panel(),
            // Only reaches here with nothing typed - the command line takes
            // Escape first, to clear itself.
            KeyCode::Esc => {
                self.back_up_to_the_tree();
            }
            KeyCode::Up => self.active_panel_mut().move_cursor(-1),
            KeyCode::Down => self.active_panel_mut().move_cursor(1),
            // Before the unguarded pair: match arms are tried in order.
            // Ctrl-PageUp/PageDown walk the tabs, as they do in every
            // browser. Ctrl-Tab would be the other convention, but a
            // terminal cannot tell it from a plain Tab.
            KeyCode::PageUp if ctrl => self.previous_tab(),
            KeyCode::PageDown if ctrl => self.next_tab(),
            KeyCode::PageUp => self.active_panel_mut().move_cursor(-15),
            KeyCode::PageDown => self.active_panel_mut().move_cursor(15),
            KeyCode::Home => self.active_panel_mut().cursor_home(),
            KeyCode::End => self.active_panel_mut().cursor_end(),
            // Before the plain Enter, or the unguarded arm takes it too.
            KeyCode::Enter if alt => self.open_properties(),
            KeyCode::Enter => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left => self.active_panel_mut().go_parent(),
            KeyCode::Right => {
                if self.active_panel().selected().map(|e| e.is_dir()) == Some(true) {
                    self.active_panel_mut().enter();
                }
            }
            KeyCode::Insert | KeyCode::Char(' ') => self.active_panel_mut().toggle_mark(),

            KeyCode::Char('*') => {
                let panel = self.active_panel_mut();
                for e in panel.entries.iter_mut() {
                    if !e.is_parent() {
                        e.marked = !e.marked;
                    }
                }
            }
            // The grey plus and minus asked for a mask - they did not mark
            // everything. `*` and Enter is still two keystrokes for that, and
            // Ctrl-A is there for anyone who wants it in one.
            KeyCode::Char('+') => self.request_select_pattern(true),
            KeyCode::Char('-') => self.request_select_pattern(false),
            // J for journal: the account of what was done.
            // Ctrl-O is what Norton and Midnight Commander both use for this,
            // and it is the reason a command does not have to pause: the
            // output is still on the screen underneath, whenever you want it.
            KeyCode::Char('o') if ctrl => self.toggle_shell_view(),
            KeyCode::Char('j') if ctrl => self.open_journal(),
            KeyCode::Char('a') if ctrl => {
                let panel = self.active_panel_mut();
                for entry in panel.entries.iter_mut() {
                    if !entry.is_parent() {
                        entry.marked = true;
                    }
                }
                let marked = self.active_panel().marked_count();
                self.info(format!("{marked} marked"));
            }

            KeyCode::F(1) => self.mode = Mode::Help { scroll: 0 },
            // Before the plain F2, or the unguarded arm takes the shifted
            // press too. Total Commander puts the multi-rename tool on
            // Ctrl-M, which a terminal cannot tell from Enter - so here it is
            // F2 with a shift behind it: rename, but the whole selection.
            KeyCode::F(2) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.open_multi_rename()
            }
            KeyCode::F(2) => self.request_rename(),
            KeyCode::F(3) => self.view_selected(),
            // Before the plain F8/Del, for the same reason as F4 below.
            KeyCode::F(8) | KeyCode::Delete if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.request_delete_forever()
            }
            // Before the plain F4: match arms are tried in order, so the
            // unguarded one would take the shifted press as well.
            KeyCode::F(4) if key.modifiers.contains(KeyModifiers::SHIFT) => self.edit_as_admin(),
            KeyCode::F(4) => self.edit_selected(),
            KeyCode::F(5) => self.request_copy(),
            // Before the plain F6, or the unguarded arm takes the shifted
            // press too. F6 sends a file to the other pane; F6 with a shift
            // behind it sends the whole tab.
            KeyCode::F(6) if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_tab_across(),
            KeyCode::F(6) => self.request_move(),
            // Before the plain F7, or the unguarded arm takes it: match arms
            // are tried in order.
            KeyCode::F(7) if alt => self.open_find(),
            KeyCode::F(7) => self.request_mkdir(),
            KeyCode::F(8) | KeyCode::Delete => self.request_delete(),
            KeyCode::F(9) => self.cycle_sort(),
            // Not every terminal sends F11, so Ctrl-B is offered as an alias.
            KeyCode::F(11) => self.open_connections(),
            KeyCode::Char('b') if ctrl => self.open_connections(),
            KeyCode::Char('d') if ctrl => self.bookmark_current_dir(),
            // The comparison family, on one modifier: C marks what differs
            // between the panes, D shows what two files differ by, S opens the
            // recursive one that can act on it.
            //
            // The graphical view also has the mnemonic Shift-F3 for D, which a
            // terminal cannot: the escape sequence for Shift-F3 is `CSI 1;2R`,
            // and `CSI <row>;<col> R` is the cursor-position report - crossterm
            // reads it as one and it never arrives as a key at all. Shift-F2
            // and Shift-F4 are `CSI 1;2Q` and `CSI 1;2S`, which clash with
            // nothing; F3 is the one unlucky letter of the four.
            KeyCode::Char('c') if alt => self.compare_folders(),
            KeyCode::Char('d') if alt => self.open_difference(),
            KeyCode::Char('s') if alt => self.open_sync(),
            KeyCode::Char('u') if alt => self.open_duplicates(),
            // Ctrl-T opens a tab in every program that has them, so that is
            // what it does here; the tree, which had it, moves to Alt-T.
            KeyCode::Char('t') if ctrl => self.new_tab(),
            KeyCode::Char('t') if alt => self.toggle_tree(),
            // Pairs with Ctrl-O, which shows the shell: one letter for
            // looking at it and for choosing which one it is.
            KeyCode::Char('o') if alt => self.open_shell_picker(),
            KeyCode::Char('w') if ctrl => self.close_tab(),
            KeyCode::Char('w') if alt => self.close_other_tabs(),
            // F10 is the Commander key for this and stays, but a terminal
            // may never let it through - GNOME Terminal opens its own menu
            // with F10, and some emulators are configured to close the window
            // outright. So there is a second way that no terminal claims.
            KeyCode::F(10) => self.should_quit = true,
            KeyCode::Char('q') if ctrl => self.should_quit = true,
            // What everybody's hands do to leave a terminal program. In raw
            // mode this arrives as a keystroke rather than a signal, so it is
            // ours to answer - and answering nothing is how a program traps
            // somebody in it.
            KeyCode::Char('c') if ctrl => self.interrupt(),
            // And what everybody's hands do to put one in the background.
            // Also a keystroke here rather than a signal; the main loop does
            // the actual stopping, because it owns the terminal.
            KeyCode::Char('z') if ctrl => self.pending_suspend = true,
            KeyCode::Char('h') if ctrl => {
                self.active_panel_mut().toggle_hidden();
                let showing = self.active_panel().show_hidden;
                self.info(if showing {
                    "Showing hidden files"
                } else {
                    "Hiding hidden files"
                });
            }
            KeyCode::Char('.') if alt => {
                self.active_panel_mut().toggle_hidden();
            }
            KeyCode::Char('r') if ctrl => {
                self.reload_both();
                self.info("Reloaded");
            }
            KeyCode::Char('u') if ctrl => {
                self.swap_panels();
                self.info("Panels swapped");
            }
            KeyCode::Char('p') if ctrl => self.open_with(),
            KeyCode::Char('e') if ctrl => self.root_shell(),
            KeyCode::Char('f') if ctrl => self.open_find(),
            _ => {}
        }
    }

    fn on_key_input(&mut self, key: KeyEvent) {
        let Mode::Input(dialog) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.info("Cancelled");
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => {
                dialog.value.pop();
            }
            KeyCode::Char(c) => dialog.value.push(c),
            _ => {}
        }
    }

    fn on_key_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm(true),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => self.confirm(false),
            _ => {}
        }
    }

    fn on_key_connections(&mut self, key: KeyEvent) {
        let Mode::Connections { tab, cursor } = &mut self.mode else {
            return;
        };
        let tab_value = *tab;
        let cursor_value = *cursor;
        let count = match tab_value {
            ConnTab::Saved => self.bookmarks.len(),
            ConnTab::Recent => self.bookmarks.recent.len(),
            ConnTab::System => self.system_places.len(),
        };

        match key.code {
            KeyCode::Esc | KeyCode::F(11) | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.mode = Mode::Connections {
                    tab: tab_value.other(),
                    cursor: 0,
                };
            }
            KeyCode::Up => *cursor = cursor_value.saturating_sub(1),
            KeyCode::Down => {
                if cursor_value + 1 < count {
                    *cursor = cursor_value + 1;
                }
            }
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = count.saturating_sub(1),

            KeyCode::Enter => match tab_value {
                ConnTab::Saved => {
                    if count == 0 {
                        self.error("No saved locations - press 'a' to add one");
                    } else {
                        self.connect_bookmark(cursor_value);
                    }
                }
                ConnTab::System => {
                    if let Some(place) = self.system_places.get(cursor_value) {
                        let path = place.path.clone();
                        self.mode = Mode::Normal;
                        self.active_panel_mut().chdir(path.clone());
                        if let Some(error) = self.active_panel().error.clone() {
                            self.error(format!("{}: {error}", path.display()));
                        }
                    }
                }
                ConnTab::Recent => {
                    if count == 0 {
                        self.error("Nowhere visited yet");
                    } else {
                        self.open_recent(cursor_value);
                    }
                }
            },

            KeyCode::Char('a') => {
                self.mode = Mode::Input(InputDialog {
                    title: "Add location".into(),
                    prompt: "URL or path (smb://user@host/share, ftp://host/pub, /local/dir):"
                        .into(),
                    value: String::new(),
                    action: InputAction::AddBookmark,
                });
            }
            KeyCode::Char('c') => {
                self.bookmark_current_dir();
                self.mode = Mode::Connections {
                    tab: ConnTab::Saved,
                    cursor: 0,
                };
            }
            // Promote something from the history into the saved list.
            KeyCode::Char('s') if tab_value == ConnTab::Recent => {
                if let Some(location) = self.bookmarks.recent.get(cursor_value).cloned() {
                    let name = location.name.clone();
                    self.bookmarks.add(location);
                    self.persist_bookmarks();
                    if !self.status_is_error {
                        self.info(format!("Saved \"{name}\""));
                    }
                }
                self.mode = Mode::Connections {
                    tab: tab_value,
                    cursor: cursor_value,
                };
            }
            // Forget the whole history at once.
            KeyCode::Char('C') if tab_value == ConnTab::Recent => {
                let forgotten = self.bookmarks.recent.len();
                self.bookmarks.clear_recent();
                self.persist_bookmarks();
                if !self.status_is_error {
                    self.info(format!("Forgot {forgotten} recent location(s)"));
                }
                self.mode = Mode::Connections {
                    tab: tab_value,
                    cursor: 0,
                };
            }
            KeyCode::Char('u') if tab_value == ConnTab::Saved => {
                if count > 0 {
                    self.disconnect_bookmark(cursor_value);
                    self.mode = Mode::Connections {
                        tab: tab_value,
                        cursor: cursor_value,
                    };
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if count > 0 {
                    match tab_value {
                        ConnTab::Saved => self.remove_bookmark(cursor_value),
                        ConnTab::Recent => {
                            self.bookmarks.remove_recent(cursor_value);
                        }
                        // A drive is not ours to forget.
                        ConnTab::System => {}
                    }
                    let remaining = match tab_value {
                        ConnTab::Saved => self.bookmarks.len(),
                        ConnTab::Recent => self.bookmarks.recent.len(),
                        ConnTab::System => self.system_places.len(),
                    };
                    self.mode = Mode::Connections {
                        tab: tab_value,
                        cursor: cursor_value.min(remaining.saturating_sub(1)),
                    };
                }
            }
            _ => {}
        }
    }

    /// Go to a remembered location, re-attaching the share if it is one.
    pub fn open_recent(&mut self, index: usize) {
        let Some(location) = self.bookmarks.recent.get(index).cloned() else {
            self.error("No such entry");
            return;
        };
        match mount::connect(&location) {
            Ok(path) => {
                self.mode = Mode::Normal;
                if location.protocol.is_network() {
                    self.active_mounts.retain(|(p, _)| p != &path);
                    self.active_mounts.push((path.clone(), location.clone()));
                }
                self.active_panel_mut().chdir(path.clone());
                if let Some(error) = self.active_panel().error.clone() {
                    self.error(format!("{}: {error}", path.display()));
                } else {
                    self.info(format!("Moved to {}", path.display()));
                }
            }
            Err(reason) => self.error(reason),
        }
    }

    fn on_key_bytes(&mut self, key: KeyEvent) {
        // Two modes in one window. Reading, the letters are shortcuts and the
        // arrows scroll; editing, the letters are hex digits and the arrows
        // move a cursor. Keeping them apart is what stops `q` from writing a
        // 0x?? into a file somebody was only looking at.
        let mut write = false;
        let Mode::Bytes {
            dump,
            scroll,
            editing,
            cursor,
            edits,
            goto,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let last_row = dump.rows().saturating_sub(1);
        let size = dump.size;
        let per_row = hex::PER_ROW as i64;

        // Typing an offset takes the keyboard while it is happening, in both
        // reading and editing: `f` is a hex digit in one and a byte in the
        // other, and it cannot be both at once.
        if let Some(typed) = goto {
            match key.code {
                KeyCode::Esc => *goto = None,
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Enter => {
                    if let Some(at) = hex::parse_offset(typed, size) {
                        cursor.to(at, size);
                        *scroll = (at / hex::PER_ROW as u64).min(last_row);
                        *goto = None;
                    }
                    // A refused offset stays on screen to be corrected,
                    // rather than being wiped so it can be retyped in full.
                }
                KeyCode::Char(character) => typed.push(character),
                _ => {}
            }
            return;
        }

        if !*editing {
            match key.code {
                KeyCode::Esc | KeyCode::F(3) | KeyCode::F(10) | KeyCode::Char('q') => {
                    self.mode = Mode::Normal
                }
                KeyCode::Down => *scroll = (*scroll + 1).min(last_row),
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll = (*scroll + 20).min(last_row),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                KeyCode::Home => *scroll = 0,
                KeyCode::End => *scroll = last_row,
                // `g` for go-to while reading; Ctrl-G as well, since that is
                // what it is called in the graphical view and in every
                // debugger.
                KeyCode::Char('g') | KeyCode::Char('G') => *goto = Some(String::new()),
                // Into edit mode, with the cursor where the eye already is.
                KeyCode::F(4) if size > 0 => {
                    *editing = true;
                    cursor.to(*scroll * hex::PER_ROW as u64, size);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            // Esc leaves editing rather than the window, so a half-finished
            // edit is one keystroke from being looked at rather than gone.
            KeyCode::Esc => *editing = false,
            KeyCode::Left => cursor.step(-1, size),
            KeyCode::Right => cursor.step(1, size),
            KeyCode::Up => cursor.step(-per_row, size),
            KeyCode::Down => cursor.step(per_row, size),
            KeyCode::PageUp => cursor.step(-per_row * 16, size),
            KeyCode::PageDown => cursor.step(per_row * 16, size),
            KeyCode::Home => cursor.to(0, size),
            KeyCode::End => cursor.to(size.saturating_sub(1), size),
            KeyCode::Tab => cursor.pane = cursor.pane.other(),
            KeyCode::Backspace => {
                if let Some(at) = edits.undo() {
                    cursor.to(at, size);
                }
            }
            KeyCode::F(2) => write = true,
            KeyCode::Char(character) => {
                let at = cursor.at;
                // What is there now: the pending edit if any, else the file.
                let current = edits.get(at).or_else(|| byte_at(dump, at)).unwrap_or(0);
                let was = byte_at(dump, at).unwrap_or(current);
                match cursor.pane {
                    hex::Pane::Hex => {
                        if let Some(digit) = hex::hex_digit(character) {
                            let now = hex::with_nibble(current, digit, cursor.low);
                            edits.set(at, was, now);
                            // Half a byte at a time, and on to the next byte
                            // once both halves have been typed.
                            if cursor.low {
                                cursor.step(1, size);
                            } else {
                                cursor.low = true;
                            }
                        }
                    }
                    hex::Pane::Text => {
                        // One keystroke to a byte here, for patching a string
                        // without doing the ASCII table in your head.
                        if !character.is_control() && character.is_ascii() {
                            edits.set(at, was, character as u8);
                            cursor.step(1, size);
                        }
                    }
                }
            }
            _ => {}
        }

        // Keep the cursor on screen, whichever way it moved.
        let row = cursor.row();
        if row < *scroll {
            *scroll = row;
        }

        if write {
            self.write_bytes();
        }
    }

    /// `Ctrl-J`: what was done, and when.
    pub fn open_journal(&mut self) {
        let Some(journal) = &self.journal else {
            self.error("No account is being kept");
            return;
        };
        let shown = journal::Shown::default();
        let days = journal.days_shown(shown);
        let rows = match days.first() {
            Some(day) => journal::arrange(journal.read_shown(shown, *day)),
            None => Vec::new(),
        };
        self.mode = Mode::Journal {
            shown,
            days,
            at: 0,
            rows,
            filter: journal::Filter::default(),
            cursor: 0,
            searching: false,
        };
    }

    /// Read whichever day and view the keys have moved to.
    fn reload_journal(&mut self) {
        let Some(journal) = self.journal.clone() else {
            return;
        };
        let Mode::Journal {
            shown,
            days,
            at,
            rows,
            cursor,
            ..
        } = &mut self.mode
        else {
            return;
        };
        *rows = match days.get(*at) {
            Some(day) => journal::arrange(journal.read_shown(*shown, *day)),
            None => Vec::new(),
        };
        *cursor = 0;
    }

    fn on_key_journal(&mut self, key: KeyEvent) {
        let mut reload = false;
        {
            let Some(journal) = self.journal.clone() else {
                self.mode = Mode::Normal;
                return;
            };
            let Mode::Journal {
                shown,
                days,
                at,
                rows,
                filter,
                cursor,
                searching,
            } = &mut self.mode
            else {
                return;
            };
            let showing = filter.apply(rows.clone());
            let last = journal::lines(&showing).len().saturating_sub(1);

            // With the search box open every printable character belongs to
            // it, or `k` and `q` would be unusable in a search term.
            if *searching {
                match key.code {
                    KeyCode::Esc => {
                        *searching = false;
                        filter.text.clear();
                        *cursor = 0;
                    }
                    KeyCode::Enter => *searching = false,
                    KeyCode::Backspace => {
                        filter.text.pop();
                        *cursor = 0;
                    }
                    KeyCode::Char(c) => {
                        filter.text.push(c);
                        *cursor = 0;
                    }
                    KeyCode::Down => *cursor = (*cursor + 1).min(last),
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    _ => {}
                }
                return;
            }

            match key.code {
                KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => {
                    self.mode = Mode::Normal;
                    return;
                }
                // Take a command back out of the account and put it on the
                // command line. Not run: a line remembered from a week ago,
                // in a directory that is not this one, is exactly where an
                // `rm` goes wrong. It is offered for reading and editing,
                // and Enter is still what runs it.
                // Alt-Enter takes the *place* rather than the line: where it
                // happened, as new tabs. For a copy that is both ends, which
                // is the question anybody asks afterwards.
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    let lines = journal::lines(&showing);
                    let scene = lines
                        .get(*cursor)
                        .and_then(|line| journal::event_at(&showing, line))
                        .and_then(journal::scene_of);
                    let Some(scene) = scene else {
                        self.error("That record does not say where it happened");
                        return;
                    };
                    self.mode = Mode::Normal;
                    self.open_scene(&scene);
                    return;
                }
                KeyCode::Enter => {
                    let lines = journal::lines(&showing);
                    let found = lines
                        .get(*cursor)
                        .and_then(|line| journal::event_at(&showing, line))
                        .and_then(command_to_reuse);
                    let Some((line, ran_in)) = found else {
                        self.error("That is not a command - nothing to run again");
                        return;
                    };
                    self.mode = Mode::Normal;
                    let here = self.active_panel().cwd.clone();
                    self.command = line;
                    // Where it ran matters: the same line means something
                    // else in another directory, and the reader is about to
                    // press Enter.
                    if Path::new(&ran_in) == here {
                        self.info("Ready to run again here. Enter runs it.");
                    } else {
                        self.info(format!("Ran in {ran_in} - you are in {}", here.display()));
                    }
                    return;
                }
                KeyCode::Down => *cursor = (*cursor + 1).min(last),
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::PageDown => *cursor = (*cursor + 15).min(last),
                KeyCode::PageUp => *cursor = cursor.saturating_sub(15),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = last,
                // The days, in the direction the dates go.
                KeyCode::Left => {
                    if *at + 1 < days.len() {
                        *at += 1;
                        reload = true;
                    }
                }
                KeyCode::Right => {
                    if *at > 0 {
                        *at -= 1;
                        reload = true;
                    }
                }
                // All, then files alone, then commands alone. They are kept
                // apart on disk because a build's twenty commands a minute
                // would bury the file work; reading is free to put them back
                // together, and often should.
                KeyCode::Tab => {
                    let was = days.get(*at).copied();
                    *shown = shown.next();
                    *days = journal.days_shown(*shown);
                    *at = was
                        .and_then(|day| days.iter().position(|&d| d == day))
                        .unwrap_or(0);
                    filter.kinds.clear();
                    reload = true;
                }
                // Only what did not work, which is the one filter worth a key
                // of its own.
                KeyCode::Char('!') => {
                    filter.failures_only = !filter.failures_only;
                    *cursor = 0;
                }
                // Walk the kinds: no filter, then one at a time.
                KeyCode::Char('k') => {
                    let kinds: Vec<journal::Kind> = journal::KINDS
                        .into_iter()
                        .filter(|kind| shown.holds(*kind))
                        .collect();
                    let next = match filter.kinds.first() {
                        None => kinds.first().copied(),
                        Some(current) => {
                            let at = kinds.iter().position(|k| k == current).unwrap_or(0);
                            kinds.get(at + 1).copied()
                        }
                    };
                    filter.kinds = next.into_iter().collect();
                    *cursor = 0;
                }
                // Search anything on a line, not the path alone: what was
                // run, which shell ran it, what a file was opened with.
                KeyCode::Char('/') => {
                    *searching = true;
                    *cursor = 0;
                }
                _ => {}
            }
        }
        if reload {
            self.reload_journal();
        }
    }

    /// `F2` in the byte editor: put the changed bytes back where they came
    /// from, in place.
    fn write_bytes(&mut self) {
        let Mode::Bytes { dump, edits, .. } = &self.mode else {
            return;
        };
        if edits.is_empty() {
            self.info("Nothing changed");
            return;
        }
        let (path, count) = (dump.path.clone(), edits.len());
        match hex::write_back(&path, edits) {
            Ok(_) => {
                self.note(
                    journal::Event::new(journal::Kind::Edit, &path)
                        .note(format!("bytes, {count} changed")),
                );
                if let Mode::Bytes { edits, .. } = &mut self.mode {
                    edits.clear();
                }
                self.info(format!("Wrote {count} byte(s)"));
                self.reload_both();
            }
            Err(e) => self.error(format!("Write failed: {e}")),
        }
    }

    fn on_key_viewer(&mut self, key: KeyEvent) {
        let Mode::Viewer { lines, scroll, .. } = &mut self.mode else {
            return;
        };
        let last = lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::F(3) | KeyCode::F(10) | KeyCode::Char('q') => {
                self.mode = Mode::Normal
            }
            KeyCode::Down => *scroll = (*scroll + 1).min(last),
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => *scroll = (*scroll + 20).min(last),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
            KeyCode::Home => *scroll = 0,
            KeyCode::End => *scroll = last,
            // Read it as something else. Not a chooser: seven encodings, and
            // the right one announces itself the moment the text stops being
            // nonsense, so a key you can press until it looks right is faster
            // than a form.
            KeyCode::Char('e') => self.view_next_encoding(false),
            KeyCode::Char('E') => self.view_next_encoding(true),
            _ => {}
        }
    }
}

/// The byte the file has at one offset, read on its own.
///
/// One read for one byte is wasteful in the abstract and free in practice:
/// this is called once per keystroke, and the alternative is holding the file.
fn byte_at(dump: &hex::Dump, at: u64) -> Option<u8> {
    let row = dump.read(at / hex::PER_ROW as u64, 1).ok()?;
    row.first()?
        .bytes
        .get((at % hex::PER_ROW as u64) as usize)
        .copied()
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn describe(targets: &[PathBuf]) -> String {
    match targets.len() {
        0 => "nothing".to_string(),
        1 => format!("\"{}\"", display_name(&targets[0])),
        n => format!("{n} items"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lost_commander_core::netloc::Protocol;
    use std::fs;
    use std::sync::{Arc, Mutex};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// Two directories with a little content in each.
    ///
    /// `App::detached` is used so tests never read or write the real
    /// `~/.config/lost-commander/bookmarks.toml`.
    fn app_fixture() -> (tempfile::TempDir, App) {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(left.join("one.txt"), "first").unwrap();
        fs::write(left.join("two.txt"), "second").unwrap();
        fs::create_dir(left.join("sub")).unwrap();
        let mut app = App::detached(left, right);
        app.bookmarks_path = Some(root.path().join("bookmarks.toml"));
        (root, app)
    }

    fn select(app: &mut App, name: &str) {
        let index = app
            .active_panel()
            .entries
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        app.active_panel_mut().cursor_to(index);
    }

    #[test]
    fn tab_switches_the_active_panel() {
        let (_root, mut app) = app_fixture();
        assert_eq!(app.active, Side::Left);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active, Side::Right);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active, Side::Left);
    }

    #[test]
    fn f10_quits_and_q_is_a_character() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(10)));
        assert!(app.should_quit);

        // `q` used to quit. It cannot any more, and this is not a
        // regression: with a command line under the panels a letter has to be
        // a letter, or `qemu` is unwritable and the reader loses their work
        // to a typo. F10 is how both Norton and Midnight Commander quit, and
        // is unchanged.
        let (_root2, mut app2) = app_fixture();
        app2.on_key(key(KeyCode::Char('q')));
        assert!(!app2.should_quit);
        assert_eq!(app2.command, "q");
    }

    #[test]
    fn an_empty_command_line_means_you_are_working_the_panels() {
        let (_root, mut app) = app_fixture();
        // Off `..`, which is never marked - it is not a file.
        app.active_panel_mut().cursor_to(1);
        let marked = app.active_panel().marked_count();

        // Space marks a file while nothing is typed...
        app.on_key(key(KeyCode::Char(' ')));
        assert_ne!(app.active_panel().marked_count(), marked);
        assert!(app.command.is_empty());

        // ...and is a space once there is, or a command could never hold one.
        app.on_key(key(KeyCode::Char('l')));
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.command, "l x");
    }

    #[test]
    fn backspace_goes_up_a_directory_until_there_is_something_to_delete() {
        let (_root, mut app) = app_fixture();
        let here = app.active_panel().cwd.clone();

        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Backspace));
        assert!(app.command.is_empty(), "it deleted the character");
        assert_eq!(app.active_panel().cwd, here, "and did not move the panel");

        app.on_key(key(KeyCode::Backspace));
        assert_ne!(app.active_panel().cwd, here, "now it climbs");
    }

    #[test]
    fn a_typed_command_is_handed_over_and_the_line_is_cleared() {
        let (_root, mut app) = app_fixture();
        for character in "echo hello".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.command.is_empty(), "the line was taken");
        // Into the shell that keeps running, so that a `cd` in one command is
        // still true for the next. Either it started and is on show, or it
        // could not start and said why - never silently nothing.
        if app.shell.is_some() {
            assert!(app.showing_shell, "you are shown the shell doing it");
        } else {
            assert!(app.status_is_error, "a shell that will not start says so");
        }
    }

    #[test]
    fn the_shell_outlives_the_command() {
        // The whole point of a subshell: one shell for the session, so `cd`
        // in one command is still true in the next. A fresh shell per command
        // starts where it is put and forgets everything the last one did.
        let (_root, mut app) = app_fixture();
        app.send_to_shell("echo one");
        if app.shell.is_none() {
            return; // No shell on this machine; nothing to prove.
        }
        let first = app.shell.as_ref().map(|s| s as *const _);
        app.send_to_shell("echo two");
        assert_eq!(
            app.shell.as_ref().map(|s| s as *const _),
            first,
            "the same session, not a second one"
        );
    }

    #[test]
    fn enter_descends_into_a_directory() {
        let (_root, mut app) = app_fixture();
        let start = app.active_panel().cwd.clone();
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.active_panel().cwd, start.join("sub"));
    }

    /// Swap the real opener for one that only records, and hand back the log.
    fn watch_opener(app: &mut App) -> Arc<Mutex<Vec<PathBuf>>> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&log);
        app.opener = Box::new(move |path: &Path| {
            recorder.lock().unwrap().push(path.to_path_buf());
            Ok(())
        });
        log
    }

    #[test]
    fn enter_on_a_file_hands_it_to_the_desktop() {
        let (_root, mut app) = app_fixture();
        let expected = app.active_panel().cwd.join("one.txt");
        let opened = watch_opener(&mut app);
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Enter));

        assert_eq!(*opened.lock().unwrap(), vec![expected]);
        // No dialog: a text file is not something to ask about.
        assert!(app.mode.is_normal());
    }

    /// Swap the real launcher for one that records the command it was given.
    ///
    /// Only the chooser tests want this, and Windows has a chooser of its own
    /// so those do not run there.
    #[cfg(not(windows))]
    fn watch_launcher(app: &mut App) -> Arc<Mutex<Vec<lost_commander_core::open::Launch>>> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&log);
        app.launcher = Box::new(move |command: &lost_commander_core::open::Launch| {
            recorder.lock().unwrap().push(command.clone());
            Ok(())
        });
        log
    }

    // Windows shows its own chooser instead of ours - see the test in
    // `apps` for that half.
    #[cfg(not(windows))]
    #[test]
    fn ctrl_p_chooses_which_application_opens_the_file() {
        let (_root, mut app) = app_fixture();
        let started = watch_launcher(&mut app);
        select(&mut app, "one.txt");

        app.on_key(ctrl('p'));
        let Mode::OpenWith { applications, .. } = &mut app.mode else {
            panic!("expected the chooser, got {:?}", app.mode);
        };
        // The list comes from the machine, so stand one in that does not.
        *applications = vec![apps::Application {
            name: "Text Editor".into(),
            exec: "gedit %U".into(),
            handles: true,
            terminal: false,
        }];

        app.on_key(key(KeyCode::Enter));
        let started = started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].program, "gedit");
        assert!(app.mode.is_normal());
    }

    // Windows shows its own chooser instead of ours - see the test in
    // `apps` for that half.
    #[cfg(not(windows))]
    #[test]
    fn what_matches_nothing_in_the_chooser_is_run_as_a_command() {
        let (_root, mut app) = app_fixture();
        let file = app.active_panel().cwd.join("one.txt");
        let started = watch_launcher(&mut app);
        select(&mut app, "one.txt");
        app.on_key(ctrl('p'));

        for c in "hexdump".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        let started = started.lock().unwrap();
        assert_eq!(started[0].program, "hexdump");
        assert_eq!(started[0].args, vec![file.display().to_string()]);
    }

    // Windows shows its own chooser instead of ours - see the test in
    // `apps` for that half.
    #[cfg(not(windows))]
    #[cfg(not(windows))]
    #[test]
    fn ctrl_a_in_the_chooser_asks_the_system_to_authorise() {
        let (_root, mut app) = app_fixture();
        let started = watch_launcher(&mut app);
        select(&mut app, "one.txt");
        app.on_key(ctrl('p'));

        let Mode::OpenWith { applications, .. } = &mut app.mode else {
            panic!("expected the chooser")
        };
        *applications = vec![apps::Application {
            name: "Text Editor".into(),
            exec: "gedit %U".into(),
            handles: true,
            terminal: false,
        }];
        app.on_key(ctrl('a'));
        assert!(
            matches!(app.mode, Mode::OpenWith { as_admin: true, .. }),
            "the toggle did not stick"
        );
        app.on_key(key(KeyCode::Enter));

        // Either a graphical prompt was spawned or a sudo line is waiting for
        // the terminal - never a bare `gedit`, which is the whole point.
        let started = started.lock().unwrap();
        let asked_graphically = started
            .iter()
            .any(|c| ["pkexec", "kdesu", "lxqt-sudo", "osascript"].contains(&c.program.as_str()));
        let asked_in_a_shell = app
            .pending_shell
            .as_deref()
            .map(|line| line.starts_with("sudo "))
            .unwrap_or(false);
        assert!(asked_graphically || asked_in_a_shell, "{started:?}");
        assert!(
            !started.iter().any(|c| c.program == "gedit"),
            "started unprivileged: {started:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shift_f4_edits_without_running_the_editor_as_root() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::SHIFT));

        let line = app.pending_shell.expect("nothing queued");
        assert!(line.contains("sudoedit"), "{line}");
        assert!(line.contains("one.txt"), "{line}");
        // The editor is not what gets the privilege.
        assert!(!line.contains("sudo vi "), "{line}");
        // ...and plain F4 is untouched.
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(4)));
        assert!(app.pending_edit.is_some());
        assert!(app.pending_shell.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ctrl_e_queues_a_root_shell_where_the_panel_is() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        app.on_key(ctrl('e'));

        let line = app.pending_shell.expect("nothing queued");
        assert!(line.contains(&cwd.display().to_string()), "{line}");
        assert!(line.contains("sudo -i"), "{line}");
    }

    // Windows shows its own chooser instead of ours - see the test in
    // `apps` for that half.
    #[cfg(not(windows))]
    #[test]
    fn escape_leaves_the_chooser_without_starting_anything() {
        let (_root, mut app) = app_fixture();
        let started = watch_launcher(&mut app);
        select(&mut app, "one.txt");

        app.on_key(ctrl('p'));
        assert!(matches!(app.mode, Mode::OpenWith { .. }));
        app.on_key(key(KeyCode::Esc));

        assert!(app.mode.is_normal());
        assert!(started.lock().unwrap().is_empty());
    }

    #[test]
    fn the_chooser_has_nothing_to_offer_for_a_directory() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "sub");
        app.on_key(ctrl('p'));
        // "With what?" is a question about a file. A directory is somewhere
        // to go, and Enter already does that.
        assert!(app.mode.is_normal());
        assert!(app.status_is_error);
    }

    #[test]
    fn a_copy_onto_an_existing_file_puts_the_question_on_screen() {
        let (_root, mut app) = app_fixture();
        let destination = app.right.cwd().to_path_buf();
        fs::write(destination.join("one.txt"), "PRECIOUS").unwrap();
        app.right.reload();

        app.start_job(Operation::Copy {
            sources: vec![app.left.cwd().join("one.txt")],
            destination: destination.clone(),
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !matches!(app.mode, Mode::Overwrite { .. }) && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Mode::Overwrite { conflict } = &app.mode else {
            panic!("the copy never asked");
        };
        assert_eq!(conflict.target, destination.join("one.txt"));
        assert_eq!(
            fs::read_to_string(destination.join("one.txt")).unwrap(),
            "PRECIOUS"
        );

        // `s` keeps what is there; the worker is released either way.
        app.on_key(key(KeyCode::Char('s')));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.job.is_some() && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(destination.join("one.txt")).unwrap(),
            "PRECIOUS"
        );
    }

    #[test]
    fn only_newer_answers_the_rest_of_the_copy_without_asking_again() {
        let (root, mut app) = app_fixture();
        let source = app.left.cwd().to_path_buf();
        let destination = app.right.cwd().to_path_buf();

        // one.txt is newer in the source; two.txt is newer at the
        // destination. Stamped rather than left to chance, since the whole
        // rule turns on the dates.
        for (name, body) in [("one.txt", "FRESH"), ("two.txt", "STALE")] {
            fs::write(destination.join(name), "AT THE DESTINATION").unwrap();
            fs::write(source.join(name), body).unwrap();
        }
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let new = old + std::time::Duration::from_secs(60 * 60);
        let stamp = |path: std::path::PathBuf, when: std::time::SystemTime| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(when))
                .unwrap();
        };
        stamp(source.join("one.txt"), new);
        stamp(destination.join("one.txt"), old);
        stamp(source.join("two.txt"), old);
        stamp(destination.join("two.txt"), new);

        app.start_job(Operation::Copy {
            sources: vec![source.join("one.txt"), source.join("two.txt")],
            destination: destination.clone(),
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !matches!(app.mode, Mode::Overwrite { .. }) && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(app.mode, Mode::Overwrite { .. }), "never asked");

        // `n` is a rule, not an answer: the rest of the run follows it.
        app.on_key(key(KeyCode::Char('n')));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.job.is_some() && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.job.is_none(), "the copy never finished");
        assert!(
            !matches!(app.mode, Mode::Overwrite { .. }),
            "it asked a second time"
        );

        assert_eq!(
            fs::read_to_string(destination.join("one.txt")).unwrap(),
            "FRESH",
            "the newer file arriving wins"
        );
        assert_eq!(
            fs::read_to_string(destination.join("two.txt")).unwrap(),
            "AT THE DESTINATION",
            "and the newer file already there is left alone"
        );
        let _ = root;
    }

    #[test]
    fn ctrl_f_finds_a_file_and_enter_goes_to_it() {
        let (_root, mut app) = app_fixture();
        let deep = app.left.cwd().join("sub/deeper");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("buried.txt"), "found me").unwrap();
        app.left.reload();

        app.on_key(ctrl('f'));
        assert!(matches!(app.mode, Mode::Find { .. }));

        for c in "buried*".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        // The walk is on a thread, so this is a wait rather than a race.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app
            .search
            .as_ref()
            .map(|s| !s.is_finished())
            .unwrap_or(false)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let found = app.search.as_ref().unwrap().snapshot();
        assert_eq!(found.hits.len(), 1, "{:?}", found.hits);
        assert_eq!(found.hits[0].path, deep.join("buried.txt"));

        // Tab into the results, Enter to go there.
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Tab));
        assert!(matches!(
            app.mode,
            Mode::Find {
                field: FindField::Results,
                ..
            }
        ));
        app.on_key(key(KeyCode::Enter));

        // The panel is in the file's directory with the cursor on it - not
        // the file opened, since finding is about where a thing is.
        assert!(app.mode.is_normal());
        assert_eq!(app.active_panel().cwd, deep);
        assert_eq!(
            app.active_panel().selected().map(|e| e.name.as_str()),
            Some("buried.txt")
        );
        // The thread does not outlive the form.
        assert!(app.search.is_none());
    }

    /// Shift-F2, which is what opens the multi-rename form.
    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    /// Type into whichever box of a form has the keyboard.
    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    /// A key with Ctrl behind it, for the ones that are not characters.
    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// Wait for a duplicate hunt to finish and take its results.
    fn settle_hunt(app: &mut App) {
        for _ in 0..400 {
            app.collect_hunt();
            if app.hunt.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the hunt never finished");
    }

    /// Wait for a file operation to finish.
    ///
    /// It says so when it gives up, which is the point of having it: the two
    /// tests that used to spin here inline just carried on afterwards, so a
    /// job that had not finished yet arrived as "the file should be gone" -
    /// an answer about the wrong thing, and one that sent the reader looking
    /// at `delete` when the fault was the clock.
    ///
    /// Thirty seconds, which is far longer than any of this should take,
    /// because a deadline only costs time when something is already wrong: a
    /// job that finishes returns immediately. The previous two seconds were
    /// not enough on Windows, where a delete to the recycle bin starts a
    /// PowerShell per file and takes about a second and a half each.
    fn settle_job(app: &mut App) {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.poll_job();
            if app.job.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the job never finished in {:?}", started.elapsed());
    }

    #[test]
    fn alt_u_finds_the_copies_and_deletes_only_what_is_ticked() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        // one.txt already says "first"; two copies of it, one nested.
        fs::write(left.join("copy-a.txt"), "first").unwrap();
        fs::create_dir_all(left.join("sub/deeper")).unwrap();
        fs::write(left.join("sub/deeper/copy-b.txt"), "first").unwrap();
        app.reload_both();

        app.on_key(alt('u'));
        assert!(matches!(app.mode, Mode::Duplicates { .. }), "no window");
        settle_hunt(&mut app);

        let Mode::Duplicates { groups, .. } = &app.mode else {
            panic!("the window closed");
        };
        assert_eq!(groups.len(), 1, "one set: three copies of \"first\"");
        assert_eq!(groups[0].copies.len(), 3);
        assert_eq!(dupes::reclaimed(groups), 0, "nothing is ticked to start");

        // Space on the heading thins the set: keep the first, let the rest go.
        app.on_key(key(KeyCode::Char(' ')));
        let Mode::Duplicates { groups, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(groups[0].keeping(), 1);
        let going = dupes::to_remove(groups);
        assert_eq!(going.len(), 2);
        let kept = groups[0].copies[0].path.clone();

        // F8 asks first, and the answer runs the ordinary delete.
        app.on_key(key(KeyCode::F(8)));
        let Mode::Confirm(dialog) = &app.mode else {
            panic!("it deleted without asking: {:?}", app.status)
        };
        assert!(dialog.message.contains('2'), "{}", dialog.message);
        app.on_key(key(KeyCode::Char('y')));
        settle_job(&mut app);

        assert!(kept.exists(), "one copy of each is always kept");
        for path in going {
            assert!(!path.exists(), "{} should be gone", path.display());
        }
    }

    #[test]
    fn the_last_copy_of_a_set_cannot_be_ticked() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        fs::write(left.join("copy-a.txt"), "first").unwrap();
        app.reload_both();
        app.on_key(alt('u'));
        settle_hunt(&mut app);

        // Down onto the first copy, tick it, then onto the second and try.
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char(' ')));
        let Mode::Duplicates { groups, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(
            groups[0].keeping(),
            1,
            "a set that would lose every copy refuses the last tick"
        );
    }

    #[test]
    fn escape_stops_the_hunt_as_well_as_closing_the_window() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('u'));
        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());
        assert!(app.hunt.is_none(), "a thread outlived the window");
    }

    #[test]
    fn f3_on_a_binary_shows_bytes_rather_than_replacement_characters() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        // An ELF header: not text, and lossy UTF-8 would make a mess of it.
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        bytes.extend((0..200u8).map(|n| n.wrapping_mul(7)));
        fs::write(left.join("prog.bin"), &bytes).unwrap();
        app.reload_both();
        select(&mut app, "prog.bin");

        app.on_key(key(KeyCode::F(3)));
        let Mode::Bytes {
            name, dump, scroll, ..
        } = &app.mode
        else {
            panic!("not shown as bytes: {:?}", app.status);
        };
        assert_eq!(name, "prog.bin");
        assert_eq!(dump.size, bytes.len() as u64);
        assert_eq!(*scroll, 0);
        // The first row is the header, read back as it is on disk.
        let first = dump.read(0, 1).unwrap();
        assert_eq!(&first[0].bytes[..4], &[0x7f, b'E', b'L', b'F']);
        assert_eq!(first[0].bytes.len(), 16, "sixteen bytes to a row");
        assert_eq!(
            &first[0].text()[..8],
            ".ELF....",
            "the unprintable bytes of the header show as dots"
        );

        // The keys move by rows and stop at the ends.
        app.on_key(key(KeyCode::End));
        let Mode::Bytes { dump, scroll, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(*scroll, dump.rows() - 1);
        app.on_key(key(KeyCode::Down));
        let Mode::Bytes { dump, scroll, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(*scroll, dump.rows() - 1, "the end is the end");
        app.on_key(key(KeyCode::Home));
        app.on_key(key(KeyCode::Up));
        let Mode::Bytes { scroll, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(*scroll, 0);

        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());

        // And a text file is still text.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(3)));
        assert!(matches!(app.mode, Mode::Viewer { .. }));
    }

    #[test]
    fn f4_in_the_dump_types_bytes_and_f2_writes_them_in_place() {
        let (root, mut app) = app_fixture();
        let path = root.path().join("left").join("patch.bin");
        // Not text, so F3 opens the dump.
        let mut bytes = vec![0x00, 0x01, 0x02, 0x03];
        bytes.extend(b"hello world".iter().copied());
        fs::write(&path, &bytes).unwrap();
        app.reload_both();
        select(&mut app, "patch.bin");

        app.on_key(key(KeyCode::F(3)));
        assert!(matches!(app.mode, Mode::Bytes { editing: false, .. }));

        // Reading, the letters are shortcuts and do not type: a dump that can
        // be edited by accident corrupts a file by accident.
        app.on_key(key(KeyCode::Char('a')));
        let Mode::Bytes { edits, .. } = &app.mode else {
            panic!("closed")
        };
        assert!(edits.is_empty(), "reading, letters do not type");

        app.on_key(key(KeyCode::F(4)));
        assert!(matches!(app.mode, Mode::Bytes { editing: true, .. }));

        // Two keystrokes to a byte: `f` then `f` turns 0x00 into 0xff.
        app.on_key(key(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('f')));
        let Mode::Bytes { edits, cursor, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(edits.get(0), Some(0xff));
        assert_eq!(cursor.at, 1, "on to the next byte once both halves are in");

        // The text column takes one keystroke to a byte, for patching a
        // string without doing the ASCII table in your head.
        let size = bytes.len() as u64;
        let Mode::Bytes { cursor, .. } = &mut app.mode else {
            panic!("closed")
        };
        cursor.to(4, size);
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('H')));
        let Mode::Bytes { edits, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(edits.get(4), Some(b'H'));
        assert_eq!(edits.len(), 2);

        // Nothing is written until it is asked for.
        assert_eq!(fs::read(&path).unwrap(), bytes);

        app.on_key(key(KeyCode::F(2)));
        let after = fs::read(&path).unwrap();
        assert_eq!(after[0], 0xff);
        assert_eq!(&after[4..15], b"Hello world");
        assert_eq!(after.len(), bytes.len(), "overwritten, not inserted into");

        let Mode::Bytes { edits, .. } = &app.mode else {
            panic!("closed")
        };
        assert!(edits.is_empty(), "written is no longer pending");
    }

    #[test]
    fn backspace_in_the_dump_takes_the_last_change_back() {
        let (root, mut app) = app_fixture();
        let path = root.path().join("left").join("undo.bin");
        fs::write(&path, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        app.reload_both();
        select(&mut app, "undo.bin");

        app.on_key(key(KeyCode::F(3)));
        app.on_key(key(KeyCode::F(4)));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('b')));
        let Mode::Bytes { edits, .. } = &app.mode else {
            panic!("closed")
        };
        assert_eq!(edits.get(0), Some(0xab));

        app.on_key(key(KeyCode::Backspace));
        let Mode::Bytes { edits, cursor, .. } = &app.mode else {
            panic!("closed")
        };
        assert!(edits.is_empty());
        assert_eq!(cursor.at, 0, "and the cursor goes back to look at it");

        // F2 with nothing pending writes nothing rather than rewriting the
        // file with what it already had.
        app.on_key(key(KeyCode::F(2)));
        assert_eq!(fs::read(&path).unwrap(), vec![0u8, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn comparing_two_binaries_names_where_they_first_differ() {
        let (root, mut app) = app_fixture();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        let mut a: Vec<u8> = (0..500u32).map(|n| (n % 251) as u8).collect();
        a[0] = 0;
        let mut b = a.clone();
        b[300] ^= 0xff;
        fs::write(left.join("blob.bin"), &a).unwrap();
        fs::write(right.join("blob.bin"), &b).unwrap();
        app.reload_both();
        select(&mut app, "blob.bin");
        app.on_key(key(KeyCode::Tab));
        select(&mut app, "blob.bin");
        app.on_key(key(KeyCode::Tab));

        app.on_key(alt('d'));
        assert!(app.mode.is_normal(), "a hex diff is not this window");
        assert!(app.status.contains("300"), "{}", app.status);
        assert!(app.status.contains("0x12c"), "{}", app.status);
    }

    #[test]
    fn alt_d_compares_two_files_from_either_gesture() {
        let (root, mut app) = app_fixture();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        fs::write(left.join("one.txt"), "alpha\nbeta\ngamma\n").unwrap();
        fs::write(left.join("two.txt"), "alpha\nBETA\ngamma\n").unwrap();
        fs::write(right.join("one.txt"), "alpha\nbeta\ndelta\n").unwrap();
        app.reload_both();

        // One from each pane, under the cursors.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));
        app.on_key(alt('d'));
        let Mode::Difference {
            diff,
            left: l,
            right: r,
            ..
        } = &app.mode
        else {
            panic!("no difference shown: {}", app.status);
        };
        assert_eq!(l, &left.join("one.txt"));
        assert_eq!(r, &right.join("one.txt"));
        assert_eq!(diff.changes, 2, "gamma out, delta in");
        app.on_key(key(KeyCode::Esc));

        // Two marked in one pane.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Insert));
        select(&mut app, "two.txt");
        app.on_key(key(KeyCode::Insert));
        app.on_key(alt('d'));
        let Mode::Difference {
            diff,
            left: l,
            right: r,
            ..
        } = &app.mode
        else {
            panic!("no difference shown: {}", app.status);
        };
        assert_eq!(l, &left.join("one.txt"));
        assert_eq!(r, &left.join("two.txt"), "both from the one pane");
        assert_eq!(diff.changes, 2, "beta out, BETA in");

        // And a plain F3 is still the viewer.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(3)));
        assert!(matches!(app.mode, Mode::Viewer { .. }));
    }

    #[test]
    fn two_identical_files_are_said_rather_than_shown() {
        let (root, mut app) = app_fixture();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        fs::write(left.join("one.txt"), "the same\n").unwrap();
        fs::write(right.join("one.txt"), "the same\n").unwrap();
        app.reload_both();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));

        app.on_key(alt('d'));
        assert!(
            app.mode.is_normal(),
            "a window of no differences is a puzzle"
        );
        assert!(app.status.contains("identical"), "{}", app.status);
        assert!(!app.status_is_error);
    }

    #[test]
    fn walking_the_differences_moves_the_view() {
        let (root, mut app) = app_fixture();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        let mut a: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let mut b = a.clone();
        b[10] = "changed near the top".into();
        b[30] = "changed near the bottom".into();
        a.push(String::new());
        b.push(String::new());
        fs::write(left.join("one.txt"), a.join("\n")).unwrap();
        fs::write(right.join("one.txt"), b.join("\n")).unwrap();
        app.reload_both();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Tab));
        app.on_key(alt('d'));

        let scroll_now = |app: &App| match &app.mode {
            Mode::Difference { scroll, .. } => *scroll,
            _ => panic!("the window closed"),
        };
        assert_eq!(scroll_now(&app), 0);
        app.on_key(key(KeyCode::Tab));
        let first = scroll_now(&app);
        assert!(first > 0, "moved to the first difference");
        app.on_key(key(KeyCode::Tab));
        let second = scroll_now(&app);
        assert!(second > first, "and on to the next");

        // Round the end and back to the first.
        for _ in 0..3 {
            app.on_key(key(KeyCode::Char('n')));
        }
        assert_eq!(scroll_now(&app), first, "the walk comes back round");
    }

    #[test]
    fn a_pair_that_cannot_be_worked_out_says_so() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        fs::write(left.join("three.txt"), "x").unwrap();
        app.reload_both();

        // Three marked is not a pair.
        for name in ["one.txt", "two.txt", "three.txt"] {
            select(&mut app, name);
            app.on_key(key(KeyCode::Insert));
        }
        app.on_key(alt('d'));
        assert!(app.mode.is_normal());
        assert!(app.status.contains("exactly two"), "{}", app.status);

        // A directory under the cursor is not a file.
        app.active_panel_mut().clear_marks();
        select(&mut app, "sub");
        app.on_key(alt('d'));
        assert!(app.status_is_error, "{}", app.status);
    }

    #[test]
    fn alt_c_marks_what_differs_between_the_two_panes() {
        let (root, mut app) = app_fixture();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        // one.txt is on both sides and the same; two.txt only on the left;
        // three.txt only on the right.
        fs::write(right.join("one.txt"), "first").unwrap();
        fs::write(right.join("three.txt"), "third").unwrap();
        app.reload_both();

        app.on_key(alt('c'));

        let marked = |panel: &lost_commander_core::panel::Panel| -> Vec<String> {
            let mut names: Vec<String> = panel
                .entries
                .iter()
                .filter(|e| e.marked)
                .map(|e| e.name.clone())
                .collect();
            names.sort();
            names
        };
        assert_eq!(marked(app.left.current()), ["two.txt"]);
        assert_eq!(marked(app.right.current()), ["three.txt"]);
        assert!(
            !app.left
                .current()
                .entries
                .iter()
                .any(|e| e.marked && e.name == "sub"),
            "a directory is not marked: whether it differs is about its contents"
        );
        assert!(!app.status_is_error, "{}", app.status);
        let _ = left;
    }

    #[test]
    fn alt_s_opens_the_synchronize_form_and_escape_stops_the_scan() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('s'));
        assert!(
            matches!(app.mode, Mode::Sync { .. }),
            "the form did not open"
        );

        // Wait for the comparison, then take its results into the form.
        for _ in 0..200 {
            app.collect_scan();
            if app.scan.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Mode::Sync { pairs, .. } = &app.mode else {
            panic!("the form closed");
        };
        let names: Vec<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"one.txt") && names.contains(&"two.txt"),
            "the left-only files are differences: {names:?}"
        );
        assert!(pairs
            .iter()
            .all(|p| p.direction == compare::Direction::ToRight));

        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());
        assert!(app.scan.is_none(), "a thread outlived the form");
    }

    #[test]
    fn space_turns_a_row_round_and_f5_carries_the_plan_out() {
        let (root, mut app) = app_fixture();
        let right = root.path().join("right");
        app.on_key(alt('s'));
        for _ in 0..200 {
            app.collect_scan();
            if app.scan.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Turn the first row round. Every pair here is a file only the left
        // has, so the two directions it can take are "to the right" and
        // "leave it alone" - pointing it left would ask for a right-hand file
        // that is not there, and the run would fail on it.
        app.on_key(key(KeyCode::Char(' ')));
        let Mode::Sync { pairs, .. } = &app.mode else {
            panic!("the form closed");
        };
        assert_eq!(pairs[0].direction, compare::Direction::Skip);
        let turned = pairs[0].name.clone();

        app.on_key(key(KeyCode::Char(' ')));
        let Mode::Sync { pairs, .. } = &app.mode else {
            panic!("the form closed");
        };
        assert_eq!(
            pairs[0].direction,
            compare::Direction::ToRight,
            "round again, never to the left"
        );
        app.on_key(key(KeyCode::Char(' '))); // and back to leaving it alone

        app.on_key(key(KeyCode::F(5)));
        assert!(matches!(app.mode, Mode::Progress), "the copy is running");
        settle_job(&mut app);
        app.reload_both();

        // Everything but the row that was told to stay put.
        for name in ["one.txt", "two.txt"] {
            assert_eq!(
                right.join(name).exists(),
                name != turned,
                "{name}: the row left alone is the one that did not move"
            );
        }
    }

    #[test]
    fn one_key_points_the_whole_comparison_one_way() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('s'));
        for _ in 0..200 {
            app.collect_scan();
            if app.scan.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let count = |app: &App| match &app.mode {
            Mode::Sync { pairs, .. } => compare::tally(pairs),
            _ => panic!("the form closed"),
        };
        let all = count(&app).to_right;
        assert!(all > 1, "more than one difference to work with");

        // Nothing at all, then back to what the comparison worked out. The
        // fixture is all left-only files, so "all to the left" cannot move
        // any of them and leaves the list exactly as it was.
        app.on_key(key(KeyCode::Char('-')));
        assert_eq!(count(&app).skipped_differences, all);
        app.on_key(key(KeyCode::Left));
        assert_eq!(count(&app).to_left, 0, "nothing to copy from the right");
        app.on_key(key(KeyCode::Right));
        assert_eq!(count(&app).to_right, all);
        app.on_key(key(KeyCode::Char('-')));
        app.on_key(key(KeyCode::Char('*')));
        assert_eq!(count(&app).to_right, all, "back to the suggestion");
    }

    #[test]
    fn synchronizing_two_directories_that_are_the_same_is_refused() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        app.right.current_mut().chdir(left);
        app.on_key(alt('s'));
        assert!(app.mode.is_normal(), "no form for a comparison with itself");
        assert!(app.status_is_error);
    }

    #[test]
    fn ctrl_t_opens_a_tab_and_ctrl_w_closes_it() {
        let (_root, mut app) = app_fixture();
        let start = app.active_panel().cwd.clone();
        assert_eq!(app.tabs(Side::Left).len(), 1);

        app.on_key(ctrl('t'));
        assert_eq!(app.tabs(Side::Left).len(), 2);
        assert_eq!(
            app.active_panel().cwd,
            start,
            "a new tab opens where the one it came from is"
        );

        // Navigate away, and the tab it came from is still where it was.
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.active_panel().cwd, start.join("sub"));
        assert_eq!(app.tabs(Side::Left).get(0).unwrap().cwd, start);

        app.on_key(ctrl('w'));
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(app.active_panel().cwd, start);

        // And the last one does not close, because a pane always shows
        // something.
        app.on_key(ctrl('w'));
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert!(app.status_is_error);
    }

    #[test]
    fn ctrl_pageup_and_pagedown_walk_the_tabs() {
        let (_root, mut app) = app_fixture();
        app.on_key(ctrl('t'));
        app.on_key(ctrl('t'));
        assert_eq!(app.tabs(Side::Left).len(), 3);
        assert_eq!(app.tabs(Side::Left).active(), 2);

        app.on_key(ctrl_key(KeyCode::PageDown));
        assert_eq!(app.tabs(Side::Left).active(), 0, "round to the first");
        app.on_key(ctrl_key(KeyCode::PageUp));
        assert_eq!(app.tabs(Side::Left).active(), 2, "and round to the last");

        // Without the modifier they are still the cursor, as they always were.
        app.active_panel_mut().cursor_home();
        app.on_key(key(KeyCode::PageDown));
        assert_ne!(app.active_panel().cursor, 0);
        assert_eq!(app.tabs(Side::Left).active(), 2, "and no tab moved");
    }

    #[test]
    fn alt_w_keeps_the_tab_on_show_and_closes_the_rest() {
        let (_root, mut app) = app_fixture();
        app.on_key(ctrl('t'));
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        let kept = app.active_panel().cwd.clone();
        app.on_key(ctrl('t'));
        app.on_key(ctrl_key(KeyCode::PageUp));
        assert_eq!(app.tabs(Side::Left).len(), 3);
        assert_eq!(app.active_panel().cwd, kept);

        app.on_key(alt('w'));

        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(
            app.active_panel().cwd,
            kept,
            "the one that was on show is the one left"
        );
    }

    #[test]
    fn shift_f6_sends_the_whole_tab_to_the_other_pane() {
        let (_root, mut app) = app_fixture();
        app.on_key(ctrl('t'));
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Insert));
        let moved = app.active_panel().cwd.clone();
        assert_eq!(app.active_panel().marked_count(), 1);

        app.on_key(shift(KeyCode::F(6)));

        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(app.tabs(Side::Right).len(), 2);
        assert_eq!(
            app.active,
            Side::Right,
            "the tab is what you were working in, so you go across with it"
        );
        assert_eq!(app.active_panel().cwd, moved);
        assert_eq!(
            app.active_panel().marked_count(),
            1,
            "the tab arrived whole, marks and all"
        );

        // The only tab of a pane stays where it is.
        app.on_key(key(KeyCode::Tab));
        app.on_key(shift(KeyCode::F(6)));
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert!(app.status_is_error);

        // And a plain F6 still asks to move files.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(6)));
        assert!(matches!(app.mode, Mode::Input(_)));
    }

    #[test]
    fn shift_f2_opens_the_rename_form_over_the_marked_files() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Insert));
        select(&mut app, "two.txt");
        app.on_key(key(KeyCode::Insert));

        app.on_key(shift(KeyCode::F(2)));
        let Mode::MultiRename {
            sources, changes, ..
        } = &app.mode
        else {
            panic!("the form did not open");
        };
        assert_eq!(sources.len(), 2, "both marked files, and not the directory");
        assert!(
            changes.iter().all(|c| !c.is_rename()),
            "the form opens with rules that change nothing"
        );

        // Plain F2 is still the one-file rename.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(2)));
        assert!(matches!(app.mode, Mode::Input(_)));
    }

    #[test]
    fn typing_a_template_renames_the_selection() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        app.on_key(ctrl('a'));
        app.on_key(shift(KeyCode::F(2)));

        // The name box has the keyboard when the form opens.
        for _ in 0.."[N]".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_in(&mut app, "photo_[C001]");
        let Mode::MultiRename { changes, .. } = &app.mode else {
            panic!("the form closed");
        };
        let named: Vec<&str> = changes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            named,
            ["photo_001", "photo_002.txt", "photo_003.txt"],
            "in the order the panel lists them, and a directory is renamed \
             like anything else - it just has no extension to keep"
        );

        app.on_key(key(KeyCode::Enter));
        assert!(app.mode.is_normal());
        assert!(left.join("photo_001").is_dir());
        assert!(left.join("photo_002.txt").exists());
        assert!(left.join("photo_003.txt").exists());
        assert!(!left.join("one.txt").exists());
        assert!(
            app.active_panel()
                .entries
                .iter()
                .any(|e| e.name == "photo_002.txt"),
            "the panel shows the new names without being told to reload"
        );
        assert_eq!(
            app.active_panel().marked_count(),
            0,
            "the marks pointed at names that are gone, so none of them are left"
        );
    }

    #[test]
    fn the_form_warns_before_it_writes_over_anything() {
        let (root, mut app) = app_fixture();
        let left = root.path().join("left");
        fs::write(left.join("taken.txt"), "in the way").unwrap();
        app.active_panel_mut().reload();

        select(&mut app, "one.txt");
        app.on_key(shift(KeyCode::F(2)));
        for _ in 0.."[N]".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_in(&mut app, "taken");

        let Mode::MultiRename { changes, .. } = &app.mode else {
            panic!("the form closed");
        };
        assert_eq!(changes[0].trouble, Some(rename::Trouble::Exists));

        // And Enter does nothing at all while that is the whole plan.
        app.on_key(key(KeyCode::Enter));
        assert!(!app.mode.is_normal(), "the form stayed open");
        assert_eq!(
            fs::read_to_string(left.join("taken.txt")).unwrap(),
            "in the way"
        );
    }

    #[test]
    fn tab_walks_the_rename_form_and_the_case_box_cycles() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(shift(KeyCode::F(2)));

        for expected in [
            RenameField::Extension,
            RenameField::Find,
            RenameField::Replace,
            RenameField::Case,
            RenameField::Name,
        ] {
            app.on_key(key(KeyCode::Tab));
            let Mode::MultiRename { field, .. } = &app.mode else {
                panic!("the form closed");
            };
            assert_eq!(*field, expected);
        }

        // On the case box, the arrows choose; typing does not fill it with
        // characters it has no use for.
        app.on_key(key(KeyCode::BackTab));
        app.on_key(key(KeyCode::Right));
        type_in(&mut app, "x");
        let Mode::MultiRename { field, rules, .. } = &app.mode else {
            panic!("the form closed");
        };
        assert_eq!(*field, RenameField::Case);
        assert_eq!(rules.case, rename::Case::Lower);
    }

    #[test]
    fn escape_stops_the_search_as_well_as_closing_the_form() {
        let (_root, mut app) = app_fixture();
        app.on_key(ctrl('f'));
        for c in "*".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Esc));

        assert!(app.mode.is_normal());
        assert!(app.search.is_none(), "a thread outlived the form");
    }

    #[test]
    fn tab_walks_the_find_form_and_skips_the_results_until_there_are_some() {
        assert_eq!(FindField::Named.next(false), FindField::Containing);
        // Nothing found yet, so there is nowhere for the results to be.
        assert_eq!(FindField::Containing.next(false), FindField::Named);
        assert_eq!(FindField::Containing.next(true), FindField::Results);
        assert_eq!(FindField::Results.next(true), FindField::Named);
    }

    #[test]
    fn the_panels_notice_what_something_else_did() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        assert!(!app
            .active_panel()
            .entries
            .iter()
            .any(|e| e.name == "outside.txt"));

        fs::write(cwd.join("outside.txt"), "written by something else").unwrap();
        app.poll_directories();

        assert!(
            app.active_panel()
                .entries
                .iter()
                .any(|e| e.name == "outside.txt"),
            "the panel sat on a stale listing"
        );
    }

    #[test]
    fn nothing_is_re_read_while_an_operation_is_running() {
        // A copy re-reads both panels when it finishes, and a listing
        // changing under it would be the copy's own writes reported as news.
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        let destination = app.right.cwd().to_path_buf();

        app.start_job(Operation::Copy {
            sources: vec![cwd.join("one.txt")],
            destination,
        });
        fs::write(cwd.join("outside.txt"), "x").unwrap();
        app.poll_directories();
        assert!(
            !app.active_panel()
                .entries
                .iter()
                .any(|e| e.name == "outside.txt"),
            "it re-read the panel mid-operation"
        );

        // ...and once it is done, the job's own reload picks it up.
        app.finish_job();
        assert!(app
            .active_panel()
            .entries
            .iter()
            .any(|e| e.name == "outside.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn alt_enter_shows_the_permissions_and_enter_writes_them_back() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = app_fixture();
        let file = app.left.cwd().join("one.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        app.left.reload();
        select(&mut app, "one.txt");

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        let Mode::Properties { now, .. } = &app.mode else {
            panic!("expected the properties dialog, got {:?}", app.mode);
        };
        assert_eq!(now.mode.unwrap().octal(), "644");
        assert_eq!(now.size, 5);

        // The cursor starts on owner/read; two rights is owner/execute.
        assert_eq!(App::permission_at(0), (Who::Owner, What::Read));
        assert_eq!(App::permission_at(2), (Who::Owner, What::Execute));
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Char(' ')));

        let Mode::Properties { now, .. } = &app.mode else {
            panic!("the dialog closed early");
        };
        assert_eq!(now.mode.unwrap().octal(), "744");
        // Nothing is on disk until Enter.
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644
        );

        app.on_key(key(KeyCode::Enter));
        assert!(app.mode.is_normal());
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o744
        );
        assert!(!app.status_is_error, "{}", app.status);
    }

    #[cfg(unix)]
    #[test]
    fn escape_leaves_the_permissions_alone() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = app_fixture();
        let file = app.left.cwd().join("one.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        app.left.reload();
        select(&mut app, "one.txt");

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Esc));

        assert!(app.mode.is_normal());
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600,
            "Escape wrote anyway"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dialog_closed_without_a_change_writes_nothing() {
        // Writing every field back would touch files nobody edited.
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.on_key(key(KeyCode::Enter));

        assert!(app.status.contains("Nothing changed"), "{}", app.status);
        assert!(!app.status_is_error);
    }

    #[test]
    fn the_nine_boxes_are_laid_out_in_reading_order() {
        // Row by row, left to right - which is what the arrow keys assume.
        assert_eq!(App::permission_at(0), (Who::Owner, What::Read));
        assert_eq!(App::permission_at(1), (Who::Owner, What::Write));
        assert_eq!(App::permission_at(2), (Who::Owner, What::Execute));
        assert_eq!(App::permission_at(3), (Who::Group, What::Read));
        assert_eq!(App::permission_at(8), (Who::Other, What::Execute));
    }

    #[test]
    fn f3_still_opens_the_built_in_viewer() {
        let (_root, mut app) = app_fixture();
        let opened = watch_opener(&mut app);
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(3)));
        match &app.mode {
            Mode::Viewer { title, lines, .. } => {
                assert_eq!(title, "one.txt");
                assert_eq!(lines, &vec!["first".to_string()]);
            }
            other => panic!("expected viewer, got {other:?}"),
        }
        assert!(opened.lock().unwrap().is_empty(), "F3 launched something");
        // Esc returns to the panels.
        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());
    }

    #[cfg(unix)]
    #[test]
    fn enter_on_a_program_asks_before_running_it() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = app_fixture();
        let script = app.active_panel().cwd.join("build.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.active_panel_mut().reload();

        let opened = watch_opener(&mut app);
        select(&mut app, "build.sh");
        app.on_key(key(KeyCode::Enter));

        // Nothing has started yet - the question comes first.
        assert!(opened.lock().unwrap().is_empty(), "ran without asking");
        match &app.mode {
            Mode::Confirm(dialog) => assert!(dialog.message.contains("build.sh")),
            other => panic!("expected a confirmation, got {other:?}"),
        }

        // Esc declines, and still nothing runs.
        app.on_key(key(KeyCode::Esc));
        assert!(opened.lock().unwrap().is_empty(), "ran after declining");

        // Asked again and accepted, it goes.
        select(&mut app, "build.sh");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(*opened.lock().unwrap(), vec![script]);
    }

    #[test]
    fn f7_creates_a_directory() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();

        app.on_key(key(KeyCode::F(7)));
        for c in "fresh".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(cwd.join("fresh").is_dir());
        assert!(app.mode.is_normal());
        assert!(!app.status_is_error, "status was: {}", app.status);
        // Cursor lands on the new directory.
        assert_eq!(app.active_panel().selected().unwrap().name, "fresh");
    }

    #[test]
    fn mkdir_reports_failure_without_crashing() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(7)));
        for c in "sub".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("already exists"), "{}", app.status);
    }

    #[test]
    fn f5_copies_the_cursor_file_to_the_other_panel() {
        let (_root, mut app) = app_fixture();
        let destination = app.other_panel().cwd.clone();
        select(&mut app, "one.txt");

        app.on_key(key(KeyCode::F(5)));
        // The dialog is prefilled with the other panel's directory.
        match &app.mode {
            Mode::Input(d) => assert_eq!(d.value, destination.display().to_string()),
            other => panic!("expected input dialog, got {other:?}"),
        }
        app.on_key(key(KeyCode::Enter));
        // The copy runs on a worker thread; wait for it before asserting.
        app.finish_job();

        assert!(!app.status_is_error, "status was: {}", app.status);
        assert_eq!(
            fs::read_to_string(destination.join("one.txt")).unwrap(),
            "first"
        );
        // The source is untouched.
        assert!(app.active_panel().cwd.join("one.txt").exists());
    }

    #[test]
    fn f5_copies_every_marked_file() {
        let (_root, mut app) = app_fixture();
        let destination = app.other_panel().cwd.clone();

        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Char(' ')));
        select(&mut app, "two.txt");
        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.active_panel().marked_count(), 2);

        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));
        app.finish_job();

        assert!(destination.join("one.txt").exists());
        assert!(destination.join("two.txt").exists());
        // Marks are cleared once the operation completes.
        assert_eq!(app.active_panel().marked_count(), 0);
    }

    #[test]
    fn f6_moves_a_file_between_panels() {
        let (_root, mut app) = app_fixture();
        let source_dir = app.active_panel().cwd.clone();
        let destination = app.other_panel().cwd.clone();
        select(&mut app, "two.txt");

        app.on_key(key(KeyCode::F(6)));
        app.on_key(key(KeyCode::Enter));
        app.finish_job();

        assert!(!app.status_is_error, "status was: {}", app.status);
        assert!(destination.join("two.txt").exists());
        assert!(!source_dir.join("two.txt").exists());
    }

    #[test]
    fn f2_renames_the_cursor_entry() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        select(&mut app, "one.txt");

        app.on_key(key(KeyCode::F(2)));
        // The dialog starts from the current name; clear it first.
        for _ in 0.."one.txt".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        for c in "renamed.txt".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(cwd.join("renamed.txt").exists());
        assert!(!cwd.join("one.txt").exists());
    }

    #[test]
    fn f8_deletes_only_after_confirmation() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        select(&mut app, "one.txt");

        // Declining leaves the file alone.
        app.on_key(key(KeyCode::F(8)));
        app.on_key(key(KeyCode::Char('n')));
        app.finish_job();
        assert!(cwd.join("one.txt").exists());
        assert!(app.mode.is_normal());

        // Confirming removes it.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(8)));
        app.on_key(key(KeyCode::Char('y')));
        app.finish_job();
        assert!(!cwd.join("one.txt").exists());
    }

    #[test]
    fn delete_removes_a_whole_directory_tree() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        fs::write(cwd.join("sub/inner.txt"), "x").unwrap();
        app.active_panel_mut().reload();

        select(&mut app, "sub");
        app.on_key(key(KeyCode::F(8)));
        app.on_key(key(KeyCode::Enter));
        app.finish_job();

        assert!(!cwd.join("sub").exists());
    }

    #[test]
    fn escape_cancels_an_input_dialog() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();

        app.on_key(key(KeyCode::F(7)));
        for c in "nope".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Esc));

        assert!(app.mode.is_normal());
        assert!(!cwd.join("nope").exists());
    }

    #[test]
    fn parent_entry_cannot_be_deleted_or_renamed() {
        let (_root, mut app) = app_fixture();
        app.active_panel_mut().cursor_home();
        assert!(app.active_panel().selected().unwrap().is_parent());

        app.on_key(key(KeyCode::F(8)));
        assert!(
            app.mode.is_normal(),
            "delete dialog must not open on \"..\""
        );
        assert!(app.status_is_error);

        app.on_key(key(KeyCode::F(2)));
        assert!(
            app.mode.is_normal(),
            "rename dialog must not open on \"..\""
        );
    }

    #[test]
    fn ctrl_h_toggles_hidden_files() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        fs::write(cwd.join(".secret"), "s").unwrap();
        app.active_panel_mut().reload();

        let visible = |a: &App| a.active_panel().entries.iter().any(|e| e.name == ".secret");
        assert!(!visible(&app));

        app.on_key(ctrl('h'));
        assert!(visible(&app));

        app.on_key(ctrl('h'));
        assert!(!visible(&app));
    }

    #[test]
    fn ctrl_u_swaps_the_panels() {
        let (_root, mut app) = app_fixture();
        let left = app.left.cwd().to_path_buf();
        let right = app.right.cwd().to_path_buf();

        app.on_key(ctrl('u'));
        assert_eq!(app.left.cwd(), right);
        assert_eq!(app.right.cwd(), left);
    }

    #[test]
    fn star_inverts_the_marks() {
        let (_root, mut app) = app_fixture();
        let selectable = app.active_panel().entries.len() - 1; // ".." never marks

        app.on_key(key(KeyCode::Char('*')));
        assert_eq!(app.active_panel().marked_count(), selectable);
        app.on_key(key(KeyCode::Char('*')));
        assert_eq!(app.active_panel().marked_count(), 0);

        // Ctrl-A marks everything in one keystroke, as it does in the pointer
        // view.
        app.on_key(ctrl('a'));
        assert_eq!(app.active_panel().marked_count(), selectable);
    }

    #[test]
    fn plus_and_minus_ask_for_a_mask_rather_than_taking_the_lot() {
        // The grey plus asked for a mask - it did not mark everything. `*`
        // and Enter is still two keystrokes for that, which is what the
        // prompt offers by default.
        let (_root, mut app) = app_fixture();
        assert!(app.active_panel().entries.iter().any(|e| e.name == "sub"));

        app.on_key(key(KeyCode::Char('+')));
        match &app.mode {
            Mode::Input(dialog) => {
                assert_eq!(dialog.value, "*", "the default is not everything");
                assert!(matches!(
                    dialog.action,
                    InputAction::SelectPattern { select: true }
                ));
            }
            other => panic!("expected a mask prompt, got {other:?}"),
        }

        // Narrow it to the text files and confirm.
        for _ in 0..1 {
            app.on_key(key(KeyCode::Backspace));
        }
        for c in "*.txt".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.mode.is_normal());
        let marked: Vec<&str> = app
            .active_panel()
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(marked, vec!["one.txt", "two.txt"], "the directory came too");

        // And the minus takes a subset back out.
        app.on_key(key(KeyCode::Char('-')));
        for _ in 0..1 {
            app.on_key(key(KeyCode::Backspace));
        }
        for c in "one*".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        let marked: Vec<&str> = app
            .active_panel()
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(marked, vec!["two.txt"]);
    }

    #[test]
    fn f9_cycles_the_sort_order() {
        let (_root, mut app) = app_fixture();
        assert_eq!(app.active_panel().sort_by, SortBy::Name);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(app.active_panel().sort_by, SortBy::Ext);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(app.active_panel().sort_by, SortBy::Size);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(app.active_panel().sort_by, SortBy::Time);
        app.on_key(key(KeyCode::F(9)));
        assert_eq!(app.active_panel().sort_by, SortBy::Name);
    }

    #[test]
    fn f4_requests_an_editor_for_files_only() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(4)));
        assert!(app.pending_edit.is_some());

        app.pending_edit = None;
        select(&mut app, "sub");
        app.on_key(key(KeyCode::F(4)));
        assert!(app.pending_edit.is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn f1_opens_and_closes_help() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(1)));
        assert!(matches!(app.mode, Mode::Help { .. }));
        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());
    }

    #[test]
    fn navigating_fills_the_recent_list_newest_first() {
        let (_root, mut app) = app_fixture();
        let start = app.active_panel().cwd.clone();

        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter)); // into sub
        app.on_key(key(KeyCode::Backspace)); // back out

        let recent: Vec<String> = app
            .bookmarks
            .recent
            .iter()
            .map(|l| l.path.clone())
            .collect();

        // Most recent first: back at the start, having been in sub.
        assert_eq!(recent[0], start.display().to_string());
        assert_eq!(recent[1], start.join("sub").display().to_string());
    }

    #[test]
    fn revisiting_moves_an_entry_up_rather_than_duplicating() {
        let (_root, mut app) = app_fixture();
        let start = app.active_panel().cwd.clone();

        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Backspace));
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));

        let sub = start.join("sub").display().to_string();
        assert_eq!(app.bookmarks.recent[0].path, sub);
        assert_eq!(
            app.bookmarks
                .recent
                .iter()
                .filter(|l| l.path == sub)
                .count(),
            1
        );
    }

    #[test]
    fn the_tree_and_bookmarks_also_feed_the_recent_list() {
        let (root, mut app) = app_fixture();
        let target = root.path().join("elsewhere");
        fs::create_dir_all(&target).unwrap();
        app.bookmarks.add(Location::local(target.clone()));

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(
            app.bookmarks.recent[0].path,
            target.display().to_string(),
            "jumping via a bookmark should be remembered too"
        );
    }

    #[test]
    fn capital_c_forgets_the_whole_history() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        assert!(!app.bookmarks.recent.is_empty());

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('C')));

        assert!(app.bookmarks.recent.is_empty());
        assert!(app.status.contains("Forgot"), "{}", app.status);
    }

    #[test]
    fn tab_walks_the_three_lists_and_comes_back_round() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(11)));
        match app.mode {
            Mode::Connections { tab, .. } => assert_eq!(tab, ConnTab::Saved),
            _ => panic!("expected the connections screen"),
        }

        app.on_key(key(KeyCode::Tab));
        match app.mode {
            Mode::Connections { tab, cursor } => {
                assert_eq!(tab, ConnTab::Recent);
                assert_eq!(cursor, 0, "the cursor resets when switching lists");
            }
            _ => panic!("expected the connections screen"),
        }

        // Three now: the machine's own drives and folders are the third,
        // and are the only way to reach a second drive without knowing its
        // letter and typing it.
        app.on_key(key(KeyCode::Tab));
        match app.mode {
            Mode::Connections { tab, .. } => assert_eq!(tab, ConnTab::System),
            _ => panic!("expected the connections screen"),
        }

        app.on_key(key(KeyCode::Tab));
        match app.mode {
            Mode::Connections { tab, .. } => assert_eq!(tab, ConnTab::Saved),
            _ => panic!("expected the connections screen"),
        }
    }

    #[test]
    fn the_machine_offers_at_least_one_place_to_go() {
        // A drive is always there - the root on Unix, at least one letter on
        // Windows - so an empty list means discovery is broken rather than
        // that the machine has no disks.
        let (_root, app) = app_fixture();
        assert!(!app.system_places.is_empty());
    }

    #[test]
    fn enter_on_a_recent_entry_goes_back_there() {
        let (_root, mut app) = app_fixture();
        let start = app.active_panel().cwd.clone();

        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter)); // now in sub, recent[0] = sub

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Tab)); // Recent
        app.on_key(key(KeyCode::Down)); // recent[1] = the starting directory
        app.on_key(key(KeyCode::Enter));

        assert!(!app.status_is_error, "{}", app.status);
        assert_eq!(app.active_panel().cwd, start);
    }

    #[test]
    fn s_promotes_a_recent_entry_into_the_saved_list() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));

        assert!(app.bookmarks.is_empty());

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('s')));

        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks.locations[0].name, "sub");
    }

    #[test]
    fn d_forgets_a_recent_entry_without_touching_bookmarks() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Backspace));
        let before = app.bookmarks.recent.len();

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('d')));

        assert_eq!(app.bookmarks.recent.len(), before - 1);
        assert!(app.bookmarks.is_empty());
    }

    #[test]
    fn recent_is_written_out_when_quitting() {
        let (root, mut app) = app_fixture();
        let path = root.path().join("bookmarks.toml");

        select(&mut app, "sub");
        app.on_key(key(KeyCode::Enter));
        app.persist_on_exit();

        let reloaded = Bookmarks::load_from(&path).unwrap();
        assert!(!reloaded.recent.is_empty());
    }

    #[test]
    fn a_bookmark_taken_inside_a_share_records_the_network_location() {
        let (root, mut app) = app_fixture();
        // Pretend a share was attached at this local path.
        let mount_point = root.path().join("mnt");
        fs::create_dir_all(mount_point.join("photos/2024")).unwrap();
        app.active_mounts.push((
            mount_point.clone(),
            Location::parse("smb://alex@nas.local/media").unwrap(),
        ));

        let inside = mount_point.join("photos/2024");
        let location = app.location_for(&inside);

        // Not the mount path, which would be gone next session.
        assert_eq!(location.protocol, Protocol::Smb);
        assert_eq!(location.host, "nas.local");
        assert_eq!(location.path, "media/photos/2024");
        assert_eq!(location.to_url(), "smb://alex@nas.local/media/photos/2024");
    }

    #[test]
    fn paths_outside_any_share_stay_local() {
        let (root, mut app) = app_fixture();
        app.active_mounts.push((
            root.path().join("mnt"),
            Location::parse("smb://nas.local/media").unwrap(),
        ));

        let location = app.location_for(&root.path().join("left"));
        assert_eq!(location.protocol, Protocol::Local);
    }

    #[test]
    fn alt_t_opens_the_tree_on_the_current_directory() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();

        app.on_key(alt('t'));

        assert!(app.active_panel().in_tree_mode());
        let tree = app.active_panel().tree.as_ref().unwrap();
        // The tree opens rooted at the filesystem root, revealed down to cwd.
        assert_eq!(tree.selected_path().unwrap(), cwd);
        assert_eq!(
            tree.nodes[0].path,
            lost_commander_core::tree::filesystem_root(&cwd)
        );
    }

    #[test]
    fn alt_t_closes_the_tree_again() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('t'));
        assert!(app.active_panel().in_tree_mode());
        app.on_key(alt('t'));
        assert!(!app.active_panel().in_tree_mode());
    }

    #[test]
    fn enter_in_the_tree_moves_the_panel_and_keeps_the_tree() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        app.on_key(alt('t'));

        // Open the current directory and step onto its "sub" child.
        app.on_key(key(KeyCode::Right));
        let target = cwd.join("sub");
        let index = app
            .active_panel()
            .tree
            .as_ref()
            .unwrap()
            .index_of(&target)
            .expect("sub should be listed");
        app.active_panel_mut().tree.as_mut().unwrap().cursor = index;

        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.active_panel().cwd, target);
        // The tree stays. Closing it made this a directory chooser rather
        // than a tree you can work in, and walking from one directory to the
        // next tagging files as you go is the whole reason to have one.
        assert!(app.active_panel().in_tree_mode());
        // ...and the keyboard goes down into the files it just opened.
        // Escape is the way back up. Tab is left meaning the other pane.
        assert!(!app.on_tree[0]);
        app.on_key(key(KeyCode::Esc));
        assert!(app.on_tree[0], "Escape climbs back to the tree");
        assert!(!app.status_is_error, "{}", app.status);
    }

    #[test]
    fn escape_leaves_the_tree_without_moving() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();

        app.on_key(alt('t'));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Esc));

        assert!(!app.active_panel().in_tree_mode());
        assert_eq!(app.active_panel().cwd, cwd);
    }

    #[test]
    fn right_expands_and_left_collapses_then_walks_out() {
        let (_root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();
        app.on_key(alt('t'));

        app.on_key(key(KeyCode::Right));
        let expanded = app
            .active_panel()
            .tree
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .expanded;
        assert!(expanded, "Right should expand the node");

        app.on_key(key(KeyCode::Left));
        let still_expanded = app
            .active_panel()
            .tree
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .expanded;
        assert!(!still_expanded, "Left should collapse it");

        // Collapsed already: Left now steps out to the parent directory.
        app.on_key(key(KeyCode::Left));
        let selected = app
            .active_panel()
            .tree
            .as_ref()
            .unwrap()
            .selected_path()
            .unwrap();
        assert_eq!(selected, cwd.parent().unwrap());
    }

    #[test]
    fn tab_still_switches_panels_while_the_tree_is_open() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('t'));

        // Keys the tree ignores must still reach the normal bindings.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active, Side::Right);
        assert!(!app.active_panel().in_tree_mode());
        assert!(
            app.left.current().in_tree_mode(),
            "the left panel keeps its tree"
        );

        app.on_key(key(KeyCode::F(10)));
        assert!(app.should_quit);
    }

    #[test]
    fn the_tree_only_lists_directories() {
        let (_root, mut app) = app_fixture();
        app.on_key(alt('t'));
        app.on_key(key(KeyCode::Right));

        let labels: Vec<String> = app
            .active_panel()
            .tree
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.label.clone())
            .collect();

        assert!(labels.contains(&"sub".to_string()));
        assert!(!labels.contains(&"one.txt".to_string()));
    }

    #[test]
    fn a_copy_shows_the_progress_dialog_until_it_finishes() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");

        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));

        // The operation is now owned by a worker thread.
        assert!(matches!(app.mode, Mode::Progress));
        assert!(app.job_is_running());

        app.finish_job();

        assert!(app.mode.is_normal());
        assert!(!app.job_is_running());
        assert!(!app.status_is_error, "status was: {}", app.status);
        assert!(app.status.contains("Copied"), "{}", app.status);
    }

    #[test]
    fn a_delete_also_runs_behind_the_progress_dialog() {
        // Shift-F8, so this stays a test about the progress dialog rather
        // than one that writes into the real user's trash. The trash
        // mechanics have their own tests, against a temporary directory.
        let (_root, mut app) = app_fixture();
        select(&mut app, "sub");

        app.on_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Progress));

        app.finish_job();
        assert!(app.status.contains("Deleted"), "{}", app.status);
    }

    #[test]
    fn f8_asks_about_the_trash_and_shift_f8_asks_about_for_good() {
        // Which of the two a key means, without running either: the trash is
        // the user's real one and a test has no business in it.
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");

        app.on_key(key(KeyCode::F(8)));
        match &app.mode {
            Mode::Confirm(dialog) => {
                assert!(matches!(
                    dialog.action,
                    ConfirmAction::Delete { to_trash: true, .. }
                ));
                assert!(dialog.message.contains("trash"), "{}", dialog.message);
                assert!(
                    !dialog.message.contains("cannot be undone"),
                    "the trash can be undone: {}",
                    dialog.message
                );
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
        app.on_key(key(KeyCode::Esc));

        select(&mut app, "one.txt");
        app.on_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT));
        match &app.mode {
            Mode::Confirm(dialog) => {
                assert!(matches!(
                    dialog.action,
                    ConfirmAction::Delete {
                        to_trash: false,
                        ..
                    }
                ));
                assert!(
                    dialog.message.contains("cannot be undone"),
                    "{}",
                    dialog.message
                );
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
        // Nothing has happened either way: the question comes first.
        assert!(app.active_panel().cwd.join("one.txt").exists());
    }

    #[test]
    fn escape_during_a_job_requests_cancellation() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");

        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Esc));

        // Esc must not tear down the dialog behind a running worker.
        assert!(matches!(app.mode, Mode::Progress));

        app.finish_job();
        assert!(app.mode.is_normal());
    }

    #[test]
    fn progress_reports_totals_scanned_from_the_source() {
        let (_root, mut app) = app_fixture();
        // one.txt is 5 bytes, two.txt is 6.
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::Char(' ')));
        select(&mut app, "two.txt");
        app.on_key(key(KeyCode::Char(' ')));

        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));
        app.finish_job();

        assert!(app.status.contains("2 item(s)"), "{}", app.status);
    }

    #[test]
    fn a_second_operation_is_refused_while_one_is_running() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));

        // Force the second start while the first job is still owned.
        app.mode = Mode::Normal;
        select(&mut app, "two.txt");
        app.on_key(key(KeyCode::F(5)));
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("already running"), "{}", app.status);

        app.finish_job();
    }

    #[test]
    fn f11_and_ctrl_b_open_the_connections_screen() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(11)));
        assert!(matches!(app.mode, Mode::Connections { .. }));
        app.on_key(key(KeyCode::Esc));
        assert!(app.mode.is_normal());

        app.on_key(ctrl('b'));
        assert!(matches!(app.mode, Mode::Connections { .. }));
    }

    #[test]
    fn ctrl_d_bookmarks_the_current_directory_and_persists_it() {
        let (root, mut app) = app_fixture();
        let cwd = app.active_panel().cwd.clone();

        app.on_key(ctrl('d'));

        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks.locations[0].path, cwd.display().to_string());
        assert_eq!(app.bookmarks.locations[0].protocol, Protocol::Local);

        // It reached disk, and reloads identically.
        let saved = root.path().join("bookmarks.toml");
        assert!(saved.exists());
        let reloaded = Bookmarks::load_from(&saved).unwrap();
        assert_eq!(reloaded.len(), 1);
    }

    #[test]
    fn adding_a_network_location_through_the_dialog() {
        let (_root, mut app) = app_fixture();

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Char('a')));
        for c in "smb://alex@nas.local/media".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(!app.status_is_error, "status was: {}", app.status);
        assert_eq!(app.bookmarks.len(), 1);
        let saved = &app.bookmarks.locations[0];
        assert_eq!(saved.protocol, Protocol::Smb);
        assert_eq!(saved.host, "nas.local");
        assert_eq!(saved.user.as_deref(), Some("alex"));
        assert_eq!(saved.share(), "media");
    }

    #[test]
    fn a_malformed_location_is_rejected_with_a_message() {
        let (_root, mut app) = app_fixture();

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Char('a')));
        for c in "gopher://nope/x".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("Bad location"), "{}", app.status);
        assert!(app.bookmarks.is_empty());
    }

    #[test]
    fn connections_screen_navigates_and_deletes() {
        let (_root, mut app) = app_fixture();
        app.bookmarks
            .add(Location::parse("smb://a.local/one").unwrap());
        app.bookmarks
            .add(Location::parse("smb://b.local/two").unwrap());
        app.bookmarks
            .add(Location::parse("ftp://c.local/three").unwrap());

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        match app.mode {
            Mode::Connections { cursor, .. } => assert_eq!(cursor, 2),
            _ => panic!("expected the connections screen"),
        }

        // Cursor cannot run past the end.
        app.on_key(key(KeyCode::Down));
        match app.mode {
            Mode::Connections { cursor, .. } => assert_eq!(cursor, 2),
            _ => panic!("expected the connections screen"),
        }

        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.bookmarks.len(), 2);
        // The cursor stays in range after the deletion.
        match app.mode {
            Mode::Connections { cursor, .. } => assert_eq!(cursor, 1),
            _ => panic!("expected the connections screen"),
        }
    }

    #[test]
    fn connecting_to_a_local_bookmark_jumps_the_active_panel() {
        let (root, mut app) = app_fixture();
        let target = root.path().join("elsewhere");
        fs::create_dir_all(&target).unwrap();
        app.bookmarks.add(Location::local(target.clone()));

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Enter));

        assert!(!app.status_is_error, "status was: {}", app.status);
        assert!(app.mode.is_normal());
        assert_eq!(app.active_panel().cwd, target);
    }

    #[test]
    fn connecting_to_a_missing_local_bookmark_reports_an_error() {
        let (root, mut app) = app_fixture();
        app.bookmarks
            .add(Location::local(root.path().join("not-there")));

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("Not reachable"), "{}", app.status);
    }

    #[test]
    fn enter_on_an_empty_connections_list_explains_itself() {
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("press 'a'"), "{}", app.status);
    }

    #[test]
    fn bookmarks_reload_from_disk_across_sessions() {
        let (root, mut app) = app_fixture();
        let path = root.path().join("bookmarks.toml");

        app.on_key(key(KeyCode::F(11)));
        app.on_key(key(KeyCode::Char('a')));
        for c in "smb://nas.local/media".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        // A fresh App pointed at the same file sees the same locations.
        let mut next = App::detached(root.path().to_path_buf(), root.path().to_path_buf());
        next.bookmarks = Bookmarks::load_from(&path).unwrap();
        assert_eq!(next.bookmarks.len(), 1);
        assert_eq!(next.bookmarks.locations[0].host, "nas.local");
    }

    #[test]
    fn copy_to_a_missing_directory_reports_an_error() {
        let (_root, mut app) = app_fixture();
        select(&mut app, "one.txt");
        app.on_key(key(KeyCode::F(5)));

        // Replace the prefilled destination with one that does not exist.
        if let Mode::Input(d) = &mut app.mode {
            d.value = "/definitely/not/here".into();
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.status_is_error);
        assert!(app.status.contains("Not a directory"), "{}", app.status);
    }

    #[test]
    fn the_status_counts_what_is_tagged_across_the_tree() {
        let (root, mut app) = app_fixture();
        app.toggle_tree();
        app.active_panel_mut().cursor_to(1);
        app.active_panel_mut().toggle_mark();

        // Tag something in a directory the pane is not showing, which is what
        // walking a tree does and what makes the count worth printing.
        let elsewhere = root.path().join("sub");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("far.txt"), b"x").unwrap();
        app.active_panel_mut()
            .tagged
            .insert(elsewhere.join("far.txt"));

        // The status shows what the program last said; these are about what
        // it shows when it has nothing to say, which is the state after the
        // next keystroke.
        app.status.clear();
        let line = crate::ui::status_line(&app);
        assert!(
            line.contains("2 tagged across the tree"),
            "the one number a reader cannot get by looking: {line}"
        );
    }

    #[test]
    fn without_a_tree_the_status_says_what_it_always_said() {
        let (_root, mut app) = app_fixture();
        app.active_panel_mut().cursor_to(1);
        app.active_panel_mut().toggle_mark();

        // The status shows what the program last said; these are about what
        // it shows when it has nothing to say, which is the state after the
        // next keystroke.
        app.status.clear();
        let line = crate::ui::status_line(&app);
        assert!(line.contains("marked"), "{line}");
        assert!(!line.contains("tagged"), "no tree, no tally: {line}");
    }

    #[test]
    fn the_status_describes_whichever_half_has_the_keyboard() {
        let (_root, mut app) = app_fixture();
        app.toggle_tree();
        assert!(app.on_tree[0], "the tree has it to begin with");
        // The status shows what the program last said; these are about what
        // it shows when it has nothing to say, which is the state after the
        // next keystroke.
        app.status.clear();
        assert!(
            crate::ui::status_line(&app).contains("(tree)"),
            "in the tree, it describes the tree"
        );

        // Enter drops into the files, and the status has to follow - saying
        // "(tree)" while the arrows move a file cursor would be describing
        // the half you are not in.
        app.on_key(key(KeyCode::Enter));
        assert!(!app.on_tree[0]);
        assert!(!crate::ui::status_line(&app).contains("(tree)"));
    }

    #[test]
    fn there_is_more_than_one_way_out() {
        // A terminal that keeps F10 for its own menu - GNOME Terminal does -
        // leaves a reader with no way to quit at all, which is how this was
        // reported: "I couldn't figure out how to quit."
        let (_root, mut app) = app_fixture();
        app.on_key(key(KeyCode::F(10)));
        assert!(app.should_quit);

        let (_root2, mut app2) = app_fixture();
        app2.on_key(ctrl_key(KeyCode::Char('q')));
        assert!(app2.should_quit, "Ctrl-Q always reaches us");
    }

    #[test]
    fn ctrl_c_stops_the_nearest_thing_and_quits_when_there_is_none() {
        let (_root, mut app) = app_fixture();

        // A half-typed command is nearer than the program itself.
        app.on_key(key(KeyCode::Char('r')));
        app.on_key(key(KeyCode::Char('m')));
        app.on_key(ctrl_key(KeyCode::Char('c')));
        assert!(app.command.is_empty(), "the line is cleared");
        assert!(!app.should_quit, "and that is all it did");

        // With nothing to interrupt, it means what it means everywhere else.
        app.on_key(ctrl_key(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_z_asks_the_main_loop_to_suspend() {
        // The key cannot do it itself: the terminal has to be handed back
        // first, and the main loop is what owns it. A process stopped while
        // the screen is in raw mode leaves the shell unusable.
        let (_root, mut app) = app_fixture();
        app.on_key(ctrl_key(KeyCode::Char('z')));
        assert!(app.pending_suspend);
        assert!(!app.should_quit, "suspending is not quitting");
    }

    #[test]
    fn an_unhooked_shell_is_sent_to_the_panel_before_every_command() {
        // `cmd` cannot say where it is, so the panel is the answer - the way
        // Far Manager does it on Windows. Otherwise a `cd` typed into the
        // shell moves it somewhere the panel never learns about, and every
        // command afterwards runs somewhere other than the prompt says.
        let lines = command_lines("cmd.exe", Path::new(r"C:\src"), "dir");
        assert_eq!(
            lines,
            vec![r#"cd /d "C:\src""#.to_string(), "dir".to_string()]
        );
    }

    #[test]
    fn a_hooked_shell_is_left_where_it_is() {
        // It reports where it goes and both sides follow, so sending it back
        // would undo a `cd` the reader meant.
        let lines = command_lines("/bin/bash", Path::new("/home/you"), "ls");
        assert_eq!(lines, vec!["ls".to_string()]);
    }

    #[test]
    fn only_a_shell_that_can_answer_is_told_where_the_panel_went() {
        let (_root, mut app) = app_fixture();
        app.shell_program = Some("cmd.exe".to_string());
        // Nothing to write into: the point is that it does not try.
        app.tell_the_shell();
        assert!(app.shell.is_none(), "and it did not start one to say it");
    }

    #[test]
    fn a_command_can_be_taken_back_out_of_the_account() {
        let event = journal::Event::new(journal::Kind::Command, "/work").note("cargo test");
        let (line, ran_in) = command_to_reuse(&event).expect("a command");
        assert_eq!(line, "cargo test");
        assert_eq!(ran_in, "/work", "where it ran is part of the answer");
    }

    #[test]
    fn what_was_never_typed_is_not_offered_for_running() {
        // A copy is a record of something that happened, not a line anybody
        // wrote, and "run it again" would be offering to do something the
        // reader never asked for.
        let copy = journal::Event::new(journal::Kind::Copy, "/a").to("/b");
        assert!(command_to_reuse(&copy).is_none());

        // And a command with nothing recorded under it is nothing to run.
        let empty = journal::Event::new(journal::Kind::Command, "/work");
        assert!(command_to_reuse(&empty).is_none());
    }

    #[test]
    fn a_copy_opens_both_ends_as_tabs_without_losing_where_you_were() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let to = root.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();

        let (_root, mut app) = app_fixture();
        let before = (app.left.len(), app.right.len());
        let was = app.left.cwd().to_path_buf();

        app.open_scene(&journal::Scene {
            left: from.clone(),
            right: Some(to.clone()),
        });

        assert_eq!(app.left.len(), before.0 + 1, "a tab, not a move");
        assert_eq!(app.right.len(), before.1 + 1);
        assert_eq!(app.left.cwd(), from);
        assert_eq!(app.right.cwd(), to);
        // The tab you were in is still there, still where it was.
        app.left.close();
        assert_eq!(app.left.cwd(), was);
    }

    #[test]
    fn a_place_that_is_gone_is_named_rather_than_ignored() {
        // Deleted since, or on a disk nobody has mounted today. "Nothing
        // happened" would leave the reader with no idea which.
        let (_root, mut app) = app_fixture();
        let before = app.left.len();
        app.open_scene(&journal::Scene {
            left: PathBuf::from("/no/such/place"),
            right: None,
        });
        assert_eq!(app.left.len(), before, "nothing opened");
        assert!(app.status_is_error, "and it said so: {}", app.status);
        assert!(app.status.contains("Gone"), "{}", app.status);
    }

    #[test]
    fn one_end_missing_still_opens_the_other() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        std::fs::create_dir_all(&from).unwrap();

        let (_root, mut app) = app_fixture();
        let before = app.left.len();
        app.open_scene(&journal::Scene {
            left: from.clone(),
            right: Some(PathBuf::from("/no/such/place")),
        });
        assert_eq!(app.left.len(), before + 1);
        assert!(app.status.contains("gone"), "{}", app.status);
    }
}
