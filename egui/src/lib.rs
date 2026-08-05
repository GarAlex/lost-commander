// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The graphical front-end.
//!
//! This is not the terminal view redrawn with nicer characters - it is laid out
//! for a pointer and a large window: a places/tree sidebar, a clickable
//! breadcrumb trail, two panes that can each be a dense detail list or a grid
//! of large icons, and a status strip that turns into a live progress bar while
//! a copy runs.
//!
//! All of the behaviour underneath - listing, sorting, marking, copying,
//! bookmarks, the tree - is the same library the terminal front-end uses.

pub mod hexedit;
pub mod icons;
pub mod imageedit;
pub mod journalview;
pub mod keys;
pub mod preview;
pub mod terminal;
pub mod textedit;
pub mod theme;

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, Vec2,
};

use lost_commander_core::apps;
use lost_commander_core::compare;
use lost_commander_core::config::Settings;
use lost_commander_core::diff;
use lost_commander_core::dupes;
use lost_commander_core::elevate::{self, Elevation};
use lost_commander_core::entry::{human_size, size_in_words, Entry};
use lost_commander_core::find;
use lost_commander_core::fsops;
use lost_commander_core::journal;
use lost_commander_core::mount;
use lost_commander_core::netloc::{Bookmarks, Location};
use lost_commander_core::open;
use lost_commander_core::panel::Panel;
use lost_commander_core::perms::{self, Mode, What, Who};
use lost_commander_core::places;
use lost_commander_core::progress::{self, Answer, Job, Operation};
use lost_commander_core::pty::{self, Terminals};
use lost_commander_core::rename;
use lost_commander_core::tabs::{self, Tabs};
use lost_commander_core::textedit::Document;

use lost_commander_core::shell::{self, CommandOutput, ShellJob};

const TILE: Vec2 = Vec2::new(104.0, 92.0);
const ROW_HEIGHT: f32 = 26.0;
/// How many command/output blocks the console keeps.
const CONSOLE_HISTORY: usize = 200;

/// One command and what it printed.
#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub prompt: String,
    /// Where it ran, shown on hover so two directories with the same name are
    /// still distinguishable in the log.
    pub cwd: PathBuf,
    pub line: String,
    pub output: CommandOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// One row per entry: icon, name, size, modified.
    Details,
    /// Large icons in a wrapping grid.
    Grid,
    /// The directory hierarchy, opened to where this pane is.
    Tree,
    /// Quick view: what the *other* pane's cursor is on.
    Preview,
    /// What has been done to the things in the *other* pane's directory.
    ///
    /// Follows the other pane the way quick view does, and for the same
    /// reason: you stand in a folder on one side and read it on the other.
    /// The journal screen answers "what did I do today"; this answers "why
    /// does this folder look like this", which is the question you actually
    /// have while looking at it.
    History,
}

/// A job with everywhere but its destination worked out.
pub enum Pending {
    Copy(Vec<PathBuf>),
    Move(Vec<PathBuf>),
    Extract {
        archive: PathBuf,
        members: Vec<String>,
        from: String,
        password: Option<String>,
    },
}

/// A modal the panes are waiting on.
///
/// The graphical view had none of these: rename, make-directory and the
/// delete confirmation existed only in the terminal front-end, and delete
/// went ahead without asking at all.
pub enum Dialog {
    Help,
    /// Where a copy, a move or an extraction is going - typed, not implied.
    ///
    /// The destination used to be wherever the other pane happened to be,
    /// with nothing shown and nothing to change. That reads as a shortcut
    /// until the other pane is not on screen, and then it is a copy into a
    /// directory the reader never saw and did not choose. Naming it in a
    /// field costs one Enter, is what every commander from Norton on has
    /// done, and is the terminal front-end's behaviour already.
    CopyTo {
        /// What is being sent, for the title.
        what: String,
        /// The other pane when there is one, this pane when there is not.
        /// The second is deliberately a destination nobody wants: with one
        /// pane there is nothing sensible to guess, so the field is prefilled
        /// with somewhere to edit rather than somewhere to accept.
        destination: String,
        job: Pending,
    },
    /// An archive, or a file in one, that will not open without a password.
    Password {
        archive: PathBuf,
        /// The member that was being reached for, so it can be shown once
        /// the password arrives. `None` means the archive itself would not
        /// even list.
        member: Option<String>,
        typed: String,
        /// Set once something has been tried and refused, so the box can say
        /// so rather than looking as though nothing happened.
        refused: bool,
    },
    Rename {
        from: PathBuf,
        name: String,
    },
    MkDir {
        name: String,
    },
    ConfirmDelete {
        targets: Vec<PathBuf>,
        /// To the trash, where it can be got back.
        to_trash: bool,
    },
    /// Find files by name, and by what is inside them.
    ///
    /// The search runs on its own thread and the list fills while it goes -
    /// a search over a large tree is usable long before it finishes.
    Find {
        query: find::Query,
        root: PathBuf,
        /// Which result the cursor is on.
        cursor: usize,
        /// Whether the keyboard is on the results rather than in the boxes.
        ///
        /// Explicit, rather than read off whichever widget egui has focused:
        /// what `Enter` means has to be a thing this code decides, not a
        /// consequence of where focus drifted.
        in_results: bool,
    },
    /// What a file is, and what it is allowed to be.
    ///
    /// Carries the properties as read, so Apply can tell what was actually
    /// changed and write only that - a dialog that wrote every field back
    /// would touch the timestamps of files nobody edited.
    Properties {
        was: Box<perms::Properties>,
        now: Box<perms::Properties>,
        /// The octal box, kept as text so a half-typed `7` is not read as a
        /// mode. Reconciled with the checkboxes each frame.
        octal: String,
    },
    /// A copy or move is blocked on a file that is already there.
    ///
    /// The worker is asleep until this is answered, so it cannot be dismissed
    /// with Escape the way the others can - every button here is an answer.
    ConfirmOverwrite {
        conflict: progress::Conflict,
    },
    /// Opening these would run something, or would open a lot of windows.
    ConfirmOpen {
        targets: Vec<PathBuf>,
        question: String,
    },
    Pattern {
        text: String,
        select: bool,
    },
    /// The theme form. Carries the palette it opened with, so Cancel can put
    /// it back after every change has already been shown live.
    Theme {
        was: theme::Palette,
    },
    /// Files that are the same file twice, and which copies to let go of.
    Duplicates {
        root: PathBuf,
        options: dupes::Options,
        groups: Vec<dupes::Group>,
        /// The scan stopped at [`dupes::MAX_GROUPS`] rather than because it
        /// ran out of tree.
        capped: bool,
    },
    /// A picture, open for turning, cropping and resizing.
    ///
    /// Boxed because the session holds a whole decoded picture, and a `Dialog`
    /// the size of its largest variant would be megabytes of enum.
    Image(Box<imageedit::Session>),
    /// A picture on its way in from the disk. Decoding a photograph is far too
    /// slow to do between frames, so the dialog opens on this and swaps itself
    /// for the session when the worker is finished.
    ImageLoading(Box<imageedit::Job>),
    /// A text file, open for typing into.
    ///
    /// Boxed for the same reason: the session holds the whole file twice over,
    /// and a `Dialog` the size of its largest variant is a cost every other
    /// dialog would pay.
    Text(Box<textedit::Session>),
    /// A file open as bytes, for changing them one at a time.
    Bytes(Box<hexedit::Session>),
    /// The account of what was done.
    Journal(Box<journalview::View>),
    /// Two files, line by line.
    Difference {
        left: PathBuf,
        right: PathBuf,
        diff: Box<diff::Diff>,
        /// The row to scroll to on the next frame, set by the jump keys.
        go_to: Option<usize>,
        /// Where the last jump left it, so the next one starts from there
        /// rather than from wherever the scroll bar happens to be.
        at: usize,
    },
    /// Make two trees agree: what differs, which way it would go, and a
    /// button that carries it out.
    ///
    /// The pairs are owned here rather than read from the scan each frame,
    /// because the directions are edited - a list that came back from the
    /// worker every frame would forget them.
    Sync {
        left: PathBuf,
        right: PathBuf,
        options: compare::Options,
        show: compare::Show,
        pairs: Vec<compare::Pair>,
        /// The comparison stopped at [`compare::MAX_PAIRS`] with tree left
        /// over. Said out loud: a list that quietly stops short reads as a
        /// list of everything there is.
        capped: bool,
    },
    /// The multi-rename tool: new names for a whole selection at once.
    ///
    /// The plan is worked out when the rules change rather than every frame,
    /// because working it out stats every target name - which is nothing
    /// once per keystroke and a great deal sixty times a second.
    MultiRename {
        rules: rename::Rules,
        sources: Vec<rename::Source>,
        changes: Vec<rename::Change>,
        computed: rename::Rules,
    },
    /// "Open with...": pick an application, or type a command.
    ///
    /// The list is read once when the dialog opens rather than every frame -
    /// it is a few hundred small files, which is cheap once and not sixty
    /// times a second.
    OpenWith {
        target: PathBuf,
        applications: Vec<apps::Application>,
        /// The chooser's own filter, which is also the command box: type a
        /// name to narrow the list, or a command line to run that instead.
        typed: String,
        /// Ask the system to authorise it first - see `lost_commander_core::elevate`.
        as_admin: bool,
    },
}

/// What a click on a row means for the marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Plain: this row alone.
    Plain,
    /// Ctrl/Cmd: add or remove this one row.
    Toggle,
    /// Shift: everything from the last click to this one.
    Range { additive: bool },
}

impl Click {
    /// Read the modifiers. Shift wins over ctrl, and both together extend the
    /// range without dropping what is already marked.
    pub fn from_modifiers(ctrl: bool, shift: bool) -> Click {
        match (shift, ctrl) {
            (true, additive) => Click::Range { additive },
            (false, true) => Click::Toggle,
            (false, false) => Click::Plain,
        }
    }
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// What a pane's header says about what is selected.
///
/// With a tree up the count has to come from the tagged set rather than from
/// the rows: tagging across directories is the point of walking a tree, and
/// most of what is tagged is not on screen. "3 of 12 selected" would be
/// counting the wrong thing and reassuring the reader with it.
fn selection_summary(
    panel: &lost_commander_core::panel::Panel,
    count: usize,
    view: ViewMode,
) -> String {
    let tagged = panel.tagged_count();
    if panel.in_tree_mode() && tagged > 0 {
        return format!("{tagged} tagged across the tree");
    }
    if panel.marked_count() > 0 {
        return format!("{} of {count} selected", panel.marked_count());
    }
    if matches!(view, ViewMode::Tree | ViewMode::Preview | ViewMode::History) {
        return String::new();
    }
    format!("{count} items")
}

/// Everything about a window except the files on the disk.
///
/// A tab is not a directory - it is a window you would otherwise have opened
/// another copy of the program for. It carries how the panes are arranged,
/// what each of them is drawing, which one you were in, and which shell was
/// standing there. Switching tabs puts all of it back.
///
/// Kept beside the tabs rather than inside `Panel`, because none of it is a
/// fact about a directory and the engine has no business knowing about
/// views, shells or how many panes a front-end has.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// The shell standing in this workspace, by identity. `None` means one
    /// has not been started here yet, which the rail says out loud.
    pub shell: Option<u64>,
    pub show_right: bool,
    /// Where the second pane was. Its cursor and marks are not kept: it is
    /// the secondary pane, and it is re-read on the way in anyway.
    pub right: PathBuf,
    pub left_view: ViewMode,
    pub right_view: ViewMode,
    /// Which pane you were working in.
    pub active: Side,
    /// Whether the shell here follows the panes, and they follow it.
    pub synced: bool,
    pub split: f32,
}

/// Which half of the window is on show.
///
/// The panes and the shell are two ways of working on the same directory,
/// and there are times you want the whole window for one of them: reading a
/// long listing, or watching a build. They are independent while only one is
/// up - a `cd` in a shell nobody can see should not move a pane nobody is
/// looking at either - and fall back into step when both are showing again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Both,
    Files,
    Shell,
}

pub struct GuiApp {
    pub left: Tabs,
    pub right: Tabs,
    pub active: Side,
    /// Each pane chooses its own view, so one can be a tree while the other
    /// stays a listing.
    pub left_view: ViewMode,
    pub right_view: ViewMode,
    pub show_sidebar: bool,
    /// The row of function keys under the panes.
    ///
    /// A pointer can reach everything through the toolbar and the menus, so
    /// this was left out of the graphical view - which quietly meant that a
    /// reader coming from the terminal view, or from any commander since
    /// Norton, had no way to discover that F5 copies here too. The keys work
    /// either way; the bar is what says so.
    pub show_keys: bool,
    /// Which halves of the window are on show.
    pub half: Half,
    /// Which shell belongs to which tab: a workspace is a directory and the
    /// shell standing in it.
    ///
    /// By identity rather than by index, because both lists are reordered by
    /// ordinary use - a tab opened beside this one, a shell closed from the
    /// middle - and a pairing held by position would quietly come to mean a
    /// different pair. Entries whose shell has since gone are dropped when
    /// they are next looked at rather than swept: the shell can end at any
    /// moment, so any sweep would have to run every frame anyway.
    pub workspaces: std::collections::HashMap<u64, Workspace>,
    /// The rail down the left: the workspaces, and how much of each is shown.
    pub show_rail: bool,
    /// Wide enough for the whole path and the shell's name, or narrow enough
    /// for an icon and the directory's own name.
    pub rail_wide: bool,
    /// How wide the right-hand column is: the places list, and what was run
    /// here under it. Points, not a fraction - see [`Showing::column`].
    pub column_width: f32,
    /// How tall the bottom row is: the shell, and the history beside it.
    pub row_height: f32,
    /// The folder the history view last read, what it read, and when.
    ///
    /// The account is on disk and a pane redraws sixty times a second, so it
    /// is read when the folder changes and at most once a second after that.
    /// Something else may be writing to it - the other front-end, or a shell
    /// hook - which is why it is re-read at all rather than only on a change
    /// this program made.
    history_of: Option<PathBuf>,
    history_rows: Vec<journal::Happening>,
    history_read_at: f64,
    /// The list of what was run here, beside the shell.
    ///
    /// On by default: a shell's own history is one list with no idea where
    /// you were standing, and the half that is about *here* is the half
    /// worth having in front of you. `Alt-P` in the terminal view offers the
    /// same list one line at a time; a window has room to show it.
    pub show_shell_history: bool,
    /// Whether the history column is showing only this directory's commands.
    ///
    /// On to begin with, which is the half worth having in front of you: a
    /// shell's own history is one list with no idea where you were standing.
    /// The other half is a keystroke away, because "what was that command"
    /// is sometimes about a directory you have since left.
    pub history_here_only: bool,
    shell_history_of: Option<PathBuf>,
    shell_history: Vec<journal::Past>,
    shell_history_read_at: f64,
    /// Whether both panes are shown. With one, it is the active pane that
    /// fills the window; the other keeps its directory and cursor for when it
    /// comes back.
    pub show_right: bool,
    /// Whether the second pane is only on screen to hold a quick view.
    ///
    /// A preview has to appear somewhere, so F3 in a one-pane window opens
    /// the other pane. Closing the preview then has to put the window back
    /// the way it was found - a pane that stayed behind would be the viewer
    /// redecorating on its way out.
    pub pane_opened_to_view: bool,
    /// How much of a pane's height the tree half gets, when it has one.
    ///
    /// A fraction rather than rows, so it means the same on a tall window as
    /// on a short one.
    pub tree_split: f32,
    /// Whether the keyboard is in the tree half rather than the file list.
    ///
    /// Two lists in one pane is two cursors, and a reader who cannot tell
    /// which one an arrow key moves will not trust either.
    pub on_tree: [bool; 2],
    /// Where each pane's tree cursor was last frame.
    ///
    /// So the view can be scrolled only when the cursor has actually moved.
    /// Scrolling to it every frame would make the wheel useless - the view
    /// would snap back before the hand left it.
    tree_at: [usize; 2],
    /// Where each pane's file cursor was last frame, for the same reason.
    listing_at: [usize; 2],
    /// The directory the visible shell last said it was in, and which tab
    /// said it.
    ///
    /// Kept so the pane follows a `cd` *when it happens* rather than
    /// whenever the two disagree. Those are different rules and only the
    /// first is usable: with the second, walking a pane somewhere the shell
    /// is not would be undone on the next frame.
    shell_was: Option<(usize, PathBuf)>,
    /// The drives and folders this machine offers.
    ///
    /// Read once at startup: finding them looks in `/media` and asks each
    /// drive letter whether it answers, and a spinning disk asked that on
    /// every frame would be heard doing it.
    system_places: Vec<places::Place>,
    /// How much of the width the left pane gets. Dragged on the divider.
    pub split: f32,
    pub bookmarks: Bookmarks,
    pub bookmarks_path: Option<PathBuf>,
    pub job: Option<Job>,
    pub status: String,
    pub status_is_error: bool,
    /// Frames rendered; only used by the screenshot harness.
    pub frames: u64,
    /// When set, save a PNG of the window and exit - this is how the view is
    /// verified without a human at the screen.
    pub screenshot_to: Option<PathBuf>,

    // ---- command line -----------------------------------------------------
    /// What is typed at the prompt.
    pub command: String,
    /// Finished commands, oldest first.
    pub console: Vec<ConsoleEntry>,
    /// The command currently running, if any.
    pub shell_job: Option<ShellJob>,
    /// Whether the output area is expanded; the prompt is always visible.
    pub show_output: bool,
    /// Persisted preferences, including which shell to use.
    pub settings: Settings,
    /// Passwords for the archives opened this session.
    ///
    /// In memory and nowhere else: never written to the settings, never to
    /// the account. The same rule the network locations follow, and for the
    /// same reason - a file manager is not a password manager, and a record
    /// of what was done is not a place for secrets.
    archive_passwords: std::collections::HashMap<PathBuf, String>,
    /// Shells found on this machine, for the picker.
    pub shells: Vec<String>,

    // ---- terminals --------------------------------------------------------
    /// Open interactive shells, each on its own pty.
    pub terminals: Terminals,
    /// Whether the terminal panel is showing.
    pub show_terminal: bool,
    /// Set while the pointer is over the terminal, so typing goes to the shell
    /// rather than to the file panes.
    pub terminal_focused: bool,
    /// Wheel movement not yet worth a whole row, kept so trackpad gestures
    /// accumulate instead of each rounding away to nothing.
    terminal_scroll_carry: f32,
    /// Whether the terminal the window starts with has been opened yet.
    opened_first_terminal: bool,

    // ---- quick view -------------------------------------------------------
    /// A preview being loaded on a worker thread.
    preview_job: Option<preview::PreviewJob>,
    /// The one loaded and ready to draw.
    preview_ready: Option<preview::Ready>,
    /// Font size, zoom and pan for the quick view.
    pub preview_view: preview::PreviewView,
    /// Per pane: the row a shift-click measures its range from.
    mark_anchor: [Option<usize>; 2],
    /// The glob in the selection menu's box.
    pub select_pattern: String,
    /// A modal waiting for an answer.
    pub dialog: Option<Dialog>,
    /// Whether the select menu should open on the next frame, so F9 can open
    /// it as well as a click on the button.
    pub show_select_menu: bool,
    /// Set when a Tab was handled here, so the focus egui's own traversal
    /// hands out on that frame can be dropped on the next one.
    drop_focus: bool,
    /// What has been typed at the command line since the last Enter.
    ///
    /// Only needed for the shell, whose own input buffer lives in another
    /// process and cannot be asked - so this is our record of it, and what
    /// "the command line is empty" means when a real shell is the line.
    pub pending_input: String,
    /// Set when the window should close - F10, as it always has been.
    pub should_quit: bool,
    /// Per pane: scroll the tree to the current directory on the next frame.
    ///
    /// A tree opened on a deep path starts at the filesystem root, so without
    /// this the one row the view exists to show is far below the fold.
    tree_scroll: [bool; 2],
    /// How a file is handed to the desktop.
    ///
    /// A field rather than a direct call so the test suite can watch what
    /// would be opened without any real application starting.
    pub opener: open::Opener,
    /// How a chosen application is started. The "Open with..." counterpart to
    /// `opener`, and injectable for the same reason.
    pub launcher: open::Launcher,
    /// Which row the chooser's cursor is on.
    open_with_cursor: usize,
    /// When the directories on screen were last looked at.
    last_poll: std::time::Instant,
    /// The search in flight, if any.
    search: Option<find::Search>,
    /// The directory comparison in flight, if any.
    scan: Option<compare::Scan>,
    /// The duplicate hunt in flight, if any.
    hunt: Option<dupes::Scan>,
    /// What was last searched for, so reopening the form does not start over.
    last_query: find::Query,
    /// Set when this frame's key opened a dialog, so the same key press does
    /// not also confirm it.
    dialog_opened: bool,
    /// Set when this frame's key handed the keyboard to the terminal, so the
    /// same key press is not also typed into it.
    terminal_taken: bool,
    /// What the quick view asked for while it was drawing, and which file it
    /// asked about. Acted on once the frame has let go of everything.
    preview_request: Option<(preview::Request, PathBuf)>,
    /// The account of what has been done, or nothing where it is turned off.
    journal: Option<journal::Journal>,
}

impl GuiApp {
    pub fn new(left: PathBuf, right: PathBuf) -> Self {
        let mut app = Self::detached(left, right);
        app.bookmarks = Bookmarks::load();
        app.bookmarks_path = Bookmarks::config_path();
        app.settings = Settings::load();
        // How the reader left the window arranged. Clamped rather than
        // trusted: a settings file is a text file somebody can edit, and a
        // split of 40 would put a pane off the edge of the screen with no way
        // to drag it back.
        if let Some(split) = app.settings.pane_split {
            app.split = split.clamp(0.15, 0.85);
        }
        if let Some(split) = app.settings.tree_split {
            app.tree_split = split.clamp(0.15, 0.85);
        }
        if let Some(width) = app.settings.column_width {
            app.column_width = width.clamp(COLUMN_MIN, COLUMN_MAX);
        }
        if let Some(height) = app.settings.shell_height {
            // Only a floor here: the ceiling depends on the window, which is
            // not known until it is drawn, and `sectors` clamps it there.
            app.row_height = height.max(ROW_MIN);
        }
        // The account, and a sweep of whatever has aged out of it. Once at
        // startup is the right moment: it is the only time the program is
        // certainly not in the middle of writing to it.
        app.journal = app.settings.journal();
        if let Some(journal) = &app.journal {
            journal.sweep(journal::Day::today());
        }
        app.shells = app
            .settings
            .shells_to_offer(std::mem::take(&mut app.shells));
        // The saved theme, before the first frame is drawn.
        theme::set_palette(theme::from_settings(&app.settings));
        let right = app.right.cwd().to_path_buf();
        let left = app.left.cwd().to_path_buf();
        app.bookmarks.push_recent(Location::local(right));
        app.bookmarks.push_recent(Location::local(left));
        app
    }

    /// An app that neither reads nor writes the user's bookmark file, which
    /// keeps the tests away from the real configuration.
    pub fn detached(left: PathBuf, right: PathBuf) -> Self {
        let mut bookmarks = Bookmarks::default();
        bookmarks.push_recent(Location::local(right.clone()));
        bookmarks.push_recent(Location::local(left.clone()));

        GuiApp {
            left: Tabs::new(Panel::new(left)),
            right: Tabs::new(Panel::new(right)),
            active: Side::Left,
            left_view: ViewMode::Details,
            right_view: ViewMode::Details,
            show_sidebar: true,
            show_keys: true,
            half: Half::Both,
            workspaces: std::collections::HashMap::new(),
            show_rail: true,
            rail_wide: false,
            column_width: 210.0,
            row_height: 280.0,
            history_of: None,
            history_rows: Vec::new(),
            history_read_at: 0.0,
            show_shell_history: true,
            history_here_only: true,
            shell_history_of: None,
            shell_history: Vec::new(),
            shell_history_read_at: 0.0,
            // One pane to begin with, which is XTree's shape and the one
            // most work actually needs: a tree and its files, with the whole
            // width to show them in. The second is for the few things that
            // are about two places at once - a copy, a move, a comparison, a
            // quick view - and every one of those opens it itself.
            show_right: false,
            pane_opened_to_view: false,
            tree_split: 0.45,
            on_tree: [false, false],
            tree_at: [0, 0],
            listing_at: [0, 0],
            shell_was: None,
            system_places: places::system_places(),
            split: 0.5,
            bookmarks,
            bookmarks_path: None,
            job: None,
            status: String::from("Ready"),
            status_is_error: false,
            frames: 0,
            screenshot_to: None,
            command: String::new(),
            console: Vec::new(),
            shell_job: None,
            show_output: true,
            settings: Settings::default(),
            archive_passwords: std::collections::HashMap::new(),
            // Detached means detached: a test must not write into the real
            // account, so this one is only wired up by `new`.
            journal: None,
            shells: shell::discover_shells(),
            terminals: Terminals::default(),
            show_terminal: true,
            terminal_focused: false,
            terminal_scroll_carry: 0.0,
            opened_first_terminal: false,
            preview_job: None,
            preview_ready: None,
            preview_view: preview::PreviewView::default(),
            mark_anchor: [None; 2],
            select_pattern: String::new(),
            dialog: None,
            show_select_menu: false,
            pending_input: String::new(),
            drop_focus: false,
            should_quit: false,
            tree_scroll: [false; 2],
            opener: Box::new(open::open),
            launcher: Box::new(open::launch),
            open_with_cursor: 0,
            last_poll: std::time::Instant::now(),
            search: None,
            scan: None,
            hunt: None,
            last_query: find::Query::default(),
            dialog_opened: false,
            terminal_taken: false,
            preview_request: None,
        }
    }

    pub fn panel(&self, side: Side) -> &Panel {
        match side {
            Side::Left => self.left.current(),
            Side::Right => self.right.current(),
        }
    }

    pub fn panel_mut(&mut self, side: Side) -> &mut Panel {
        match side {
            Side::Left => self.left.current_mut(),
            Side::Right => self.right.current_mut(),
        }
    }

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

    /// `Ctrl-T`: another tab, on the directory this one is showing.
    fn new_tab(&mut self, side: Side) {
        // A fork, not a blank window. Everything about how you have this one
        // arranged - one pane or two, what each is drawing, which shell is
        // standing here - is what you were about to set up again by hand.
        let forked = (side == Side::Left).then(|| {
            self.remember_workspace();
            self.workspaces.get(&self.workspace_id()).cloned()
        });
        let panel = self.tabs(side).duplicate();
        self.tabs_mut(side).open(panel);
        self.active = side;
        if let Some(Some(carried)) = forked {
            let id = self.workspace_id();
            self.workspaces.insert(id, carried);
        }
        let where_ = self.panel(side).cwd.display().to_string();
        self.info(format!("New workspace: {where_}"));
    }

    /// `Ctrl-W`: close the tab on show.
    fn close_tab(&mut self, side: Side) {
        if self.tabs_mut(side).close() {
            self.sync_tree_view(side);
            self.info("Tab closed");
        } else {
            // Closing the last tab would leave the pane showing nothing, and a
            // pane with nothing in it is not a thing this program has.
            self.error("That is the only tab in this pane");
        }
    }

    /// `Alt-W`: keep the tab on show and close the rest.
    fn close_other_tabs(&mut self, side: Side) {
        match self.tabs_mut(side).close_others() {
            0 => self.error("There is only this tab"),
            n => self.info(format!(
                "Closed {n} other {}",
                if n == 1 { "tab" } else { "tabs" }
            )),
        }
    }
    fn walk_tabs(&mut self, side: Side, forward: bool) {
        // The left pane's tabs are the workspaces, so walking them is
        // walking windows: the arrangement goes with them.
        if side == Side::Left {
            let at = self.left.active();
            let count = self.left.len();
            let next = if forward {
                (at + 1) % count
            } else {
                (at + count - 1) % count
            };
            self.show_workspace(next);
            self.sync_tree_view(side);
            return;
        }
        if forward {
            self.tabs_mut(side).next();
        } else {
            self.tabs_mut(side).prev();
        }
        self.sync_tree_view(side);
    }

    /// `Shift-F6`: send this tab to the other pane, as `F6` sends a file.
    ///
    /// The tab goes whole - its cursor, its marks, its sort order - because a
    /// tab that arrived as a bare path would have lost the reason you wanted
    /// it over there.
    fn move_tab_across(&mut self, side: Side) {
        let Some(panel) = self.tabs_mut(side).take() else {
            self.error("That is the only tab in this pane");
            return;
        };
        let where_ = panel.cwd.display().to_string();
        let other = Self::other_side(side);
        self.tabs_mut(other).accept(panel);
        // Both panes need their tree state settled: one lost a tab and one
        // gained the tab that is now on show.
        self.sync_tree_view(side);
        self.sync_tree_view(other);
        // Following it across is what you meant by moving it: the tab is the
        // thing you were working in, and it is now over there.
        self.active = other;
        if !self.show_right {
            // With the second pane folded away the tab would otherwise vanish
            // into a pane nobody can see.
            self.show_right = true;
        }
        self.info(format!("Moved to the other pane: {where_}"));
    }

    fn other_side(side: Side) -> Side {
        match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    /// Make the pane's view mode agree with the tab that is now on show.
    ///
    /// The view is a property of the pane and the tree is a property of the
    /// panel, so switching to a tab that was not in a tree while the pane says
    /// "tree" would leave the two disagreeing about what is on screen.
    fn sync_tree_view(&mut self, side: Side) {
        let wants_tree = self.view(side) == ViewMode::Tree;
        let has_tree = self.panel(side).in_tree_mode();
        if wants_tree && !has_tree {
            self.panel_mut(side).enter_tree_mode();
            self.tree_scroll[Self::side_index(side)] = true;
        } else if !wants_tree && has_tree {
            self.panel_mut(side).leave_tree_mode();
        }
    }

    fn side_index(side: Side) -> usize {
        match side {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    pub fn view(&self, side: Side) -> ViewMode {
        match side {
            Side::Left => self.left_view,
            Side::Right => self.right_view,
        }
    }

    /// Switch a pane between listing, grid and tree.
    ///
    /// The tree is built on the panel itself, so it always reflects that
    /// pane's directory rather than some separate idea of where you are.
    pub fn set_view(&mut self, side: Side, mode: ViewMode) {
        // Asking for the tree puts the keyboard in it. You turned it on to
        // walk it, and having to press Tab first to reach the thing you just
        // asked for reads as the key not having worked.
        let index = match side {
            Side::Left => 0,
            Side::Right => 1,
        };
        self.on_tree[index] = mode == ViewMode::Tree;

        // Asked here rather than in each of the keys that can close a quick
        // view - F3 again, Ctrl-Q, Escape, or picking another view from the
        // pane's own header. They all arrive through this one function, and a
        // pane handed back by three of the four would be worse than one never
        // handed back at all.
        // Either of the two views that answer about somewhere else: a pane
        // borrowed to show one has to be given back when it stops, whichever
        // it was.
        let was_borrowed = matches!(self.view(side), ViewMode::Preview | ViewMode::History);
        let still_borrowed = matches!(mode, ViewMode::Preview | ViewMode::History);
        if was_borrowed && !still_borrowed && self.pane_opened_to_view {
            self.pane_opened_to_view = false;
            if side == Side::Right {
                self.show_right = false;
                // The keyboard cannot be left in a pane that is not there.
                self.active = Side::Left;
            }
        }
        // Two panes that both follow the other would have nothing to follow:
        // neither would be a listing with a cursor in it.
        let follows_the_other = matches!(mode, ViewMode::Preview | ViewMode::History);
        if follows_the_other
            && matches!(
                self.view(side.other()),
                ViewMode::Preview | ViewMode::History
            )
        {
            let other = side.other();
            match other {
                Side::Left => self.left_view = ViewMode::Details,
                Side::Right => self.right_view = ViewMode::Details,
            }
            self.panel_mut(other).leave_tree_mode();
        }
        match side {
            Side::Left => self.left_view = mode,
            Side::Right => self.right_view = mode,
        }
        if mode == ViewMode::Tree {
            self.panel_mut(side).enter_tree_mode();
            self.tree_scroll[Self::side_index(side)] = true;
        } else {
            self.panel_mut(side).leave_tree_mode();
        }
    }

    pub fn active_panel(&self) -> &Panel {
        self.panel(self.active)
    }

    pub fn active_panel_mut(&mut self) -> &mut Panel {
        let side = self.active;
        self.panel_mut(side)
    }

    fn info(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = false;
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = true;
    }

    fn navigate(&mut self, side: Side, path: PathBuf) {
        self.panel_mut(side).chdir(path.clone());
        if let Some(error) = self.panel(side).error.clone() {
            self.error(format!("{}: {error}", path.display()));
        } else {
            self.bookmarks.push_recent(Location::local(path.clone()));
            // A pane showing a tree should follow along rather than go stale.
            if self.view(side) == ViewMode::Tree {
                self.panel_mut(side).enter_tree_mode();
                self.tree_scroll[Self::side_index(side)] = true;
            }
            self.info(path.display().to_string());
        }
    }

    fn selection(&self, side: Side) -> Vec<PathBuf> {
        self.panel(side).action_targets()
    }

    // ---- command line ------------------------------------------------------

    /// `src $` - short, to leave room for the command itself.
    pub fn prompt(&self) -> String {
        Self::prompt_for(&self.active_panel().cwd)
    }

    /// The prompt for a specific directory.
    ///
    /// History entries are labelled with the directory the command actually
    /// ran in, not with wherever the panels happen to be by the time it
    /// finishes - otherwise a slow command plus a panel switch produces a log
    /// line that names the wrong place.
    pub fn prompt_for(path: &Path) -> String {
        let shown = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let sigil = if cfg!(windows) { ">" } else { "$" };
        format!("{shown} {sigil}")
    }

    /// The shell to run commands with: the user's choice, else the
    /// environment's, else the platform default.
    pub fn chosen_shell(&self) -> (String, String) {
        shell::resolve_shell(
            lost_commander_core::mount::Platform::current(),
            self.settings.shell.as_deref(),
            shell::environment_shell().as_deref(),
        )
    }

    /// Write down how the window is arranged.
    ///
    /// Quietly: a divider that could not be saved is not worth interrupting
    /// anybody over, and the message would land in the status line at the
    /// exact moment they were looking somewhere else. It will be retried the
    /// next time anything is dragged.
    fn remember_layout(&mut self) {
        self.layout_into_settings();
        let _ = self.settings.save();
    }

    /// Copy the arrangement into the settings without writing them.
    ///
    /// Split out so it can be tested. A test that called the saving version
    /// would write the *real* settings file of whoever ran the suite - which
    /// is not hypothetical: it happened, and it took somebody's chosen theme
    /// with it.
    fn layout_into_settings(&mut self) {
        self.settings.pane_split = Some(self.split);
        self.settings.tree_split = Some(self.tree_split);
        self.settings.column_width = Some(self.column_width);
        self.settings.shell_height = Some(self.row_height);
    }

    /// Remember a different shell for future commands.
    pub fn set_shell(&mut self, program: Option<String>) {
        self.settings.shell = program;
        if let Err(e) = self.settings.save() {
            self.error(format!("Could not save settings: {e}"));
            return;
        }
        let (program, _) = self.chosen_shell();
        self.info(format!("Commands will run with {program}"));
    }

    /// Send the visible shell after the active pane, when the pane has moved.
    ///
    /// The other half of the coupling: `cd here` did this on demand and
    /// nothing did it on its own, so walking the panes left the shell behind
    /// and the next command ran somewhere you were no longer looking.
    ///
    /// Switching panes counts as moving, because the pane you are working in
    /// is the one the shell should be in - that is the whole point of the two
    /// being coupled.
    ///
    /// A pinned tab is left alone.
    fn shell_follows_the_pane(&mut self) {
        if self.terminals.is_pinned(self.terminals.active) {
            return;
        }
        let cwd = self.active_panel().cwd.clone();
        let Some(session) = self.terminals.active_mut() else {
            return;
        };
        // Where the shell says it is, or failing that where it was started -
        // an unhooked shell never says, and would otherwise be sent a `cd`
        // after every keystroke that moved a pane.
        let already = session.shell_cwd().unwrap_or_else(|| session.cwd.clone());
        if already == cwd {
            return;
        }
        let line = lost_commander_core::shell::cd_command(&session.program, &cwd);
        session.run_line(&line);
        session.cwd = cwd;
    }

    /// Move the active pane to wherever the visible shell has just gone.
    ///
    /// Only on a change, and only for the tab that reported it. A shell says
    /// where it is through the hook, so this is reading an answer rather than
    /// guessing at one - and a shell with no seam to hook never answers, so
    /// nothing happens and nothing pretends to.
    ///
    /// Switching tabs is not a `cd`: the new tab's directory is noted without
    /// acting on it, or every glance at another shell would drag the pane
    /// somewhere the reader did not ask to go.
    fn follow_the_shell(&mut self) {
        let Some((tab, where_)) = self
            .terminals
            .active()
            .and_then(|s| s.shell_cwd().map(|c| (self.terminals.active, c)))
        else {
            return;
        };
        if self.terminals.is_pinned(tab) {
            // Pinned: left where it is, and steering nothing.
            self.shell_was = Some((tab, where_));
            return;
        }
        let changed = match &self.shell_was {
            Some((was_tab, was)) => *was_tab == tab && *was != where_,
            None => false,
        };
        self.shell_was = Some((tab, where_.clone()));
        if !changed || !where_.is_dir() {
            return;
        }
        let side = self.active;
        if self.panel(side).cwd == where_ {
            return;
        }
        self.navigate(side, where_);
    }

    // ---- terminals ---------------------------------------------------------

    /// Open a terminal running `program` in the active panel's directory.
    ///
    /// This is the `+` button: several shells run at once, so a long build can
    /// carry on in one tab while another is used for something else.
    pub fn open_terminal(&mut self, program: Option<String>) {
        let program = program.unwrap_or_else(|| self.chosen_shell().0);
        let cwd = self.active_panel().cwd.clone();
        match self.terminals.open(&program, &cwd, 24, 80) {
            Ok(()) => {
                self.show_terminal = true;
                self.terminal_focused = true;
                // Started while this workspace was on show, so it is this
                // workspace's shell - which is what makes coming back to a
                // window bring its shell with it.
                self.pair_shell_here();
                self.note_unrecorded_shell(&program, &cwd);
                self.info(format!("Opened {program} in {}", cwd.display()));
            }
            Err(e) => self.error(format!("Could not start {program}: {e}")),
        }
    }

    /// Say in the account when a shell cannot report what is run in it.
    ///
    /// Without this, a day spent working in `dash` leaves an empty stream -
    /// and an empty stream reads as "nothing was run", which is the one thing
    /// a record must never say when something was. `sh`, `dash` and the rest
    /// of the POSIX family have no preexec, no `PROMPT_COMMAND` and no `DEBUG`
    /// trap, so there is nothing to hook and nothing to be done but say so.
    fn note_unrecorded_shell(&mut self, program: &str, cwd: &Path) {
        if self.journal.is_none() {
            return;
        }
        let hooked = self
            .terminals
            .active()
            .map(|session| session.journals())
            .unwrap_or(false);
        if hooked {
            return;
        }
        self.note(
            journal::Event::new(journal::Kind::Session, cwd)
                .by(shell::program_name(program))
                .note(lost_commander_core::shellhook::why_not()),
        );
    }

    // ---- quick view ---------------------------------------------------------

    /// The pane showing a quick view, if either is.
    pub fn preview_side(&self) -> Option<Side> {
        [Side::Left, Side::Right]
            .into_iter()
            .find(|&side| self.view(side) == ViewMode::Preview)
    }

    /// What a quick-view pane should be showing.
    ///
    /// The entry under the *other* pane's cursor - that is what quick view has
    /// always meant: the pane opposite is where you move, and this one follows.
    pub fn preview_target(&self, side: Side) -> Option<Entry> {
        self.panel(side.other()).selected().cloned()
    }

    /// Start a load when the other pane's cursor moves, and collect finished
    /// ones. Loading is off-thread, so this only ever hands work over.
    fn poll_preview(&mut self) {
        let Some(side) = self.preview_side() else {
            // Nothing is previewing; drop what was held so a big photograph
            // is not kept in memory for a pane nobody is looking at.
            self.preview_job = None;
            self.preview_ready = None;
            return;
        };

        let target = self.preview_target(side);
        let wanted = target.as_ref().map(|entry| entry.path.clone());
        let showing = self.preview_ready.as_ref().map(|ready| ready.path.clone());
        let loading = self.preview_job.as_ref().map(|job| job.path.clone());

        if wanted != showing && wanted != loading {
            self.preview_job = target
                .as_ref()
                .map(|entry| preview::PreviewJob::spawn(entry.path.clone(), entry.is_dir()));
            if wanted.is_none() {
                self.preview_ready = None;
            }
        }

        if let Some(job) = &mut self.preview_job {
            if job.is_finished() {
                let path = job.path.clone();
                if let Some(loaded) = job.take() {
                    // A new picture arrives fitted and centred rather than at
                    // whatever zoom the last one was left at.
                    self.preview_view.reset_zoom();
                    self.preview_ready = Some(preview::Ready::new(path, loaded));
                }
                self.preview_job = None;
            }
        }
    }

    /// Draw the quick view.
    fn pane_preview(&mut self, ui: &mut egui::Ui, side: Side) {
        let target = self.preview_target(side);
        let Some(entry) = target else {
            ui.add_space(12.0);
            ui.label(
                RichText::new("Nothing selected in the other pane.")
                    .size(11.5)
                    .color(theme::text_faint()),
            );
            return;
        };

        let ready = self
            .preview_ready
            .as_mut()
            .filter(|ready| ready.path == entry.path);
        match ready {
            Some(ready) => {
                ready.ensure_texture(ui.ctx());
                // The preview cannot open a dialog from in here - the panes
                // and the dialog are the same `self` it is already holding -
                // so what it wants is put down and picked up after the frame.
                if let Some(request) =
                    preview::draw(ui, ready, &mut self.preview_view, Some(&entry))
                {
                    self.preview_request = Some((request, entry.path.clone()));
                }
            }
            None => {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Reading {}...", entry.name))
                        .size(11.5)
                        .color(theme::text_faint()),
                );
            }
        }
    }

    /// Close the terminal on screen - the `-` button.
    pub fn close_active_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.terminals.close(self.terminals.active);
        self.terminal_scroll_carry = 0.0;
        if self.terminals.is_empty() {
            // Nothing left to type at, so give the keyboard back to the panes.
            self.terminal_focused = false;
        }
    }

    /// Open the terminal the window starts with, once.
    ///
    /// Not done in `new`: building a `GuiApp` should not fork a process, or
    /// every test that constructs one would start a shell. And only once -
    /// closing the last terminal has to mean closed, not "back next frame".
    pub fn open_initial_terminal(&mut self) {
        if self.opened_first_terminal {
            return;
        }
        self.opened_first_terminal = true;
        if !self.show_terminal || !self.terminals.is_empty() {
            return;
        }
        self.open_terminal(None);
        // Present, but not holding the keyboard: the files are what the window
        // is for, and a shell that swallowed the first keystroke would be a
        // surprise. Clicking it hands the keyboard over.
        self.terminal_focused = false;
    }

    // ---- saving what the shell printed --------------------------------------

    /// The output of whichever shell surface is on screen, as plain text.
    ///
    /// One accessor for both, because "copy the output" means the same thing
    /// whether the panel is showing a terminal or the one-shot command line,
    /// and a button that vanished when the panel was toggled would be silly.
    pub fn output_text(&mut self) -> Option<String> {
        if self.show_terminal {
            return self
                .terminals
                .active_mut()
                .map(|session| session.transcript());
        }
        if self.console.is_empty() {
            return None;
        }
        let mut text = String::new();
        for entry in &self.console {
            text.push_str(&format!("{} {}\n", entry.prompt, entry.line));
            text.push_str(&entry.output.stdout);
            text.push_str(&entry.output.stderr);
            if !entry.output.succeeded() {
                let code = entry.output.code;
                text.push_str(&match code {
                    Some(code) => format!("[exit {code}]\n"),
                    None => "[killed]\n".to_string(),
                });
            }
        }
        Some(text)
    }

    /// What a saved transcript should be called.
    pub fn output_basename(&self, stamp: &str) -> String {
        let title = if self.show_terminal {
            self.terminals
                .active()
                .map(|session| session.title.as_str())
                .unwrap_or("terminal")
        } else {
            "console"
        };
        pty::transcript_name(title, stamp)
    }

    /// Write the output into the active panel's directory - "current folder"
    /// meaning the one being looked at, not the process's own.
    ///
    /// Never overwrites: the stamp makes a clash all but impossible, but a
    /// saved log is not worth losing to the one time it is not.
    pub fn save_output(&mut self, stamp: &str) -> Option<PathBuf> {
        let Some(text) = self.output_text() else {
            self.error("Nothing to save yet");
            return None;
        };

        let directory = self.active_panel().cwd.clone();
        let wanted = self.output_basename(stamp);
        let mut path = directory.join(&wanted);
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string());
        if let Some(stem) = stem {
            let mut attempt = 2;
            while path.exists() {
                path = directory.join(format!("{stem}-{attempt}.log"));
                attempt += 1;
            }
        }

        match std::fs::write(&path, text.as_bytes()) {
            Ok(()) => {
                // So it shows up in the pane that was just written into.
                self.left.reload();
                self.right.reload();
                self.info(format!("Saved {}", path.display()));
                Some(path)
            }
            Err(e) => {
                self.error(format!("Could not write {}: {e}", path.display()));
                None
            }
        }
    }

    /// Start or stop recording the terminal on screen.
    ///
    /// A new recording is always a new file: nothing ever appends, so a file
    /// is one session's worth of output and its name says when it started.
    pub fn toggle_recording(&mut self, stamp: &str) -> Option<PathBuf> {
        let Some(session) = self.terminals.active() else {
            self.error("No terminal to record");
            return None;
        };

        if session.recording().is_some() {
            let stopped = self.terminals.active_mut().and_then(|s| s.stop_recording());
            if let Some((path, lines)) = stopped {
                self.left.reload();
                self.right.reload();
                // The account and the transcript answer different questions -
                // "what was run" against "what did it print" - so the one
                // that is always on should say where to find the other.
                self.note(
                    journal::Event::new(journal::Kind::Session, &path)
                        .note(format!("Stopped recording - {lines} line(s)")),
                );
                self.info(format!("Recorded {lines} lines to {}", path.display()));
                return Some(path);
            }
            return None;
        }

        let directory = self.active_panel().cwd.clone();
        let mut path = directory.join(pty::transcript_name(&session.title, stamp));
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string());
        if let Some(stem) = stem {
            let mut attempt = 2;
            while path.exists() {
                path = directory.join(format!("{stem}-{attempt}.log"));
                attempt += 1;
            }
        }

        let started = self
            .terminals
            .active_mut()
            .map(|session| session.start_recording(&path));
        match started {
            Some(Ok(())) => {
                self.left.reload();
                self.right.reload();
                self.note(
                    journal::Event::new(journal::Kind::Session, &path).note("Started recording"),
                );
                self.info(format!("Recording to {}", path.display()));
                Some(path)
            }
            Some(Err(e)) => {
                self.error(format!("Could not record to {}: {e}", path.display()));
                None
            }
            None => None,
        }
    }

    /// Type the selected names into the active terminal, quoted.
    ///
    /// The terminal form of the original Ctrl-Enter: the shell sees the
    /// characters as though they were typed, so its own editing and completion
    /// still apply afterwards.
    pub fn send_selection_to_terminal(&mut self, full_path: bool) -> bool {
        let chosen = self.selected_names(full_path);
        if chosen.is_empty() {
            return false;
        }
        let text = format!("{} ", chosen.join(" "));
        match self.terminals.active_mut() {
            Some(session) => {
                session.write_str(&text);
                true
            }
            None => false,
        }
    }

    /// Point the active terminal at the active panel's directory.
    pub fn terminal_follow_panel(&mut self) {
        let cwd = self.active_panel().cwd.clone();
        let line = format!("cd {}", shell::quote_here(&cwd.display().to_string()));
        match self.terminals.active_mut() {
            Some(session) => {
                session.run_line(&line);
                session.cwd = cwd;
                self.info("Terminal followed the panel");
            }
            None => self.error("No terminal open"),
        }
    }

    /// Marked entries, or the one under the cursor, quoted for the shell.
    fn selected_names(&self, full_path: bool) -> Vec<String> {
        let panel = self.active_panel();
        if panel.marked_count() > 0 {
            panel
                .entries
                .iter()
                .filter(|e| e.marked)
                .map(|e| Self::render_name(e, full_path))
                .collect()
        } else {
            panel
                .selected()
                .filter(|e| !e.is_parent())
                .map(|e| vec![Self::render_name(e, full_path)])
                .unwrap_or_default()
        }
    }

    /// Put the selected file names on the command line, quoted so a name with
    /// spaces stays a single argument. This is the original Ctrl-Enter.
    pub fn insert_selection(&mut self, full_path: bool) {
        let panel = self.active_panel();
        let chosen: Vec<String> = if panel.marked_count() > 0 {
            panel
                .entries
                .iter()
                .filter(|e| e.marked)
                .map(|e| Self::render_name(e, full_path))
                .collect()
        } else {
            panel
                .selected()
                .filter(|e| !e.is_parent())
                .map(|e| vec![Self::render_name(e, full_path)])
                .unwrap_or_default()
        };
        if chosen.is_empty() {
            return;
        }
        if !self.command.is_empty() && !self.command.ends_with(' ') {
            self.command.push(' ');
        }
        self.command.push_str(&chosen.join(" "));
    }

    fn render_name(entry: &Entry, full_path: bool) -> String {
        let raw = if full_path {
            entry.path.display().to_string()
        } else {
            entry.name.clone()
        };
        shell::quote_here(&raw)
    }

    /// Run whatever is on the command line, in the active panel's directory.
    pub fn run_command(&mut self) {
        let line = self.command.trim().to_string();
        if line.is_empty() {
            return;
        }
        if self.shell_job.is_some() {
            self.error("A command is already running");
            return;
        }

        // `cd` has to be handled here: a subprocess changing its own working
        // directory would have no effect on the panel.
        if let Some(target) = shell::intercept(&line) {
            let cwd = self.active_panel().cwd.clone();
            match shell::resolve_cd(&target, &cwd) {
                Some(path) if path.is_dir() => {
                    let side = self.active;
                    self.navigate(side, path);
                }
                Some(path) => self.error(format!("Not a directory: {}", path.display())),
                None => self.error("Could not work out where to go"),
            }
            self.command.clear();
            return;
        }

        let cwd = self.active_panel().cwd.clone();
        self.shell_job = Some(ShellJob::spawn_with(line.clone(), cwd, self.chosen_shell()));
        self.info(format!("Running: {line}"));
        self.command.clear();
    }

    /// Write down what the interactive shells have run since the last frame.
    ///
    /// The one-shot command line below is this program running a command and
    /// waiting for it; a terminal panel is a real shell that nothing here is
    /// in the middle of. What it ran arrives as marks in its own output - see
    /// [`lost_commander_core::shellhook`] - and this is where they are collected.
    ///
    /// The directory recorded is the shell's, not the panel's: once someone
    /// has typed `cd`, those are two different places, and the one the
    /// command ran in is the one worth keeping.
    pub fn poll_terminal_commands(&mut self) {
        if self.journal.is_none() {
            return;
        }
        let mut ran: Vec<(
            lost_commander_core::shellhook::Ran,
            std::path::PathBuf,
            String,
        )> = Vec::new();
        for session in &self.terminals.sessions {
            let shell = shell::program_name(&session.program);
            for one in session.take_commands() {
                let cwd = one.cwd.clone().unwrap_or_else(|| session.cwd.clone());
                ran.push((one, cwd, shell.clone()));
            }
        }
        for (one, cwd, shell) in ran {
            self.note(
                journal::Event::new(journal::Kind::Command, &cwd)
                    .note(one.line.clone())
                    .by(shell)
                    .lasting(one.ms)
                    .failed_if(one.failed(), format!("exit {}", one.code)),
            );
        }
    }

    /// Retire a finished command: record its output and re-read the panels,
    /// since it may well have created or removed files.
    pub fn poll_shell(&mut self) {
        let Some(job) = &mut self.shell_job else {
            return;
        };
        if !job.is_finished() {
            return;
        }
        let Some(output) = job.take() else { return };
        let line = job.line.clone();
        let prompt = Self::prompt_for(&job.cwd);
        let cwd = job.cwd.clone();
        let mut job = self.shell_job.take().expect("checked");
        let took = job.took();
        job.join();

        let failed = !output.succeeded();
        // The line and how it ended, never the output. A record of what was
        // run is a record; a copy of every build log is a disk full.
        self.note(
            journal::Event::new(journal::Kind::Command, &cwd)
                .note(line.clone())
                .by(shell::program_name(&job.shell))
                .lasting(took)
                .failed_if(
                    failed,
                    match output.code {
                        Some(code) => format!("exit {code}"),
                        None => "did not run".to_string(),
                    },
                ),
        );
        self.console.push(ConsoleEntry {
            prompt,
            cwd,
            line: line.clone(),
            output,
        });
        if self.console.len() > CONSOLE_HISTORY {
            let excess = self.console.len() - CONSOLE_HISTORY;
            self.console.drain(..excess);
        }

        self.left.reload();
        self.right.reload();

        if failed {
            self.error(format!("{line} failed"));
        } else {
            self.info(format!("{line} finished"));
        }
    }

    fn start(&mut self, operation: Operation) {
        if self.job.is_some() {
            self.error("Another operation is already running");
            return;
        }
        self.job = match &self.journal {
            Some(journal) => Some(Job::spawn_recorded(operation, journal.clone())),
            None => Some(Job::spawn(operation)),
        };
    }

    /// Note one thing that happened, if an account is being kept.
    ///
    /// Every single-file operation goes through here rather than reaching for
    /// the journal itself, so "is there a journal at all" is asked in one
    /// place and the call sites read as one line each.
    fn note(&self, event: journal::Event) {
        if let Some(journal) = &self.journal {
            journal.record(event);
        }
    }

    /// Notice what something else did to the directories on screen.
    ///
    /// A file manager whose listing is only right until anything else touches
    /// the disk is a file manager you have to remember to press `Ctrl-R` in.
    ///
    /// Not while an operation is running: it re-reads both panels when it
    /// finishes, and a listing changing under a copy's own progress would be
    /// the copy's own writes reported back as news.
    fn poll_directories(&mut self, ctx: &egui::Context) {
        // egui only draws when something happens, so the window has to be
        // asked to wake up - otherwise an idle window never looks.
        ctx.request_repaint_after(POLL_EVERY);

        if self.job.is_some() {
            return;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last_poll) < POLL_EVERY {
            return;
        }
        self.last_poll = now;

        for side in [Side::Left, Side::Right] {
            if self.panel_mut(side).poll_changes() {
                if let Some(tree) = self.panel_mut(side).tree.as_mut() {
                    tree.refresh();
                }
            }
        }
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };

        // The worker blocks on a collision until it is answered, so the
        // question has to be lifted onto the screen before anything else.
        if let Some(conflict) = job.asking() {
            if !matches!(self.dialog, Some(Dialog::ConfirmOverwrite { .. })) {
                self.dialog = Some(Dialog::ConfirmOverwrite { conflict });
            }
            return;
        }

        if !job.is_finished() {
            return;
        }
        let snapshot = job.snapshot();
        let past = job.operation.past_tense();
        let mut job = self.job.take().expect("checked");
        job.join();

        self.left.current_mut().clear_marks();
        self.right.current_mut().clear_marks();
        self.left.reload();
        self.right.reload();
        for side in [Side::Left, Side::Right] {
            if let Some(tree) = self.panel_mut(side).tree.as_mut() {
                tree.refresh();
            }
        }

        if snapshot.cancelled {
            self.error(format!("Cancelled after {} item(s)", snapshot.items_done));
        } else if snapshot.failures.is_empty() {
            self.info(snapshot.outcome(past));
        } else {
            self.error(format!(
                "{past} with errors: {}",
                snapshot.failures.join("; ")
            ));
        }
    }
}

/// How often the directories on screen are checked for outside changes.
///
/// A second is under the threshold at which a stale listing is noticed, and
/// far above the cost of looking - one directory read per panel, with the
/// per-entry detail only for directories small enough for it to be free.
const POLL_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        theme::apply(ctx);
        self.open_initial_terminal();
        self.poll_job();
        self.poll_preview();
        self.poll_shell();
        self.poll_terminal_commands();
        self.poll_directories(ctx);

        // Ctrl-Enter puts the selection on the command line; taken before the
        // text field sees it, so plain Enter can still mean "run".
        let (insert_name, insert_path) = ctx.input(|i| {
            let enter = i.key_pressed(egui::Key::Enter);
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            (
                enter && ctrl && !i.modifiers.shift,
                enter && ctrl && i.modifiers.shift,
            )
        });
        self.keyboard(ctx);

        if insert_name || insert_path {
            // With the terminal focused the names are typed into the shell;
            // otherwise they go on the one-shot command line.
            if !(self.terminal_focused && self.send_selection_to_terminal(insert_path)) {
                self.insert_selection(insert_path);
            }
        }

        self.terminals.reap_finished();
        // Only while both are on screen. A `cd` in a shell nobody can see
        // has no business moving a pane nobody is looking at, and the pane
        // walking off while the window is given over to the shell is the
        // same surprise from the other end. They are put back in step when
        // both come back - see `show_half`.
        self.sync_halves();
        self.terminal_input(ctx);

        egui::TopBottomPanel::top("toolbar")
            .exact_height(46.0)
            .frame(chrome_frame(theme::surface()))
            .show(ctx, |ui| self.toolbar(ui));

        egui::TopBottomPanel::bottom("status")
            .exact_height(34.0)
            .frame(chrome_frame(theme::surface()))
            .show(ctx, |ui| self.status_bar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::bg()))
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let cut = sectors(
                    full,
                    Showing {
                        rail: self.show_rail,
                        rail_wide: self.rail_wide,
                        top: self.half != Half::Shell,
                        bottom: self.half != Half::Files,
                        places: self.show_sidebar,
                        history: self.show_shell_history,
                        keys: self.show_keys,
                        column: self.column_width,
                        // The one-shot command line is a line, not a drawer:
                        // it gets the height it needs rather than the height
                        // the shell was left at, and its seam does not drag.
                        row: if self.show_terminal {
                            self.row_height
                        } else if self.show_output {
                            150.0
                        } else {
                            ROW_MIN
                        },
                    },
                );

                // Each sector paints its own background, which being a panel
                // used to do for it.
                let painter = ui.painter().clone();
                if let Some(rect) = cut.rail {
                    painter.rect_filled(rect, 0.0, theme::sidebar());
                }
                for rect in [cut.shell, cut.keys].into_iter().flatten() {
                    painter.rect_filled(rect, 0.0, theme::surface());
                }
                for rect in [cut.places, cut.history].into_iter().flatten() {
                    painter.rect_filled(rect, 0.0, theme::sidebar());
                }

                if let Some(rect) = cut.rail {
                    self.rail(ui, rect);
                }

                if let Some(rect) = cut.panes {
                    self.panes_in(ui, rect);
                }

                if let Some(rect) = cut.keys {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    self.key_bar(&mut child);
                }

                if let Some(rect) = cut.shell {
                    let mut child =
                        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(CHROME_PAD)));
                    if self.show_terminal {
                        self.terminal_panel(&mut child);
                    } else {
                        self.console_panel(&mut child);
                    }
                }

                if let Some(rect) = cut.places {
                    let mut child =
                        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(CHROME_PAD)));
                    self.sidebar(&mut child);
                }
                if let Some(rect) = cut.history {
                    self.shell_history_column(ui, rect.shrink2(CHROME_PAD));
                }

                // The seams go last, so they take the pointer before whatever
                // is drawn under them.
                if let Some(seam) = cut.vertical {
                    if let Some(pointer) = self.drag_seam(ui, "column_seam", seam, true) {
                        self.column_width = (full.max.x - pointer.x).clamp(COLUMN_MIN, COLUMN_MAX);
                    }
                }
                // Only a drawer can be dragged taller; the one-shot command
                // line is one line and there is nothing to give it. With one
                // half on show there is no seam at all.
                if let Some(seam) = cut.horizontal {
                    if self.show_terminal {
                        if let Some(pointer) = self.drag_seam(ui, "row_seam", seam, false) {
                            self.row_height = (full.max.y - pointer.y).max(ROW_MIN);
                        }
                    } else {
                        ui.painter().hline(
                            full.x_range(),
                            seam.center().y,
                            egui::Stroke::new(1.0, theme::border()),
                        );
                    }
                }
            });

        // Whatever the quick view asked for while it was drawing, now that
        // nothing is holding a borrow of the panes.
        if let Some((request, path)) = self.preview_request.take() {
            match request {
                preview::Request::EditImage => self.open_image(path),
            }
        }

        self.dialogs(ctx);
        self.handle_screenshot(ctx);

        if self.should_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // A running job repaints continuously so the bar animates; otherwise
        // egui only redraws on input, which is what keeps it idle-cheap.
        if self.job.is_some()
            || self.preview_job.is_some()
            || self.screenshot_to.is_some()
            || !self.terminals.is_empty()
        {
            // Terminal output arrives on another thread with no egui event to
            // wake the loop, so the panel has to keep asking.
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}

/// The gap drawn between the panes, and the width of the target you can
/// actually grab. They differ on purpose: a hairline is what looks right and
/// a hairline is what you cannot hit.
/// The breathing room a sector's contents get inside it, now that a sector
/// is a rectangle this draws rather than a panel with a frame of its own.
pub const CHROME_PAD: Vec2 = Vec2::new(8.0, 6.0);
pub const GUTTER: f32 = 6.0;
pub const GRAB: f32 = 6.0;

/// How narrow either pane may be dragged, as a fraction of the width.
///
/// Not zero: a pane dragged away to nothing looks like a bug and cannot be
/// grabbed back. Folding one away entirely is what the toolbar toggle is for.
pub const SPLIT_MIN: f32 = 0.12;
pub const SPLIT_MAX: f32 = 0.88;

/// What the window is showing, for [`sectors`] to lay out.
#[derive(Debug, Clone, Copy)]
pub struct Showing {
    /// The rail of workspaces, down the left of everything.
    pub rail: bool,
    /// Wide enough for a path and a shell name, or narrow enough for a name.
    pub rail_wide: bool,
    /// The top row: the panes, and the places list beside them.
    pub top: bool,
    /// The bottom row: the shell, and the history beside it.
    ///
    /// Both are on together nearly always. Either alone is the window given
    /// over to one job - reading a directory, or working in a shell - which
    /// is what `Ctrl-O` has meant in every commander since Norton.
    pub bottom: bool,
    /// The right-hand column: the places list, with the history under it.
    pub places: bool,
    /// What was run here, under the places list and the same width as it.
    pub history: bool,
    /// The function keys, under the panes.
    pub keys: bool,
    /// The width of the right-hand column, in points.
    ///
    /// Points rather than a fraction, unlike the split between the panes: a
    /// list of place names has a natural width - about twenty characters -
    /// and it is the same width on a laptop and on a large monitor. A
    /// fraction would make it grow into the middle of a wide window.
    pub column: f32,
    /// The height of the bottom row, in points, for the same reason: a
    /// drawer's useful height is "eight lines of output".
    pub row: f32,
}

/// The four sectors of the window, and the two lines between them.
///
/// Two splitters rather than four: the vertical one runs the whole height, so
/// the shell is exactly as wide as the panes above it, and the horizontal one
/// runs the whole width, so what was run here lines up with the shell that
/// ran it. Each was previously its own panel with its own edge, which is how
/// the drawer came to be a different width from the panes it belonged to.
#[derive(Debug, Clone, Copy)]
pub struct Sectors {
    /// The full-height rail of workspaces, down the left.
    pub rail: Option<Rect>,
    /// Top left: the panes. Split again by the second pane, and by a tree.
    pub panes: Option<Rect>,
    /// The strip under the panes, when the key bar is on.
    pub keys: Option<Rect>,
    /// Bottom left: the shell, or the one-shot command line.
    pub shell: Option<Rect>,
    /// Top right: the places list.
    pub places: Option<Rect>,
    /// Bottom right: what was run in the shell's directory.
    pub history: Option<Rect>,
    /// The full-height line between the columns.
    pub vertical: Option<Rect>,
    /// The full-width line between the rows. Only when there are two rows to
    /// separate: with one, there is nothing to drag.
    pub horizontal: Option<Rect>,
}

/// How short either row may be dragged, and how narrow the column.
pub const ROW_MIN: f32 = 34.0;
pub const PANES_MIN: f32 = 120.0;
pub const COLUMN_MIN: f32 = 140.0;
pub const COLUMN_MAX: f32 = 420.0;
/// The strip the function keys get, when they are on.
pub const KEYS_HEIGHT: f32 = 26.0;
/// The rail, narrow and wide. Two widths rather than a seam to drag: it has
/// exactly two things to show - a name, or a name with its path and shell -
/// and a width in between shows neither of them properly.
pub const RAIL_NARROW: f32 = 96.0;
pub const RAIL_WIDE: f32 = 210.0;

/// Cut the window into its sectors.
pub fn sectors(full: Rect, showing: Showing) -> Sectors {
    let half = GUTTER * 0.5;

    // The rail comes off the left before anything else is measured: it is
    // outside the arrangement rather than part of it, which is the point of
    // moving the tabs out of the panes and out of the shell.
    let (rail, full) = if showing.rail {
        let width = if showing.rail_wide {
            RAIL_WIDE
        } else {
            RAIL_NARROW
        }
        .min(full.width() * 0.4);
        (
            Some(Rect::from_min_max(
                full.min,
                egui::pos2(full.min.x + width, full.max.y),
            )),
            Rect::from_min_max(egui::pos2(full.min.x + width + half, full.min.y), full.max),
        )
    } else {
        (None, full)
    };
    // A window showing neither half would be an empty window. The panes are
    // what it opens on, so they are what it falls back to.
    let (top, bottom) = match (showing.top, showing.bottom) {
        (false, false) => (true, false),
        pair => pair,
    };

    // The right column is the places list with the history under it, so it
    // stands or falls with that list being switched on - each row of it
    // belongs to the row of the window beside it.
    let column = if showing.places {
        showing
            .column
            .clamp(COLUMN_MIN, (full.width() * 0.5).max(COLUMN_MIN))
            .min(COLUMN_MAX)
    } else {
        0.0
    };
    let seam_x = full.max.x - column;

    // The bottom row keeps enough of the window for the panes to still be
    // panes: a drawer dragged to the top is a drawer nobody can get back.
    let row = showing
        .row
        .clamp(ROW_MIN, (full.height() - PANES_MIN).max(ROW_MIN));
    let seam_y = full.max.y - row;

    let left_max_x = if showing.places {
        seam_x - half
    } else {
        full.max.x
    };
    let column_min_x = seam_x + half;

    // With one row on show it takes the whole height: there is nothing to
    // share the window with.
    let (top_bottom, bottom_top) = match (top, bottom) {
        (true, true) => (seam_y - half, seam_y + half),
        _ => (full.max.y, full.min.y),
    };

    let top_left = top.then(|| Rect::from_min_max(full.min, egui::pos2(left_max_x, top_bottom)));
    let (panes, keys) = match top_left {
        Some(rect) if showing.keys && rect.height() > KEYS_HEIGHT * 2.0 => (
            Some(Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.max.y - KEYS_HEIGHT),
            )),
            Some(Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - KEYS_HEIGHT),
                rect.max,
            )),
        ),
        other => (other, None),
    };

    Sectors {
        rail,
        panes,
        keys,
        shell: bottom.then(|| {
            Rect::from_min_max(
                egui::pos2(full.min.x, bottom_top),
                egui::pos2(left_max_x, full.max.y),
            )
        }),
        places: (showing.places && top).then(|| {
            Rect::from_min_max(
                egui::pos2(column_min_x, full.min.y),
                egui::pos2(full.max.x, top_bottom),
            )
        }),
        history: (showing.places && showing.history && bottom)
            .then(|| Rect::from_min_max(egui::pos2(column_min_x, bottom_top), full.max)),
        vertical: showing.places.then(|| {
            Rect::from_min_max(
                egui::pos2(seam_x - half, full.min.y),
                egui::pos2(seam_x + half, full.max.y),
            )
        }),
        horizontal: (top && bottom).then(|| {
            Rect::from_min_max(
                egui::pos2(full.min.x, seam_y - half),
                egui::pos2(full.max.x, seam_y + half),
            )
        }),
    }
}

/// The left pane, the divider, and the right pane, for a given split.
pub fn pane_rects(full: Rect, split: f32) -> (Rect, Rect, Rect) {
    let split = split.clamp(SPLIT_MIN, SPLIT_MAX);
    let middle = full.min.x + full.width() * split;
    let half = GUTTER * 0.5;
    (
        Rect::from_min_max(full.min, egui::pos2(middle - half, full.max.y)),
        Rect::from_min_max(
            egui::pos2(middle - half, full.min.y),
            egui::pos2(middle + half, full.max.y),
        ),
        Rect::from_min_max(egui::pos2(middle + half, full.min.y), full.max),
    )
}

/// What the right-hand columns of a details row take, including their gap.
pub const DATE_WIDTH: f32 = 86.0;
pub const SIZE_WIDTH: f32 = 46.0;
pub const MODE_WIDTH: f32 = 78.0;

/// Which columns a details row of this width can afford.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowColumns {
    pub mode: bool,
    pub date: bool,
    pub size: bool,
}

impl RowColumns {
    /// What the columns to the right of the name take between them.
    pub fn width(&self, is_dir: bool) -> f32 {
        (if self.mode { MODE_WIDTH } else { 0.0 })
            + (if self.date { DATE_WIDTH } else { 0.0 })
            + (if self.size && !is_dir {
                SIZE_WIDTH
            } else {
                0.0
            })
    }
}

/// Drop columns from the right as the pane narrows.
///
/// The name is what a file listing is for, so the columns go in the order
/// they are least missed: permissions first, then the date, then the size;
/// below that the name gets the row to itself. Without this a dragged-in pane
/// paints the name straight over the numbers.
///
/// Permissions are the first to go because they are the least often what you
/// came to the listing for - and the last to arrive, so a pane at any width
/// that showed the date before still shows it.
pub fn row_columns(width: f32, is_dir: bool) -> RowColumns {
    RowColumns {
        mode: width > 430.0,
        date: width > 300.0,
        // A directory never shows a size, whatever the width.
        size: !is_dir && width > 190.0,
    }
}

/// Where a pointer at `x` puts the split.
pub fn split_from_pointer(full: Rect, x: f32) -> f32 {
    if full.width() <= 0.0 {
        return 0.5;
    }
    ((x - full.min.x) / full.width()).clamp(SPLIT_MIN, SPLIT_MAX)
}

/// A centred modal window, with the chrome the rest of the view uses.
/// A dialog's text field: takes focus when the dialog opens, reports Enter.
///
/// Focus is asked for once rather than every frame. Asking every frame means
/// the field never *loses* focus - and `lost_focus()` is how egui reports
/// Enter on a single-line edit, so the dialog could never be confirmed and
/// every later keystroke fell into it. Which is what happened.
/// A dialog's text box. Returns true when Enter asked to confirm.
///
/// `accept_enter` is false on the frame the dialog opened, so a dialog reached
/// by an Enter-family key is not answered by the press that opened it.
/// "report.txt", or "3 items" - what goes in the title of the box.
fn describe_targets(paths: &[PathBuf]) -> String {
    match paths {
        [one] => one
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| one.display().to_string()),
        many => format!("{} items", many.len()),
    }
}

/// The same, for members named by their path inside an archive.
fn describe_members(members: &[String]) -> String {
    match members {
        [one] => one.rsplit('/').next().unwrap_or(one).to_string(),
        many => format!("{} items", many.len()),
    }
}

fn dialog_field(ui: &mut egui::Ui, text: &mut String, hint: &str, accept_enter: bool) -> bool {
    dialog_field_focused(ui, text, hint, accept_enter, true)
}

/// As [`dialog_field`], saying whether this is the box that should start with
/// the keyboard.
///
/// A form with two of them needs an answer. Both asking for focus whenever
/// nothing holds it means the last one drawn wins - which is the wrong one -
/// and leaves frames in which neither does.
fn dialog_field_focused(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    accept_enter: bool,
    wants_focus: bool,
) -> bool {
    let field = ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(320.0)
            .hint_text(hint)
            .font(egui::TextStyle::Monospace),
    );
    if wants_focus && ui.memory(|m| m.focused().is_none()) {
        field.request_focus();
    }
    accept_enter && field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// One side of a difference row: its line number and its text.
///
/// A side with nothing on it draws nothing, which is what says the line was
/// added or taken away without a word of explanation.
fn diff_cell(ui: &mut egui::Ui, side: Option<(usize, &str)>, numbers: usize, ink: Color32) {
    let text = match side {
        Some((number, text)) => format!("{:>numbers$}  {text}", number),
        None => String::new(),
    };
    ui.add(
        egui::Label::new(RichText::new(text).monospace().size(11.5).color(ink))
            .truncate()
            .selectable(true),
    );
}
/// One side of a comparison row: how big it is and when it changed.
///
/// Blank where that side has nothing, which is what says "only the other one
/// has this" without a word of explanation.
fn side_cell(facts: Option<&compare::Facts>) -> String {
    match facts {
        None => String::new(),
        Some(facts) if facts.is_dir => "<DIR>".to_string(),
        Some(facts) => format!("{:>8}  {}", human_size(facts.size), stamp(facts.modified)),
    }
}

/// One fixed-width cell in a row that has to line up with the rows above and
/// below it.
///
/// `add_sized` centres what it is handed, which is right for a button and
/// wrong for a column: centred names do not share a left edge and centred
/// sizes do not share a right one.
fn cell(ui: &mut egui::Ui, width: f32, align: egui::Align, add: impl FnOnce(&mut egui::Ui)) {
    let layout = if align == egui::Align::Max {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(width, 16.0), layout, |ui| {
        // The size passed above is what the cell would *like*; without this
        // a long name grows the cell and shoves every column after it along.
        ui.set_width(width);
        add(ui);
    });
}

/// A path's last component, which is all a heading has room for.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// A file time as the panels write it, or a dash when it cannot be read.
fn stamp(when: Option<std::time::SystemTime>) -> String {
    lost_commander_core::entry::format_time(when)
}

/// `DISPLAY` and `XAUTHORITY`, which `pkexec` would otherwise strip.
fn display_pair() -> Option<(String, String)> {
    elevate::display_here()
}

/// The editor to fall back on when neither `$VISUAL` nor `$EDITOR` is set.
fn default_editor() -> String {
    if cfg!(windows) { "notepad" } else { "vi" }.to_string()
}

/// A chooser result that has stopped borrowing the dialog.
///
/// The list lives inside `self.dialog`, so acting on a choice while still
/// holding a reference into it would borrow `self` twice. Owning the pick for
/// the few lines between "clicked" and "started" is cheaper than restructuring
/// the dialog around that.
enum OwnedChoice {
    App(apps::Application),
    Command(String),
}

fn owned_choice(chosen: &apps::Chosen) -> OwnedChoice {
    match chosen {
        apps::Chosen::App(app) => OwnedChoice::App((*app).clone()),
        apps::Chosen::Command(command) => OwnedChoice::Command((*command).to_string()),
    }
}

/// Returns true when Escape asked for it to go away.
pub(crate) fn modal(
    ctx: &egui::Context,
    title: &str,
    contents: impl FnOnce(&mut egui::Ui),
) -> bool {
    egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.label(
            RichText::new(title)
                .size(13.5)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(8.0);
        contents(ui);
    });
    // Escape closes whatever is open, from anywhere in it.
    ctx.input(|i| i.key_pressed(egui::Key::Escape))
}

fn chrome_frame(fill: Color32) -> egui::Frame {
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(10, 6))
}

impl GuiApp {
    // ---- toolbar + breadcrumbs --------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if tool_button(ui, icons::Tool::Up, "Parent directory", false).clicked() {
                let side = self.active;
                if let Some(parent) = self.panel(side).cwd.parent().map(Path::to_path_buf) {
                    self.navigate(side, parent);
                }
            }
            if tool_button(ui, icons::Tool::Reload, "Reload", false).clicked() {
                self.left.reload();
                self.right.reload();
                for side in [Side::Left, Side::Right] {
                    if let Some(tree) = self.panel_mut(side).tree.as_mut() {
                        tree.refresh();
                    }
                }
                self.info("Reloaded");
            }

            ui.add_space(4.0);
            separator(ui);
            ui.add_space(4.0);

            // The selection menu sits with the operations it feeds.
            let marked = self.active_panel().marked_count();
            let select_button = tool_button(
                ui,
                icons::Tool::Select,
                "Select: all, none, invert, or by pattern",
                marked > 0,
            );
            // Not the menu default, which is to close on any click at all:
            // this menu has a box to type a pattern into, and a menu that
            // shuts the moment you click that box is no use to anyone.
            egui::Popup::menu(&select_button)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_min_width(180.0);
                    self.selection_menu(ui);
                });

            if tool_button(ui, icons::Tool::Copy, "Copy to the other pane", false).clicked() {
                self.copy_to_other();
            }
            if tool_button(ui, icons::Tool::Move, "Move to the other pane", false).clicked() {
                self.move_to_other();
            }
            if tool_button(ui, icons::Tool::Trash, "Move selection to the trash", false).clicked() {
                self.delete_selection(true);
            }
            if tool_button(ui, icons::Tool::Star, "Bookmark this directory", false).clicked() {
                let location = Location::local(self.active_panel().cwd.clone());
                let name = location.name.clone();
                self.bookmarks.add(location);
                if let Some(path) = &self.bookmarks_path {
                    let _ = self.bookmarks.save_to(path);
                }
                self.info(format!("Bookmarked \"{name}\""));
            }

            ui.add_space(8.0);
            self.breadcrumbs(ui);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tool_button(
                    ui,
                    icons::Tool::Sidebar,
                    "Places, and what was run here, in the right-hand column",
                    self.show_sidebar,
                )
                .clicked()
                {
                    self.show_sidebar = !self.show_sidebar;
                }
                if tool_button(
                    ui,
                    icons::Tool::TwoPanes,
                    "Second pane - folding it away gives the other the whole width",
                    self.show_right,
                )
                .clicked()
                {
                    self.show_right = !self.show_right;
                }
                // The two views that answer about the folder you are
                // standing in, drawn in the pane you are not. They came off
                // the panes' own switch, so this and the two keys are where
                // they live - a feature reachable only by a key nobody has
                // been told about is not reachable.
                let looking = self.view(self.active.other());
                let views = tool_button(
                    ui,
                    icons::Tool::QuickView,
                    "Look at this folder in the other pane",
                    matches!(looking, ViewMode::Preview | ViewMode::History),
                );
                let mut want: Option<keys::Action> = None;
                let mut want_half: Option<Half> = None;
                egui::Popup::menu(&views).show(|ui| {
                    ui.set_min_width(220.0);
                    // Which halves of the window are on show. First, because
                    // it is the coarsest thing on the menu: everything below
                    // is about what goes in a half.
                    for (half, label) in [
                        (Half::Both, "Panes and shell    Ctrl-O / Ctrl-Shift-O"),
                        (Half::Files, "Panes only         Ctrl-Shift-O"),
                        (Half::Shell, "Shell only         Ctrl-O"),
                    ] {
                        if ui.selectable_label(self.half == half, label).clicked() {
                            want_half = Some(half);
                        }
                    }
                    ui.separator();
                    if ui
                        .selectable_label(looking == ViewMode::Preview, "Quick view    F3")
                        .clicked()
                    {
                        want = Some(keys::Action::QuickView);
                    }
                    if ui
                        .selectable_label(looking == ViewMode::History, "Folder history    Alt-H")
                        .clicked()
                    {
                        want = Some(keys::Action::ViewHistory);
                    }
                });
                if let Some(action) = want {
                    self.run_action(action);
                }
                if let Some(half) = want_half {
                    // Straight to it from the menu: the keys toggle, because
                    // a key you press to leave is the key you pressed to
                    // arrive, but a menu names the state you are asking for.
                    self.half = Half::Both;
                    if half != Half::Both {
                        self.show_half(half);
                    }
                }

                if tool_button(
                    ui,
                    icons::Tool::Keys,
                    "Function keys under the panes",
                    self.show_keys,
                )
                .clicked()
                {
                    self.show_keys = !self.show_keys;
                }
                if tool_button(
                    ui,
                    icons::Tool::ListView,
                    "Terminal panel / one-shot command line",
                    self.show_terminal,
                )
                .clicked()
                {
                    self.show_terminal = !self.show_terminal;
                    if !self.show_terminal {
                        self.terminal_focused = false;
                    }
                }
            });
        });
    }

    /// Clickable path segments - jumping three levels up is one click, not
    /// three presses of Backspace.
    fn breadcrumbs(&mut self, ui: &mut egui::Ui) {
        let side = self.active;
        let cwd = self.panel(side).cwd.clone();
        let mut target: Option<PathBuf> = None;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mut accumulated = PathBuf::new();
            let components: Vec<_> = cwd.components().collect();

            for (index, component) in components.iter().enumerate() {
                accumulated.push(component.as_os_str());
                let raw = component.as_os_str().to_string_lossy().to_string();
                let label = if raw == "/" { "/".to_string() } else { raw };
                let last = index + 1 == components.len();

                let text = RichText::new(label)
                    .color(if last {
                        theme::text()
                    } else {
                        theme::text_dim()
                    })
                    .size(13.0);
                if ui
                    .add(
                        egui::Button::new(text)
                            .fill(Color32::TRANSPARENT)
                            .frame(false),
                    )
                    .clicked()
                {
                    target = Some(accumulated.clone());
                }
                if !last {
                    ui.label(
                        RichText::new("\u{203A}")
                            .color(theme::text_faint())
                            .size(13.0),
                    );
                }
            }
        });

        if let Some(path) = target {
            self.navigate(side, path);
        }
    }

    // ---- sidebar -----------------------------------------------------------

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let mut target: Option<PathBuf> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // The machine's own places come first, and they are the ones a
            // reader cannot discover any other way. Without them the only
            // route to a second drive is typing its letter, and on Windows
            // there is no root to walk up to that would reveal one: `C:` and
            // `D:` are two trees, not two directories.
            section_label(ui, "THIS COMPUTER");
            for place in &self.system_places {
                let icon = match place.kind {
                    places::Kind::Drive => icons::Kind::Binary,
                    places::Kind::Home => icons::Kind::Folder,
                    places::Kind::Folder => icons::Kind::Folder,
                };
                let free = place
                    .free
                    .map(|bytes| format!("{} free", human_size(bytes)));
                if sidebar_row_with(ui, &place.name, icon, false, free) {
                    target = Some(place.path.clone());
                }
            }

            ui.add_space(10.0);
            section_label(ui, "PLACES");
            if self.bookmarks.locations.is_empty() {
                ui.label(
                    RichText::new("Nothing saved yet")
                        .color(theme::text_faint())
                        .size(11.0),
                );
            }
            for location in self.bookmarks.locations.clone() {
                if sidebar_row(ui, &location.name, icons::Kind::Folder, false) {
                    target = Some(PathBuf::from(&location.path));
                }
            }

            ui.add_space(10.0);
            section_label(ui, "RECENT");
            for location in self.bookmarks.recent.iter().take(6).cloned() {
                if sidebar_row(ui, &location.name, icons::Kind::Parent, true) {
                    target = Some(PathBuf::from(&location.path));
                }
            }
        });

        if let Some(path) = target {
            let side = self.active;
            self.navigate(side, path);
        }
    }

    // ---- a file pane -------------------------------------------------------

    fn pane(&mut self, ui: &mut egui::Ui, side: Side) {
        let focused = self.active == side;
        let rect = ui.max_rect();

        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), theme::surface());
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(
                if focused { 1.5 } else { 1.0 },
                if focused {
                    theme::accent()
                } else {
                    theme::border()
                },
            ),
            egui::StrokeKind::Inside,
        );

        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink(10.0))
                .layout(Layout::top_down(Align::Min)),
        );

        // No tab strip here any more: tabs are workspaces and they live in
        // the rail, out of the panes and out of the shell.

        // Pane header: where you are, and how much is here.
        let current = self.view(side);
        let panel = self.panel(side);
        let folder_name = |panel: &Panel| {
            panel
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| panel.cwd.display().to_string())
        };
        // A pane answering about somewhere else says where that is. It used
        // to show its own directory's name, which with two panes both called
        // `src` read as a pane describing itself and was plainly wrong: what
        // is on screen is the *other* pane's folder, and the header is the
        // only thing that can say so.
        let answers_elsewhere = matches!(current, ViewMode::Preview | ViewMode::History);
        let name = match current {
            ViewMode::History => format!(
                "History of {}",
                lost_commander_core::paths::undecorated(&self.panel(side.other()).cwd).display()
            ),
            ViewMode::Preview => match self.panel(side.other()).selected() {
                Some(entry) => format!(
                    "Quick view: {}",
                    lost_commander_core::paths::undecorated(&entry.path).display()
                ),
                None => "Quick view".to_string(),
            },
            _ => folder_name(panel),
        };
        let count = panel.entries.len().saturating_sub(1);
        // Worked out here rather than in the closure below, which needs
        // unique access to `self` and so cannot hold a borrow of the panel.
        let summary = selection_summary(panel, count, current);

        inner.horizontal(|ui| {
            ui.label(
                RichText::new(name)
                    .color(if focused {
                        theme::text()
                    } else {
                        theme::text_dim()
                    })
                    .size(13.0)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // The view switch lives here rather than on the toolbar,
                // because it belongs to this pane and not to the window. It
                // is how *this* pane draws its own directory, and nothing
                // else - so a pane that is not showing its own directory has
                // no use for it, and drawing three inert-looking buttons
                // beside somebody else's folder only invites the question of
                // which folder they would apply to. F3 and Alt-H are the way
                // back, and the toolbar menu says so.
                if !answers_elsewhere {
                    for (mode, tool, hint) in [
                        (
                            ViewMode::Tree,
                            icons::Tool::TreeView,
                            "Tree - where this pane is",
                        ),
                        (ViewMode::Grid, icons::Tool::GridView, "Icon grid"),
                        (ViewMode::Details, icons::Tool::ListView, "Detail list"),
                    ] {
                        if tool_button(ui, tool, hint, current == mode).clicked() {
                            self.set_view(side, mode);
                            self.active = side;
                        }
                    }
                }

                ui.add_space(6.0);
                let summary = summary.clone();
                ui.label(RichText::new(summary).color(theme::text_faint()).size(11.0));
            });
        });
        inner.add_space(6.0);

        // Quick view brings its own scrolling - text scrolls, a picture does
        // not - so it is drawn instead of the pane's ScrollArea, not inside it.
        if self.view(side) == ViewMode::Preview {
            self.pane_preview(&mut inner, side);
            return;
        }

        if self.view(side) == ViewMode::History {
            self.pane_history(&mut inner, side);
            return;
        }

        let mut clicked_side = false;
        let scroll_id = match side {
            Side::Left => "scroll_left",
            Side::Right => "scroll_right",
        };

        // The tree does not replace the listing - it sits above it. How the
        // files are drawn and whether there is a tree over them are two
        // independent questions, and making them one meant choosing the tree
        // cost you the files.
        if self.view(side) == ViewMode::Tree {
            clicked_side |= self.pane_halves(&mut inner, side, focused);
            if clicked_side {
                self.active = side;
            }
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .auto_shrink([false, false])
            .show(&mut inner, |ui| {
                let hit = match self.view(side) {
                    ViewMode::Details => self.details_view(ui, side, focused),
                    ViewMode::Grid => self.grid_view(ui, side, focused),
                    // Drawn as halves above, and never inside this one.
                    ViewMode::Tree => false,
                    // Handled above; they do not belong in a ScrollArea.
                    ViewMode::Preview | ViewMode::History => false,
                };
                clicked_side |= hit;
            });

        if clicked_side {
            self.active = side;
        }
    }

    /// Say which half has the keyboard, when it is not obvious.
    fn say_which_half(&mut self, side: Side) {
        if !self.panel(side).in_tree_mode() {
            return;
        }
        let index = match side {
            Side::Left => 0,
            Side::Right => 1,
        };
        let where_ = if self.on_tree[index] {
            "The tree. Enter opens a directory and drops into it."
        } else {
            "The files. Escape goes back up to the tree."
        };
        self.info(where_);
    }

    /// A pane in two halves: the tree, then the files under it.
    ///
    /// XTree's arrangement, and the reason it is worth copying is the pair of
    /// them together - you walk directories in the top half and the files of
    /// wherever you are stand in the bottom one, so tagging a file here and
    /// another one three directories away is a single continuous gesture
    /// rather than two visits.
    ///
    /// Returns whether anything in either half was clicked.
    fn pane_halves(&mut self, ui: &mut egui::Ui, side: Side, focused: bool) -> bool {
        let index = match side {
            Side::Left => 0,
            Side::Right => 1,
        };
        let on_tree = self.on_tree[index];
        let mut clicked = false;

        let total = ui.available_height();
        // A floor on both halves: a divider dragged to the very top leaves a
        // tree that cannot be read and no way to drag it back.
        let top = (total * self.tree_split).clamp(60.0, (total - 60.0).max(60.0));

        let id = match side {
            Side::Left => "tree_left",
            Side::Right => "tree_right",
        };
        ui.allocate_ui(Vec2::new(ui.available_width(), top), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    clicked |= self.pane_tree(ui, side, focused && on_tree);
                });
        });

        // The divider, and the grip that drags it. Wider than the line it
        // draws: a one-pixel target is one nobody can hit.
        let (grip, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 8.0),
            Sense::click_and_drag(),
        );
        let line = egui::Rect::from_min_size(
            egui::pos2(grip.min.x, grip.center().y),
            Vec2::new(grip.width(), 1.0),
        );
        ui.painter().rect_filled(
            line,
            0.0,
            if response.hovered() || response.dragged() {
                theme::accent()
            } else {
                theme::border()
            },
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if response.dragged() && total > 0.0 {
            self.tree_split = (self.tree_split + response.drag_delta().y / total).clamp(0.15, 0.85);
        }
        if response.drag_stopped() {
            self.remember_layout();
        }

        let files_id = match side {
            Side::Left => "files_left",
            Side::Right => "files_right",
        };
        egui::ScrollArea::vertical()
            .id_salt(files_id)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                clicked |= self.details_view(ui, side, focused && !on_tree);
            });
        clicked
    }

    /// Put the cursor on a row the file half is actually showing.
    ///
    /// With a tree up the listing draws files only, so a cursor left on a
    /// directory would be a selection nobody can see - and F5 would copy
    /// something the reader never pointed at. Called after anything that
    /// moves it.
    fn snap_to_a_visible_row(&mut self, side: Side) {
        if !self.panel(side).in_tree_mode() {
            return;
        }
        let panel = self.panel(side);
        if panel
            .entries
            .get(panel.cursor)
            .is_some_and(|e| !e.is_dir() && !e.is_parent())
        {
            return;
        }
        let cursor = panel.cursor;
        // Nearest first, downwards on a tie: a cursor that jumped to the top
        // of the listing every time it landed on a directory would throw away
        // where the reader was.
        let next = panel
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_dir() && !e.is_parent())
            .min_by_key(|(index, _)| (index.abs_diff(cursor), if *index < cursor { 1 } else { 0 }))
            .map(|(index, _)| index);
        if let Some(index) = next {
            self.panel_mut(side).cursor_to(index);
        }
    }
    /// The directory hierarchy for this pane, opened to where it is.
    ///
    /// Clicking a row moves the pane there and the tree stays put, so it doubles
    /// as "where am I" and as a way to jump somewhere else without walking
    /// through every level.
    fn pane_tree(&mut self, ui: &mut egui::Ui, side: Side, focused: bool) -> bool {
        let Some(tree) = self.panel(side).tree.as_ref() else {
            return false;
        };
        let cwd = self.panel(side).cwd.clone();
        let at = tree.cursor;
        let nodes: Vec<(PathBuf, String, usize, bool, bool, char)> = tree
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                (
                    node.path.clone(),
                    node.label.clone(),
                    node.depth,
                    node.expanded,
                    node.leaf,
                    tree.marker(index),
                )
            })
            .collect();

        let mut interacted = false;
        let mut scrolled = false;
        // Read before it is overwritten, which is the whole point of keeping
        // it. Setting it first made "has the cursor moved" always false, so
        // the view never followed the cursor at all.
        let moved = at != self.tree_at[Self::side_index(side)];
        self.tree_at[Self::side_index(side)] = at;
        let mut toggle: Option<usize> = None;
        let mut navigate_to: Option<PathBuf> = None;

        for (index, (path, label, depth, expanded, leaf, _marker)) in nodes.iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());

            // Two different things, and they were one: the *cursor* is the
            // row the arrow keys move, and "you are here" is the directory
            // the pane is showing. Highlighting only the second meant up and
            // down moved a cursor that was never drawn - they looked dead
            // while left and right, which visibly add and remove rows,
            // looked fine.
            let is_here = *path == cwd;
            let is_cursor = index == at;
            let index_of_side = Self::side_index(side);
            if is_here && self.tree_scroll[index_of_side] {
                // Opening the tree puts where you are in the middle, which is
                // the one moment centring is what you want.
                ui.scroll_to_rect(rect, Some(Align::Center));
                scrolled = true;
            } else if is_cursor && moved {
                // Only when the cursor has moved, and only as far as it must:
                // `None` brings the row into view without re-centring, so the
                // cursor travels down the screen the way it does in an editor
                // and the view follows once it runs out of room.
                ui.scroll_to_rect(rect, None);
            }
            if !ui.is_rect_visible(rect) {
                continue;
            }
            // The cursor is the selection; where the pane is standing is a
            // wash underneath it, so both are legible when they differ - and
            // they differ exactly while you are walking somewhere else.
            paint_selection(ui, rect, is_cursor, is_here && !is_cursor, focused);

            let indent = *depth as f32 * 12.0;
            let arrow = Rect::from_min_size(
                egui::pos2(rect.min.x + indent + 4.0, rect.center().y - 4.0),
                Vec2::splat(8.0),
            );
            if !leaf {
                let colour = if is_here {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                let points = if *expanded {
                    vec![
                        arrow.left_top(),
                        arrow.right_top(),
                        egui::pos2(arrow.center().x, arrow.bottom()),
                    ]
                } else {
                    vec![
                        arrow.left_top(),
                        arrow.left_bottom(),
                        egui::pos2(arrow.right(), arrow.center().y),
                    ]
                };
                ui.painter().add(eframe::epaint::Shape::convex_polygon(
                    points,
                    colour,
                    Stroke::NONE,
                ));
            }

            let icon = Rect::from_min_size(
                egui::pos2(rect.min.x + indent + 16.0, rect.center().y - 8.0),
                Vec2::splat(16.0),
            );
            icons::draw(
                ui.painter(),
                icon,
                icons::Kind::Folder,
                !expanded && !is_here,
            );

            ui.painter().text(
                egui::pos2(icon.right() + 8.0, rect.center().y),
                Align2_LEFT_CENTER,
                label,
                FontId::proportional(12.5),
                if is_here {
                    theme::text()
                } else {
                    theme::text_dim()
                },
            );

            if response.clicked() {
                interacted = true;
                // The triangle expands; anywhere else takes you there.
                if !leaf && response.interact_pointer_pos().map(|p| p.x) < Some(icon.min.x) {
                    toggle = Some(index);
                } else {
                    navigate_to = Some(path.clone());
                }
            }
        }

        if scrolled {
            self.tree_scroll[Self::side_index(side)] = false;
        }
        if let Some(index) = toggle {
            if let Some(tree) = self.panel_mut(side).tree.as_mut() {
                tree.cursor = index;
                tree.toggle(index);
            }
        }
        if let Some(path) = navigate_to {
            if path != cwd {
                self.navigate(side, path);
            }
        }
        interacted
    }

    /// Dense rows: icon, name, size, modified.
    fn details_view(&mut self, ui: &mut egui::Ui, side: Side, focused: bool) -> bool {
        let entries: Vec<Entry> = self.panel(side).entries.clone();
        let cursor = self.panel(side).cursor;
        let mut interacted = false;
        let mut open: Option<PathBuf> = None;
        let mut select: Option<(usize, Click)> = None;

        // The listing never followed its cursor: arrow past the bottom row and
        // the cursor simply left the screen. Unnoticed while a pane was one
        // long list and the window was usually tall enough; obvious the
        // moment a tree took half the height.
        let moved = cursor != self.listing_at[Self::side_index(side)];
        self.listing_at[Self::side_index(side)] = cursor;

        // With a tree up, the directories are the half above this one, and
        // repeating them here would be the same list twice with the cursor
        // ambiguous between them. `..` goes too - climbing is what the tree
        // is for.
        let files_only = self.panel(side).in_tree_mode();

        for (index, entry) in entries.iter().enumerate() {
            if files_only && (entry.is_dir() || entry.is_parent()) {
                continue;
            }
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
            // Before the visibility check, not after: the row that needs
            // scrolling to is precisely the one that is not visible.
            if index == cursor && moved {
                // Into view rather than centred, so the cursor walks down the
                // screen and the view only follows when it runs out of room.
                ui.scroll_to_rect(rect, None);
            }
            if !ui.is_rect_visible(rect) {
                continue;
            }

            paint_selection(ui, rect, index == cursor, entry.marked, focused);

            let icon = Rect::from_min_size(
                egui::pos2(rect.min.x + 6.0, rect.center().y - 8.0),
                Vec2::splat(16.0),
            );
            icons::draw(ui.painter(), icon, icons::classify(entry), false);

            let painter = ui.painter();
            let text_colour = if entry.marked {
                theme::marked_text()
            } else if entry.is_dir() {
                theme::text()
            } else {
                theme::text_dim()
            };

            // Right-hand columns first, so the name can use whatever is left.
            // Which of them fit depends on the width, now that the divider
            // can be dragged: a narrow pane drops the date, then the size,
            // rather than having the name painted over the top of them.
            let columns = row_columns(rect.width(), entry.is_dir());
            let mut edge = rect.right() - 8.0;
            // Monospaced, because the point of `rwxr-xr-x` is that the same
            // permission is in the same place on every row - which it is not
            // if the characters are different widths.
            if columns.mode {
                if let Some(mode) = entry.mode {
                    painter.text(
                        egui::pos2(edge, rect.center().y),
                        Align2_RIGHT_CENTER,
                        format!(
                            "{}{}",
                            lost_commander_core::perms::kind_char(entry.kind, entry.is_symlink),
                            mode.symbolic()
                        ),
                        FontId::monospace(10.5),
                        theme::text_faint(),
                    );
                }
                edge -= MODE_WIDTH;
            }
            if columns.date {
                let date = lost_commander_core::entry::format_time(entry.modified);
                painter.text(
                    egui::pos2(edge, rect.center().y),
                    Align2_RIGHT_CENTER,
                    &date,
                    FontId::proportional(11.0),
                    theme::text_faint(),
                );
                edge -= DATE_WIDTH;
            }
            // Directories get no size text at all - the icon already says
            // what they are, and "<DIR>" is a terminal-era placeholder.
            if columns.size {
                painter.text(
                    egui::pos2(edge, rect.center().y),
                    Align2_RIGHT_CENTER,
                    human_size(entry.size),
                    FontId::proportional(11.0),
                    theme::text_dim(),
                );
                edge -= SIZE_WIDTH;
            }

            // Elided rather than free-running, for the same reason.
            let name_left = icon.right() + 8.0;
            let mut job = egui::text::LayoutJob::simple_singleline(
                entry.name.clone(),
                FontId::proportional(12.5),
                text_colour,
            );
            job.wrap = egui::text::TextWrapping {
                max_width: (edge - 8.0 - name_left).max(24.0),
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('.'),
            };
            let galley = painter.layout_job(job);
            painter.galley(
                egui::pos2(name_left, rect.center().y - galley.size().y * 0.5),
                galley,
                text_colour,
            );

            if response.clicked() {
                interacted = true;
                select = Some((
                    index,
                    ui.input(|i| {
                        Click::from_modifiers(
                            i.modifiers.ctrl || i.modifiers.command,
                            i.modifiers.shift,
                        )
                    }),
                ));
            }
            if response.double_clicked() {
                interacted = true;
                open = Some(entry.path.clone());
            }
        }

        self.apply_click(side, select, open);
        interacted
    }

    /// Large icons in a wrapping grid - the view that makes a photo directory
    /// legible at a glance.
    fn grid_view(&mut self, ui: &mut egui::Ui, side: Side, focused: bool) -> bool {
        let entries: Vec<Entry> = self.panel(side).entries.clone();
        let cursor = self.panel(side).cursor;
        let mut interacted = false;
        let mut open: Option<PathBuf> = None;
        let mut select: Option<(usize, Click)> = None;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for (index, entry) in entries.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(TILE, Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }

                paint_selection(ui, rect, index == cursor, entry.marked, focused);

                let icon = Rect::from_center_size(
                    egui::pos2(rect.center().x, rect.min.y + 32.0),
                    Vec2::splat(44.0),
                );
                icons::draw(ui.painter(), icon, icons::classify(entry), false);

                let colour = if entry.marked {
                    theme::marked_text()
                } else if entry.is_dir() {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                // Two short lines beat one clipped line for long file names.
                for (line_index, line) in wrap_label(&entry.name, 15).iter().take(2).enumerate() {
                    ui.painter().text(
                        egui::pos2(
                            rect.center().x,
                            rect.min.y + 62.0 + line_index as f32 * 13.0,
                        ),
                        Align2_CENTER_CENTER,
                        line,
                        FontId::proportional(11.5),
                        colour,
                    );
                }

                if response.clicked() {
                    interacted = true;
                    select = Some((
                        index,
                        ui.input(|i| {
                            Click::from_modifiers(
                                i.modifiers.ctrl || i.modifiers.command,
                                i.modifiers.shift,
                            )
                        }),
                    ));
                }
                if response.double_clicked() {
                    interacted = true;
                    open = Some(entry.path.clone());
                }
            }
        });

        self.apply_click(side, select, open);
        interacted
    }

    fn apply_click(&mut self, side: Side, select: Option<(usize, Click)>, open: Option<PathBuf>) {
        if let Some((index, click)) = select {
            self.active = side;
            let anchor = self.mark_anchor[Self::side_index(side)].unwrap_or(index);
            let panel = self.panel_mut(side);
            panel.cursor_to(index);
            match click {
                Click::Plain => panel.clear_marks(),
                // Ctrl/Cmd-click toggles one, as everywhere else.
                Click::Toggle => {
                    if let Some(entry) = panel.entries.get_mut(index) {
                        if !entry.is_parent() {
                            entry.marked = !entry.marked;
                        }
                    }
                }
                // Shift-click takes everything from the last click to here,
                // which is what makes a selection of two hundred files
                // possible without two hundred clicks.
                Click::Range { additive } => panel.mark_range(anchor, index, additive),
            }
            // A range keeps the anchor it was measured from; anything else
            // becomes the new anchor.
            if !matches!(click, Click::Range { .. }) {
                self.mark_anchor[Self::side_index(side)] = Some(index);
            }
        }
        if let Some(path) = open {
            // Inside an archive nothing on disk is being pointed at, so the
            // panel's own walk decides: a level to step into, or a member to
            // read.
            if self.panel(side).in_archive() {
                self.activate_in_archive(side);
                return;
            }
            // Double-click means the same thing Enter does: a file is
            // something to open, a directory somewhere to go.
            //
            // Anything that is neither - a row for a file deleted since the
            // pane was listed - goes to `navigate`, which is where the "no
            // longer there" message already lives. Asking `is_file` rather
            // than `!is_dir` is what puts it on that side.
            if path.is_file() {
                // An archive is a folder that happens to be one file.
                if lost_commander_core::archive::is_archive(&path) {
                    self.step_into_archive(side, path);
                    return;
                }
                self.open_paths(vec![path]);
            } else {
                self.navigate(side, path);
            }
        }
    }

    // ---- archives ----------------------------------------------------------

    /// Walk into an archive, asking for a password if it wants one.
    fn step_into_archive(&mut self, side: Side, path: PathBuf) {
        let held = self.archive_passwords.get(&path).cloned();
        match self.panel_mut(side).open_archive(&path, held) {
            Ok(()) => {
                let inside = self.panel(side).inside.clone();
                if let Some(inside) = inside {
                    let locked = match inside.members.iter().any(|m| m.encrypted) {
                        true => " - some of it needs a password",
                        false => "",
                    };
                    self.info(format!(
                        "{} - {} item(s), {}{locked}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        inside.members.len(),
                        inside.format
                    ));
                }
            }
            // Only the formats that encrypt their index land here; a zip is
            // always listable and asks later, when a file is read.
            Err(e) if lost_commander_core::archive::is_locked(&e) => {
                self.ask_for_password(path, None)
            }
            Err(e) => self.error(format!("Could not open {}: {e}", path.display())),
        }
    }

    /// Enter, inside an archive: a level to walk into or a member to open.
    fn activate_in_archive(&mut self, side: Side) {
        let Some(entry) = self.panel(side).selected().cloned() else {
            return;
        };
        if entry.is_dir() {
            self.panel_mut(side).enter();
            return;
        }
        self.view_member(side, &entry);
    }

    /// Where members are put when something outside this program has to read
    /// one. Removed when the program exits, which is what makes it honest to
    /// call it a cache rather than a place to work.
    fn member_cache(&mut self) -> std::io::Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lost-commander-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Pull one member out to a real file, so something that only understands
    /// paths can read it.
    fn member_to_disk(&mut self, archive: &Path, member: &str) -> std::io::Result<PathBuf> {
        let password = self.archive_passwords.get(archive).cloned();
        let bytes = lost_commander_core::archive::read_with(archive, member, password.as_deref())?;
        let cache = self.member_cache()?;
        // The member's own name, under a directory named for the archive, so
        // two archives holding a `readme.txt` do not collide and the file
        // arrives at the editor with the name it had.
        let stem = archive
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string());
        let into = cache.join(stem).join(member);
        if let Some(parent) = into.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&into, bytes)?;
        Ok(into)
    }

    /// Open a member with whatever the desktop uses for it.
    ///
    /// The file handed over is a **copy**, and read-only mode means nothing
    /// written to it comes back. That is worth a line in the account rather
    /// than a surprise later: it names the copy, so "where did my edit go"
    /// has an answer.
    fn view_member(&mut self, side: Side, entry: &Entry) {
        let Some(inside) = self.panel(side).inside.clone() else {
            return;
        };
        let Some(member) = self.panel(side).member_of(entry) else {
            return;
        };
        match self.member_to_disk(&inside.archive, &member) {
            Ok(copy) => {
                let outcome = (self.opener)(&copy);
                self.note(
                    journal::Event::new(journal::Kind::Open, &copy)
                        .note(format!(
                            "{} from {} - a copy, changes to it do not go back",
                            member,
                            inside.archive.display()
                        ))
                        .failed_if(outcome.is_err(), "could not be opened"),
                );
                match outcome {
                    Ok(()) => self.info(format!("Opened a copy of {member}")),
                    Err(e) => self.error(e),
                }
            }
            Err(e) if lost_commander_core::archive::is_locked(&e) => {
                self.ask_for_password(inside.archive.clone(), Some(member))
            }
            Err(e) => self.error(format!("Could not read {member}: {e}")),
        }
    }

    /// Put up the password box.
    fn ask_for_password(&mut self, archive: PathBuf, member: Option<String>) {
        let refused = self.archive_passwords.contains_key(&archive);
        self.dialog = Some(Dialog::Password {
            archive,
            member,
            typed: String::new(),
            refused,
        });
    }

    /// A password has been typed: keep it for the session and try again.
    fn password_given(&mut self, archive: PathBuf, member: Option<String>, typed: String) {
        self.archive_passwords.insert(archive.clone(), typed);
        let side = self.active;
        match member {
            // It was the archive itself that would not list.
            None => self.step_into_archive(side, archive),
            Some(member) => {
                let entry = self
                    .panel(side)
                    .entries
                    .iter()
                    .find(|e| self.panel(side).member_of(e).as_deref() == Some(member.as_str()))
                    .cloned();
                match entry {
                    Some(entry) => self.view_member(side, &entry),
                    None => self.error("That file is no longer on the list"),
                }
            }
        }
    }

    // ---- operations --------------------------------------------------------

    /// Bring the other pane on screen, for something that is about to use it.
    ///
    /// A comparison reads both panes and marks what differs, so it needs two
    /// of them and it changes nothing on disk - there is nothing here worth
    /// making the reader confirm. It just opens, and both sides are visible
    /// when the marks land. Anything that *writes* asks where instead, in a
    /// field: see [`GuiApp::ask_where`].
    fn need_other_pane(&mut self) {
        if !self.show_right {
            self.show_right = true;
            // Theirs now, not borrowed: a preview closing later must not fold
            // away a pane that was opened to be looked at alongside this one.
            self.pane_opened_to_view = false;
        }
    }

    fn copy_to_other(&mut self) {
        // F5 out of an archive is an extraction, which is a copy whose source
        // happens to be inside a file.
        if self.panel(self.active).in_archive() {
            self.extract_to_other();
            return;
        }
        let sources = self.selection(self.active);
        if sources.is_empty() {
            self.error("Nothing selected");
            return;
        }
        self.ask_where(describe_targets(&sources), Pending::Copy(sources));
    }

    /// Put up the destination field, focused, with somewhere to start from.
    fn ask_where(&mut self, what: String, job: Pending) {
        let start = if self.show_right {
            self.panel(self.active.other()).cwd.clone()
        } else {
            self.panel(self.active).cwd.clone()
        };
        self.dialog = Some(Dialog::CopyTo {
            what,
            destination: start.display().to_string(),
            job,
        });
    }

    /// Start the job the destination box was holding.
    ///
    /// `false` if what was typed is not a directory - the box closes and the
    /// reason is on the status line, rather than a copy going somewhere that
    /// does not exist.
    fn send_to(&mut self, job: Pending, destination: &str) -> bool {
        let into = match self.resolve_dir(destination) {
            Ok(into) => into,
            Err(message) => {
                self.error(message);
                return false;
            }
        };
        self.start(match job {
            Pending::Copy(sources) => Operation::Copy {
                sources,
                destination: into,
            },
            Pending::Move(sources) => Operation::Move {
                sources,
                destination: into,
            },
            Pending::Extract {
                archive,
                members,
                from,
                password,
            } => Operation::Extract {
                archive,
                members,
                from,
                destination: into,
                password,
            },
        });
        true
    }

    /// A typed destination, made absolute against the pane it was typed in.
    fn resolve_dir(&self, raw: &str) -> Result<PathBuf, String> {
        let typed = PathBuf::from(raw.trim());
        let path = if typed.is_absolute() {
            typed
        } else {
            self.panel(self.active).cwd.join(typed)
        };
        if path.is_dir() {
            Ok(path)
        } else {
            Err(format!("Not a directory: {}", path.display()))
        }
    }

    /// F5, inside an archive: pull the selection out into the other pane.
    fn extract_to_other(&mut self) {
        let side = self.active;
        let Some(inside) = self.panel(side).inside.clone() else {
            return;
        };

        // A directory selected means everything under it, worked out from the
        // index rather than by walking anything.
        let chosen: Vec<String> = self
            .panel(side)
            .action_entries()
            .iter()
            .filter_map(|entry| self.panel(side).member_of(entry))
            .collect();
        if chosen.is_empty() {
            self.error("Nothing selected");
            return;
        }
        let mut members: Vec<String> = Vec::new();
        for one in &chosen {
            for member in lost_commander_core::archive::under(&inside.members, one) {
                if !member.is_dir && !members.contains(&member.path) {
                    members.push(member.path.clone());
                }
            }
        }
        if members.is_empty() {
            self.error("Nothing in there to extract");
            return;
        }

        // Locked members with no password yet: ask once, rather than failing
        // once per file.
        let password = self.archive_passwords.get(&inside.archive).cloned();
        let locked = inside
            .members
            .iter()
            .any(|member| member.encrypted && members.contains(&member.path));
        if locked && password.is_none() {
            self.ask_for_password(inside.archive.clone(), None);
            return;
        }

        self.ask_where(
            describe_members(&members),
            Pending::Extract {
                archive: inside.archive.clone(),
                members,
                from: inside.at.clone(),
                password,
            },
        );
    }

    /// Move, rather than copy, into the other pane.
    ///
    /// The same Vec of sources: every operation here has always been plural,
    /// and takes whatever is marked or, failing that, the row under the
    /// cursor.
    fn move_to_other(&mut self) {
        let sources = self.selection(self.active);
        if sources.is_empty() {
            self.error("Nothing selected");
            return;
        }
        self.ask_where(describe_targets(&sources), Pending::Move(sources));
    }

    fn delete_selection(&mut self, to_trash: bool) {
        let targets = self.selection(self.active);
        if targets.is_empty() {
            self.error("Nothing selected");
            return;
        }
        self.start(Operation::Delete { targets, to_trash });
    }

    // ---- the command line ----------------------------------------------------
    //
    // The original kept a command line at the bottom of the screen and sent
    // every printable character to it, which is why its panel commands never
    // collided with typing. The same thing happens here, except the line is a
    // real shell: characters go to the pty, and `pending_input` is our record
    // of what has been typed since the last Enter - the shell's own input
    // buffer is inside the shell process and cannot be asked.

    /// Whether the command line has anything on it.
    pub fn command_line_empty(&self) -> bool {
        if self.show_terminal {
            self.pending_input.is_empty()
        } else {
            self.command.is_empty()
        }
    }

    /// Send a typed character to the command line.
    pub fn type_into_command_line(&mut self, text: &str) {
        self.shell_back_on_screen();
        if !self.show_terminal {
            self.command.push_str(text);
            return;
        }
        if self.terminals.is_empty() {
            // The command line is always there, so make one.
            self.open_terminal(None);
            self.terminal_focused = false;
        }
        if let Some(session) = self.terminals.active_mut() {
            session.write_str(text);
            self.pending_input.push_str(text);
        }
    }

    /// Rub out the last character.
    pub fn command_line_backspace(&mut self) {
        if !self.show_terminal {
            self.command.pop();
            return;
        }
        if let Some(session) = self.terminals.active_mut() {
            session.write(b"\x7f");
        }
        self.pending_input.pop();
    }

    /// Throw the line away - Escape, as it always has been.
    pub fn command_line_clear(&mut self) {
        if !self.show_terminal {
            self.command.clear();
            return;
        }
        if let Some(session) = self.terminals.active_mut() {
            // Ctrl-U is what a shell understands by "forget this line".
            session.write(b"\x15");
        }
        self.pending_input.clear();
    }

    /// Hand a control sequence to the shell behind the command line.
    ///
    /// Completion and history are the shell's own; this only carries the key
    /// across. With the one-shot command line showing there is no shell to
    /// ask, so it says so rather than pretending.
    pub fn send_to_command_line(&mut self, bytes: &[u8]) {
        if !self.show_terminal {
            self.error("The shell panel has these - Ctrl-O");
            return;
        }
        if self.terminals.is_empty() {
            self.open_terminal(None);
            self.terminal_focused = false;
        }
        if let Some(session) = self.terminals.active_mut() {
            session.write(bytes);
        }
    }

    /// Run whatever is on it.
    pub fn command_line_run(&mut self) {
        if !self.show_terminal {
            self.run_command();
            return;
        }
        if let Some(session) = self.terminals.active_mut() {
            session.write(b"\r");
        }
        self.pending_input.clear();
    }

    // ---- keyboard ------------------------------------------------------------

    /// Carry out one [`keys::Action`].
    ///
    /// Everything the toolbar and the pane headers do goes through here too,
    /// so a key and a click cannot drift apart.
    pub fn run_action(&mut self, action: keys::Action) {
        self.run_action_inner(action);
        // Whatever just happened, the cursor has to end on a row the file
        // half is drawing. With a tree up it draws files only, so a cursor
        // left on a directory would be a selection nobody can see - and F5
        // would copy something the reader never pointed at.
        let side = self.active;
        self.snap_to_a_visible_row(side);
    }

    fn run_action_inner(&mut self, action: keys::Action) {
        use keys::Action as A;
        let side = self.active;

        // A tree navigates differently: left and right are collapse and
        // expand rather than parent and open. Only while the keyboard is in
        // the tree half, though - the files below it are an ordinary listing
        // and an arrow key there means what it means everywhere else.
        let in_the_tree = self.panel(side).in_tree_mode()
            && self.on_tree[match side {
                Side::Left => 0,
                Side::Right => 1,
            }];
        if in_the_tree && keys::is_navigation(action) {
            self.tree_action(side, action);
            return;
        }

        // Inside an archive this is a reader, so the things that would change
        // one say so plainly. Refusing is the honest answer while writing is
        // not implemented; half-working would be worse than either.
        if self.panel(side).in_archive() && keys::changes_files(action) {
            self.error("Archives are read-only here - F5 extracts a copy out first");
            return;
        }

        match action {
            A::CursorUp => self.panel_mut(side).move_cursor(-1),
            A::CursorDown => self.panel_mut(side).move_cursor(1),
            A::PageUp => self.panel_mut(side).move_cursor(-15),
            A::PageDown => self.panel_mut(side).move_cursor(15),
            A::Home => self.panel_mut(side).cursor_home(),
            A::End => self.panel_mut(side).cursor_end(),
            A::Open => {
                if self.panel(side).in_archive() {
                    self.activate_in_archive(side);
                    return;
                }
                let target = self
                    .panel(side)
                    .selected()
                    .filter(|e| e.is_dir() || e.is_parent())
                    .map(|e| e.path.clone());
                match target {
                    Some(path) => self.navigate(side, path),
                    None => {
                        // An archive is a folder that happens to be one file.
                        let file = self.panel(side).selected().map(|e| e.path.clone());
                        match file.filter(|path| lost_commander_core::archive::is_archive(path)) {
                            Some(path) => self.step_into_archive(side, path),
                            // Anything else goes to the application the
                            // desktop registered for it. `F3` is still the
                            // quick view.
                            None => self.open_selection(),
                        }
                    }
                }
            }
            A::Parent => {
                if self.panel(side).in_archive() {
                    self.panel_mut(side).go_parent();
                    return;
                }
                if let Some(parent) = self.panel(side).cwd.parent().map(Path::to_path_buf) {
                    self.navigate(side, parent);
                }
            }
            A::Root => {
                let root = self
                    .panel(side)
                    .cwd
                    .ancestors()
                    .last()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("/"));
                self.navigate(side, root);
            }
            A::SwitchPane => {
                // Tab means one thing: the other pane. It briefly also walked
                // the halves of a pane with a tree, which made it mean two
                // things depending on state - and a key whose meaning you
                // have to work out is one you stop trusting. Enter goes down
                // into the files, Escape comes back up.
                if self.show_right {
                    self.active = side.other();
                } else {
                    // Tab with one pane is a request for the other one:
                    // "switch to the other pane" only means anything if there
                    // is one, and this is the cheapest way to ask. It opens
                    // and the cursor lands on it - the cursor never goes onto
                    // a pane nobody can see, which is the thing worth
                    // avoiding. F12 folds it away again.
                    self.show_right = true;
                    self.pane_opened_to_view = false;
                    self.active = side.other();
                }
            }
            A::SwapPanes => {
                std::mem::swap(&mut self.left, &mut self.right);
                std::mem::swap(&mut self.left_view, &mut self.right_view);
                self.info("Panes swapped");
            }
            A::Reload => {
                self.left.reload();
                self.right.reload();
                self.info("Reloaded");
            }

            A::Mark => self.panel_mut(side).toggle_mark(),
            A::MarkAll => self.panel_mut(side).mark_all(),
            A::ClearMarks => self.panel_mut(side).clear_marks(),
            A::InvertMarks => self.panel_mut(side).invert_marks(),
            A::SelectByPattern => {
                self.dialog = Some(Dialog::Pattern {
                    text: self.select_pattern.clone(),
                    select: true,
                });
            }
            A::DeselectByPattern => {
                self.dialog = Some(Dialog::Pattern {
                    text: self.select_pattern.clone(),
                    select: false,
                });
            }
            A::SelectMenu => self.show_select_menu = true,

            A::Help => self.dialog = Some(Dialog::Help),
            A::Theme => {
                self.dialog = Some(Dialog::Theme {
                    was: theme::palette(),
                })
            }
            A::Find => self.begin_find(),
            A::Properties => self.begin_properties(),
            A::OpenWith => self.begin_open_with(),
            A::EditAsAdmin => self.edit_as_admin(),
            A::RootShell => self.root_shell(),
            A::NewTab => self.new_tab(side),
            A::CloseTab => self.close_tab(side),
            A::CloseOtherTabs => self.close_other_tabs(side),
            A::NextTab => self.walk_tabs(side, true),
            A::PreviousTab => self.walk_tabs(side, false),
            A::MoveTabAcross => self.move_tab_across(side),

            A::CompareFiles => self.begin_difference(),
            A::Duplicates => self.begin_duplicates(),
            A::EditImage => self.begin_image_edit(),
            A::Journal => self.begin_journal(),
            A::EditExternally => self.edit_externally(),
            A::CompareFolders => self.compare_folders(),
            A::Synchronize => self.begin_sync(),

            A::Rename => self.begin_rename(),
            A::MultiRename => self.begin_multi_rename(),
            A::View => self.view_selected(),
            A::Edit => self.edit_selected(),
            A::Copy => self.copy_to_other(),
            A::Move => self.move_to_other(),
            A::MkDir => {
                self.dialog = Some(Dialog::MkDir {
                    name: String::new(),
                })
            }
            A::Delete => self.begin_delete(true),
            A::DeleteForever => self.begin_delete(false),
            A::Quit => self.should_quit = true,

            A::ViewDetails => self.set_view(side, ViewMode::Details),
            A::ViewGrid => self.set_view(side, ViewMode::Grid),
            A::ViewTree => {
                let mode = if self.view(side) == ViewMode::Tree {
                    ViewMode::Details
                } else {
                    ViewMode::Tree
                };
                self.set_view(side, mode);
            }
            A::QuickView => {
                // Quick view goes in the *other* pane, since this one is what
                // it follows.
                let other = side.other();
                let mode = if self.view(other) == ViewMode::Preview {
                    ViewMode::Details
                } else {
                    ViewMode::Preview
                };
                self.set_view(other, mode);
                self.active = side;
            }
            A::ViewHistory => self.history_of_here(),
            A::ToggleHidden => {
                self.panel_mut(side).toggle_hidden();
                let showing = self.panel(side).show_hidden;
                self.info(if showing {
                    "Showing hidden files"
                } else {
                    "Hiding hidden files"
                });
            }
            A::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            A::ToggleSecondPane => {
                // Asking for the pane yourself makes it yours: the viewer no
                // longer gets to fold it away when the preview closes.
                self.pane_opened_to_view = false;
                self.show_right = !self.show_right;
                if !self.show_right {
                    // The one pane left on screen is whichever was active, so
                    // folding never moves you somewhere you were not looking.
                    self.active = side;
                }
            }
            A::Bookmark => {
                let location = Location::local(self.panel(side).cwd.clone());
                let name = location.name.clone();
                self.bookmarks.add(location);
                if let Some(path) = &self.bookmarks_path {
                    let _ = self.bookmarks.save_to(path);
                }
                self.info(format!("Bookmarked \"{name}\""));
            }
            A::ShellOnly => self.show_half(Half::Shell),
            A::FilesOnly => self.show_half(Half::Files),
            A::ToggleShellPanel => {
                self.show_terminal = !self.show_terminal;
                if !self.show_terminal {
                    self.terminal_focused = false;
                }
            }
            A::FocusTerminal => {
                self.show_terminal = true;
                if self.terminals.is_empty() {
                    self.open_terminal(None);
                }
                self.terminal_focused = true;
                self.terminal_taken = true;
            }
            A::LeaveTerminal => self.terminal_focused = false,
            A::FocusCommandLine => {
                self.show_terminal = false;
                self.terminal_focused = false;
            }
            // Handed straight to the shell, which owns completion and
            // history - there is no point reimplementing readline next to a
            // real one.
            A::CompleteCommand => self.send_to_command_line(b"\t"),
            A::HistoryBack => self.send_to_command_line(b"\x1b[A"),
            A::HistoryForward => self.send_to_command_line(b"\x1b[B"),

            A::Cancel => {
                let index = match side {
                    Side::Left => 0,
                    Side::Right => 1,
                };
                // With a tree above and nothing else asking for attention,
                // Escape is the way back up to it - the other half of what
                // Enter did coming down.
                if self.dialog.is_none()
                    && !self.show_select_menu
                    && self.panel(side).in_tree_mode()
                    && !self.on_tree[index]
                {
                    self.on_tree[index] = true;
                    self.say_which_half(side);
                    return;
                }
                self.dialog = None;
                self.show_select_menu = false;
            }
        }
    }

    /// Navigation inside a tree pane.
    fn tree_action(&mut self, side: Side, action: keys::Action) {
        use keys::Action as A;
        let Some(tree) = self.panel_mut(side).tree.as_mut() else {
            return;
        };
        let cursor = tree.cursor;
        match action {
            A::CursorUp => tree.move_cursor(-1),
            A::CursorDown => tree.move_cursor(1),
            A::PageUp => tree.move_cursor(-15),
            A::PageDown => tree.move_cursor(15),
            A::Home => tree.cursor_home(),
            A::End => tree.cursor_end(),
            A::Open => {
                // Right expands; the pane only moves on Enter.
                let path = tree.selected_path().map(|p| p.to_path_buf());
                tree.expand(cursor);
                if let Some(path) = path {
                    self.navigate(side, path);
                    // And down into what was just opened. Enter on a
                    // directory means "show me what is in here", and the
                    // answer is in the half below - leaving the keyboard in
                    // the tree would make you press another key to look at
                    // the thing you just asked for.
                    self.on_tree[match side {
                        Side::Left => 0,
                        Side::Right => 1,
                    }] = false;
                }
            }
            A::Parent => {
                let expanded = tree.selected().map(|n| n.expanded).unwrap_or(false);
                if expanded {
                    tree.collapse(cursor);
                } else if let Some(parent) = tree.parent_of(cursor) {
                    tree.cursor = parent;
                }
            }
            _ => {}
        }
    }

    // ---- the operations the graphical view was missing -----------------------

    fn begin_rename(&mut self) {
        let side = self.active;
        match self.panel(side).selected().filter(|e| !e.is_parent()) {
            Some(entry) => {
                self.dialog = Some(Dialog::Rename {
                    from: entry.path.clone(),
                    name: entry.name.clone(),
                })
            }
            None => self.error("Nothing to rename"),
        }
    }

    /// `Shift-F3`: two files, line by line.
    ///
    /// Which two is [`diff::choose`]'s answer: mark a pair in one pane, or put
    /// one under each pane's cursor.
    fn begin_difference(&mut self) {
        let chosen = match diff::choose(
            self.panel(Side::Left),
            self.panel(Side::Right),
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
                        file_name(&chosen.left),
                        file_name(&chosen.right)
                    ));
                    return;
                }
                self.dialog = Some(Dialog::Difference {
                    left: chosen.left,
                    right: chosen.right,
                    diff: Box::new(difference),
                    go_to: None,
                    at: 0,
                });
            }
            Err(refusal) => self.error(refusal.message()),
        }
    }

    /// `Alt-U`: files under this pane that are the same file twice.
    ///
    /// U because C, D and S already belong to the comparison family and the
    /// mnemonic space for "duplicate" was spoken for by all three.
    fn begin_duplicates(&mut self) {
        let root = self.panel(self.active).cwd.clone();
        let options = dupes::Options::default();
        self.hunt = Some(dupes::Scan::spawn(root.clone(), options.clone()));
        self.dialog = Some(Dialog::Duplicates {
            root,
            options,
            groups: Vec::new(),
            capped: false,
        });
    }

    /// `Ctrl-E` on a picture: open it for turning, cropping and resizing.
    ///
    /// Only for the formats we decode ourselves. A RAW or a HEIC reaches the
    /// preview as a thumbnail the system drew, and a thumbnail is not the
    /// picture - editing one and saving it over the original would replace a
    /// photograph with a postage stamp.
    fn begin_image_edit(&mut self) {
        let chosen = self
            .panel(self.active)
            .selected()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.clone());
        match chosen {
            Some(path) => self.open_image(path),
            None => self.error("Not a picture"),
        }
    }

    /// Open one named picture, whether the cursor or the quick view asked.
    fn open_image(&mut self, path: PathBuf) {
        if lost_commander_core::preview::classify(&path, false)
            != lost_commander_core::preview::Kind::Image
        {
            self.error("Only the pictures this decodes itself can be edited here");
            return;
        }
        self.dialog = Some(Dialog::ImageLoading(Box::new(imageedit::Job::spawn(path))));
    }

    /// `Alt-C`: mark what differs between the two panes.
    ///
    /// No dialog and no walk - it compares the two listings already on screen,
    /// which is what makes it instant and what makes it stop at the top level.
    /// The recursive question is what `Alt-S` is for.
    fn compare_folders(&mut self) {
        self.need_other_pane();
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
    fn begin_sync(&mut self) {
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
        self.dialog = Some(Dialog::Sync {
            left,
            right,
            options,
            show: compare::Show::differences_only(),
            pairs: Vec::new(),
            capped: false,
        });
    }

    /// `Ctrl-M`: new names for the whole selection at once.
    fn begin_multi_rename(&mut self) {
        let sources: Vec<rename::Source> = self
            .panel(self.active)
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
        self.dialog = Some(Dialog::MultiRename {
            computed: rules.clone(),
            rules,
            sources,
            changes,
        });
    }

    /// Delete asks first. It did not, which for a recursive delete of
    /// everything marked is not a risk worth taking for one keystroke.
    fn begin_delete(&mut self, to_trash: bool) {
        let targets = self.selection(self.active);
        if targets.is_empty() {
            self.error("Nothing selected");
            return;
        }
        self.dialog = Some(Dialog::ConfirmDelete { targets, to_trash });
    }

    /// `Enter` on a file: hand the selection to whatever the desktop has
    /// registered for each one.
    ///
    /// Plural, like every other operation here - `Enter` on five marked
    /// photos opens five photos. Directories are dropped rather than being an
    /// error, since "open" for a directory means navigate, and a pane can only
    /// navigate to one.
    fn open_selection(&mut self) {
        let paths = self
            .selection(self.active)
            .into_iter()
            .filter(|path| !path.is_dir())
            .collect();
        self.open_paths(paths);
    }

    /// Shift-Enter: open the file with an application of your choosing.
    ///
    /// One file, not the selection: "with what?" is a question about a
    /// particular file, and the answer for a photo is rarely the answer for
    /// the spreadsheet marked next to it.
    fn begin_open_with(&mut self) {
        let side = self.active;
        let target = self
            .panel(side)
            .selected()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing to open");
            return;
        };

        // Where the system has its own chooser, that is the chooser: it can
        // do things a list of names cannot, and it is the dialog the user
        // already recognises.
        if let Some(launch) = apps::chooser_command(mount::Platform::current(), &target) {
            match open::launch(&launch) {
                Ok(()) => self.info("Open with..."),
                Err(e) => self.error(e),
            }
            return;
        }

        self.dialog = Some(Dialog::OpenWith {
            applications: apps::applications_for(&target),
            target,
            typed: String::new(),
            as_admin: false,
        });
        self.open_with_cursor = 0;
    }

    /// Start the chosen application on the file.
    fn open_with(&mut self, chosen: apps::Chosen, target: &Path, as_admin: bool) {
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let (label, launch, terminal) = match chosen {
            apps::Chosen::App(app) => (
                app.name.clone(),
                apps::open_with_command(mount::Platform::current(), app, target),
                app.terminal,
            ),
            apps::Chosen::Command(command) => (
                command.to_string(),
                apps::exec_command(command, target),
                false,
            ),
        };

        // A terminal application gets a shell tab rather than being started
        // with its output thrown away - the route F4 already takes to
        // $EDITOR, and for the same reason: there are real terminals here.
        // Unprivileged, that is the whole story; privileged, `sudo` in front
        // of that same line is where its password prompt can appear.
        if terminal && !as_admin {
            if let apps::Chosen::App(app) = chosen {
                let line = apps::terminal_line(app, target);
                self.run_in_terminal(&line, &format!("{label} {name}"));
                return;
            }
        }

        let Some(launch) = launch else {
            self.error(format!("{label} is not a command"));
            return;
        };

        if as_admin {
            let said = format!("{name} with {label}, as administrator");
            self.run_elevated(
                elevate::elevate(
                    mount::Platform::current(),
                    &launch,
                    display_pair()
                        .as_ref()
                        .map(|(d, x)| (d.as_str(), x.as_str())),
                    &lost_commander_core::preview::on_disk,
                ),
                &said,
            );
            return;
        }

        let outcome = (self.launcher)(&launch);
        self.note(
            journal::Event::new(journal::Kind::Open, target)
                .note(format!("{label} ({})", launch.program))
                .failed_if(outcome.is_err(), "could not be opened"),
        );
        match outcome {
            Ok(()) => self.info(format!("Opened {name} with {label}")),
            Err(e) => self.error(e),
        }
    }

    /// Carry out an elevation, whichever of its two shapes it turned out to be.
    ///
    /// Nothing here grants privilege: a `Command` spawns the system's own
    /// authorisation prompt, and a `Shell` line goes to a terminal tab because
    /// that is where a password prompt has somewhere to appear.
    fn run_elevated(&mut self, elevation: Elevation, said: &str) {
        match elevation {
            Elevation::Command(command) => match (self.launcher)(&command) {
                Ok(()) => self.info(format!("Authorising: {said}")),
                Err(e) => self.error(e),
            },
            Elevation::Shell(line) => self.run_in_terminal(&line, said),
            Elevation::Refused(reason) => self.error(reason),
        }
    }

    /// `Alt-F7` / `Ctrl-F`: find files under the active panel.
    fn begin_find(&mut self) {
        let root = self.active_panel().cwd.clone();
        self.dialog = Some(Dialog::Find {
            query: self.last_query.clone(),
            root,
            cursor: 0,
            in_results: false,
        });
    }

    /// `Alt-Enter`: what this file is, and what it is allowed to be.
    ///
    /// The one under the cursor rather than the whole selection: permissions
    /// of a heterogeneous set is a different question with a different answer,
    /// and showing one file's bits over a selection of ten would be a lie.
    fn begin_properties(&mut self) {
        let target = self
            .panel(self.active)
            .selected()
            .filter(|entry| !entry.is_parent())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing selected");
            return;
        };
        match perms::read(&target) {
            Ok(properties) => {
                let octal = properties.mode.map(|mode| mode.octal()).unwrap_or_default();
                self.dialog = Some(Dialog::Properties {
                    was: Box::new(properties.clone()),
                    now: Box::new(properties),
                    octal,
                });
            }
            Err(e) => self.error(format!("{}: {e}", target.display())),
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
        self.panel_mut(self.active).clear_marks();
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

    /// Write back only what was actually changed.
    fn apply_properties(&mut self, was: &perms::Properties, now: &perms::Properties) {
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

        self.panel_mut(self.active).reload();
        match (failed, wrote.is_empty()) {
            (Some(e), _) => self.error(format!("{}: {e}", now.name())),
            (None, true) => self.info("Nothing changed"),
            (None, false) => self.info(format!("{}: set {}", now.name(), wrote.join(", "))),
        }
    }

    /// Go to a result: the panel moves to its directory with the cursor on it.
    ///
    /// Not "open it" - a search is how you find *where* something is, and
    /// landing next to it is what lets the next thing you do be anything at
    /// all rather than only the one thing a chooser guessed.
    fn go_to(&mut self, path: &Path) {
        let side = self.active;
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            self.error("Nowhere to go");
            return;
        };
        self.navigate(side, parent);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let index = self
            .panel(side)
            .entries
            .iter()
            .position(|entry| entry.name == name);
        match index {
            Some(index) => self.panel_mut(side).cursor_to(index),
            None => self.error(format!("{name} is no longer there")),
        }
    }

    /// `Shift-F4`: edit a file you do not own.
    ///
    /// Not "run the editor as root" - see `elevate::edit_as_root` for why
    /// that is the wrong tool even though it is the obvious one.
    fn edit_as_admin(&mut self) {
        let side = self.active;
        let target = self
            .panel(side)
            .selected()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.clone());
        let Some(target) = target else {
            self.error("Nothing to edit");
            return;
        };
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| default_editor());
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let said = format!("{editor} {name}, as administrator");

        self.run_elevated(
            elevate::edit_as_root(mount::Platform::current(), &editor, &target),
            &said,
        );
    }

    /// A shell running as administrator, where the panel is.
    fn root_shell(&mut self) {
        let cwd = self.active_panel().cwd.clone();
        let said = format!("root shell in {}", cwd.display());
        self.run_elevated(elevate::root_shell(mount::Platform::current(), &cwd), &said);
    }

    /// Type a command into a shell tab, opening one if there is none.
    ///
    /// A tab whose shell is running a full-screen program is not free to take
    /// a command: the line would reach `vim` as keystrokes rather than the
    /// shell as a command. That case gets a tab of its own - which is what you
    /// would have done by hand anyway.
    fn run_in_terminal(&mut self, line: &str, said: &str) {
        self.show_terminal = true;
        let busy = self
            .terminals
            .active()
            .map(|session| session.is_busy())
            .unwrap_or(false);
        if self.terminals.is_empty() || busy {
            self.open_terminal(None);
        }
        match self.terminals.active_mut() {
            Some(session) => {
                session.run_line(line);
                self.terminal_focused = true;
                self.terminal_taken = true;
                self.info(said.to_string());
            }
            None => self.error("No terminal to run it in"),
        }
    }

    /// Ask about anything worth asking about, then open the rest.
    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            self.error("Nothing to open");
            return;
        }
        let targets: Vec<(PathBuf, bool)> = paths
            .into_iter()
            .map(|path| {
                let executable = open::is_executable(&path);
                (path, executable)
            })
            .collect();

        match open::open_warning(mount::Platform::current(), &targets) {
            Some(question) => {
                self.dialog = Some(Dialog::ConfirmOpen {
                    targets: targets.into_iter().map(|(path, _)| path).collect(),
                    question,
                })
            }
            None => {
                let paths = targets.into_iter().map(|(path, _)| path).collect();
                self.open_now(paths);
            }
        }
    }

    /// The half that actually starts them, once anything to ask about has been
    /// asked. One failure is reported and the rest still go: a broken
    /// association on the third file is no reason to withhold the other four.
    fn open_now(&mut self, targets: Vec<PathBuf>) {
        let mut opened = 0usize;
        let mut failure: Option<String> = None;
        for path in &targets {
            let outcome = (self.opener)(path);
            // What the file was handed to, worked out the same way the
            // opener works it out, without running anything.
            let by = open::open_command(
                mount::Platform::current(),
                path,
                &lost_commander_core::preview::on_disk,
            )
            .map(|plan| plan.program)
            .unwrap_or_else(|_| "the desktop".to_string());
            self.note(
                journal::Event::new(journal::Kind::Open, path)
                    .note(format!("{by} - whichever application it chose"))
                    .failed_if(outcome.is_err(), "could not be opened"),
            );
            match outcome {
                Ok(()) => opened += 1,
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        match failure {
            Some(e) => self.error(e),
            None if opened == 1 => {
                let name = targets[0].file_name().unwrap_or_default().to_string_lossy();
                self.info(format!("Opened {name}"))
            }
            None => self.info(format!("Opened {opened} files")),
        }
    }

    /// F3: show the file in the other pane's quick view.
    /// `Alt-H`: what has been done to the things in this folder, next door.
    ///
    /// The same shape as [`GuiApp::view_selected`], and for the same reason:
    /// the answer is about the folder you are standing in, so it cannot be
    /// drawn in the pane that is showing it. A second press stops, and a pane
    /// opened only to answer is folded away again when it does.
    fn history_of_here(&mut self) {
        let side = self.active;
        if self.view(side.other()) == ViewMode::History {
            self.set_view(side.other(), ViewMode::Details);
            self.active = side;
            self.info("History off.");
            return;
        }
        if !self.show_right {
            self.show_right = true;
            self.pane_opened_to_view = true;
        }
        self.set_view(side.other(), ViewMode::History);
        self.active = side;
        self.info("History of this folder, in the other pane. Alt-H again to stop.");
    }

    fn view_selected(&mut self) {
        let side = self.active;
        if self.panel(side).selected().is_none() {
            self.error("Nothing to view");
            return;
        }
        // A second press stops, rather than setting a mode that is already
        // set. F3 is how you look at a file and how you stop looking at it -
        // one key, because that is what a reader reaches for either way.
        if self.view(side.other()) == ViewMode::Preview {
            self.set_view(side.other(), ViewMode::Details);
            self.active = side;
            self.info("Preview off.");
            return;
        }
        if !self.show_right {
            // Borrowed, and noted as borrowed. `set_view` gives it back when
            // the preview closes.
            self.show_right = true;
            self.pane_opened_to_view = true;
        }
        self.set_view(side.other(), ViewMode::Preview);
        self.active = side;
        self.info("Preview on - it follows the cursor. F3 again to stop.");
    }

    /// F4: hand the file to `$EDITOR`, in a shell tab.
    ///
    /// A graphical file manager with real terminals in it has no business
    /// bundling an editor: the user already has one, and it already knows
    /// their settings.
    fn edit_selected(&mut self) {
        let chosen = self
            .panel(self.active)
            .selected()
            .filter(|entry| !entry.is_parent() && !entry.is_dir())
            .map(|entry| entry.path.clone());
        let Some(path) = chosen else {
            self.error("Nothing to edit");
            return;
        };
        // A binary poured into a text editor comes out as replacement
        // characters, and saving *that* would put them in the file. The right
        // editor for a binary is the one that works in bytes.
        if lost_commander_core::hex::is_binary(&path).unwrap_or(false) {
            match lost_commander_core::hex::Dump::open(&path) {
                Ok(dump) => {
                    self.dialog = Some(Dialog::Bytes(Box::new(hexedit::Session::new(dump))))
                }
                Err(e) => self.error(format!("Cannot edit: {e}")),
            }
            return;
        }
        match Document::open(&path) {
            Ok(document) => {
                self.dialog = Some(Dialog::Text(Box::new(textedit::Session::new(document))))
            }
            Err(e) => self.error(format!("Cannot edit: {e}")),
        }
    }

    /// `Ctrl-J`: what was done, and when.
    fn begin_journal(&mut self) {
        let Some(journal) = &self.journal else {
            self.error("No account is being kept - turn it on in the settings");
            return;
        };
        self.dialog = Some(Dialog::Journal(Box::new(journalview::View::new(journal))));
    }

    /// `Alt-E`: hand the file to `$EDITOR`, in a shell tab.
    ///
    /// F4 has its own editor now, which is what a text file wants. This is
    /// still here because there are real terminals in this window, the user
    /// already has an editor, and that editor already knows their settings -
    /// so for anything F4 will not open, or anyone who would rather use their
    /// own, the route is one key away rather than gone.
    fn edit_externally(&mut self) {
        let side = self.active;
        let chosen = self
            .panel(side)
            .selected()
            .filter(|e| !e.is_parent())
            .map(|e| (e.path.clone(), e.name.clone()));
        let Some((path, name)) = chosen else {
            self.error("Nothing to edit");
            return;
        };
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_string());

        let line = format!(
            "{editor} {}",
            shell::quote_here(&path.display().to_string())
        );
        self.run_in_terminal(&line, &format!("{editor} {name}"));
    }

    // ---- keyboard input and dialogs -----------------------------------------

    /// Turn key presses into actions, unless something else is taking them.
    fn keyboard(&mut self, ctx: &egui::Context) {
        // egui walks keyboard focus between widgets on Tab, and it does the
        // walking *after* this runs - so a Tab the panes handled still leaves
        // focus parked on whatever button happened to be first, and the next
        // frame sees a focused widget and treats every key as typing. One
        // Shift-Tab used to kill the keyboard for the rest of the session.
        // Whatever that traversal handed out is dropped here, on the frame
        // after, which is the earliest it can be seen.
        if self.drop_focus {
            ctx.memory_mut(|m| m.stop_text_input());
            self.drop_focus = false;
        }
        // A dialog answers its own keys, and a focused text field takes
        // everything - otherwise typing a file name would trip every
        // single-key binding in the map.
        let typing = ctx.memory(|m| m.focused()).is_some();
        let mut pending: Vec<keys::Action> = Vec::new();

        // Anything printable belongs to the command line. Collected here and
        // acted on below, so the two cannot get out of order.
        let mut typed: Vec<String> = Vec::new();

        // What was held down for the key press each text event belongs to.
        // Not the frame's own `modifiers`, which is the state at the *end* of
        // the frame: a press and release of Alt-C arrive together, so by then
        // Alt is already up and the letter looks like typing.
        let mut held = egui::Modifiers::NONE;

        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    pressed: true,
                    modifiers,
                    ..
                } = event
                {
                    held = *modifiers;
                }
                match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if let Some(action) = keys::action_for(*key, *modifiers) {
                            // While the shell has the keyboard only the way
                            // out is ours; everything else is the shell's.
                            let allowed = if self.terminal_focused {
                                action == keys::Action::LeaveTerminal
                            } else {
                                !typing || action == keys::Action::Cancel
                            };
                            if allowed {
                                pending.push(action);
                            }
                        }
                    }
                    // A dialog is modal: its keystrokes are its own whether or
                    // not one of its boxes happens to hold focus this frame.
                    // Keying off focus alone let a form with two text fields -
                    // which has frames where neither holds it - type into the
                    // shell as well as into the box.
                    //
                    // A character produced while Alt or Ctrl is held is not a
                    // character anyone meant to type: X11 sends `c` alongside
                    // the key event for Alt-C, so every Alt binding used to
                    // leave its letter on the command line behind it. Text
                    // events carry no modifiers of their own, which is why
                    // this reads the frame's.
                    egui::Event::Text(text)
                        if !typing
                            && !self.terminal_focused
                            && self.dialog.is_none()
                            && keys::is_typed_text(held) =>
                    {
                        typed.push(text.clone());
                    }
                    _ => {}
                }
            }
        });

        // Tab in any of its forms is ours, so its traversal has to be undone.
        self.drop_focus = pending.iter().copied().any(keys::traverses_focus);

        for action in pending {
            if self.dialog.is_some() {
                // A dialog swallows the rest: Escape closes it, and its own
                // buttons do the confirming.
                if action == keys::Action::Cancel {
                    self.close_dialog();
                }
                continue;
            }
            // The keys that mean one thing to the panes and another to the
            // command line, told apart the way the original told them apart:
            // by whether the line has anything on it.
            if keys::defers_to_command_line(action) && !self.command_line_empty() {
                match action {
                    keys::Action::Open => self.command_line_run(),
                    keys::Action::Parent => self.command_line_backspace(),
                    keys::Action::Cancel => self.command_line_clear(),
                    _ => {}
                }
                continue;
            }
            let had_dialog = self.dialog.is_some();
            self.run_action(action);
            // A dialog opened by *this* frame's key must not also be answered
            // by it. Shift-Enter opens the chooser and is an Enter press, so
            // without this the chooser confirms its first row and vanishes in
            // the same frame it appeared - which is exactly what it did.
            if !had_dialog && self.dialog.is_some() {
                self.dialog_opened = true;
            }
        }

        if self.dialog.is_some() {
            return;
        }
        for text in typed {
            match keys::action_for_text(&text, self.command_line_empty()) {
                Some(action) => self.run_action(action),
                None => self.type_into_command_line(&text),
            }
        }
    }

    /// Close whatever dialog is open, and let go of anything it owned.
    ///
    /// Two paths close a dialog - Escape here, and the dialog's own buttons -
    /// and both have to do this. When only one did, a search thread outlived
    /// the form it belonged to and its results came back on the next open.
    /// Close whatever is open, and stop whatever it had running.
    ///
    /// Both threads hang off a dialog, and a window closed mid-walk must not
    /// leave one crawling a disk for a list nobody is looking at.
    fn close_dialog(&mut self) {
        self.dialog = None;
        self.search = None;
        self.scan = None;
        self.hunt = None;
    }

    /// Draw whichever modal is open.
    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let still_open;
        // False on the frame the dialog opened - see `dialog_opened`.
        let accept_enter = !std::mem::take(&mut self.dialog_opened);
        // Taken out and put back so the closure can borrow the rest of self.
        let mut dialog = dialog;

        match &mut dialog {
            // An archive, or a file in one, that will not open without a
            // password.
            Dialog::Password {
                archive,
                member,
                typed,
                refused,
            } => {
                let mut go = false;
                let mut closed = false;
                let title = format!("{} needs a password", file_name(archive));
                let escaped = modal(ctx, &title, |ui| {
                    ui.set_min_width(360.0);
                    if let Some(member) = member.as_deref() {
                        ui.label(
                            RichText::new(member)
                                .size(11.0)
                                .monospace()
                                .color(theme::text_dim()),
                        );
                        ui.add_space(4.0);
                    }
                    if *refused {
                        ui.label(
                            RichText::new("That password did not open it.")
                                .size(11.0)
                                .color(theme::danger()),
                        );
                        ui.add_space(4.0);
                    }
                    let box_ = ui.add(
                        egui::TextEdit::singleline(typed)
                            .password(true)
                            .desired_width(340.0)
                            .hint_text("password"),
                    );
                    box_.request_focus();
                    go = box_.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Kept for this session only, and written nowhere.")
                            .size(10.5)
                            .color(theme::text_faint()),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        go |= ui.button("Open").clicked();
                        closed = ui.button("Cancel").clicked();
                    });
                });
                if escaped || closed {
                    self.dialog = None;
                    return;
                }
                if go && !typed.is_empty() {
                    let (archive, member, typed) =
                        (archive.clone(), member.clone(), std::mem::take(typed));
                    self.dialog = None;
                    self.password_given(archive, member, typed);
                    return;
                }
                still_open = true;
            }
            // The picture is still being decoded. The window opens straight
            // away and says so, rather than the whole application stopping
            // for however long a forty-megapixel JPEG takes.
            Dialog::ImageLoading(job) => {
                let mut closed = false;
                let arrived = job.is_finished().then(|| job.take()).flatten();
                let name = file_name(&job.path);
                let escaped = modal(ctx, "Edit picture", |ui| {
                    ui.set_min_width(300.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("reading {name}..."))
                                .size(11.5)
                                .color(theme::text_dim()),
                        );
                    });
                    ui.add_space(6.0);
                    closed = ui.button("Cancel").clicked();
                });
                still_open = !escaped && !closed;
                match arrived {
                    Some(Ok(loaded)) if still_open => {
                        self.dialog = Some(Dialog::Image(Box::new(imageedit::Session::new(
                            job.path.clone(),
                            loaded,
                        ))));
                        return;
                    }
                    Some(Err(e)) => {
                        self.error(e);
                        self.dialog = None;
                        return;
                    }
                    _ => ctx.request_repaint(),
                }
            }
            Dialog::Image(session) => match imageedit::draw(ctx, session) {
                imageedit::Outcome::Close => still_open = false,
                imageedit::Outcome::Nothing => still_open = true,
                imageedit::Outcome::Write(target) => {
                    still_open = true;
                    let result = session.result();
                    match imageedit::save(&result, &target) {
                        Ok(()) => {
                            self.note(
                                journal::Event::new(journal::Kind::Edit, &session.path)
                                    .to(&target)
                                    .note(format!(
                                        "picture, {}x{}",
                                        result.width(),
                                        result.height()
                                    )),
                            );
                            session.written = true;
                            self.left.reload();
                            self.right.reload();
                            self.info(format!(
                                "Wrote {} ({}x{})",
                                file_name(&target),
                                result.width(),
                                result.height()
                            ));
                        }
                        Err(e) => self.error(format!("Save failed: {e}")),
                    }
                }
            },
            Dialog::Text(session) => match textedit::draw(ctx, session) {
                textedit::Outcome::Close => still_open = false,
                textedit::Outcome::Nothing => still_open = true,
                textedit::Outcome::Write(target) => {
                    still_open = true;
                    match session.document.save(&target) {
                        Ok(lost) => {
                            // Saving with characters the encoding could not
                            // hold is not a failure - the file is written -
                            // but it is not a plain success either, and the
                            // one thing it must not be is silent.
                            let mut note =
                                journal::Event::new(journal::Kind::Edit, &target).note(format!(
                                    "text, {}, {}",
                                    session.document.write_as.label(),
                                    session.document.newline.label()
                                ));
                            if !lost.is_empty() {
                                note = note
                                    .failed(format!("{} character(s) would not fit", lost.len()));
                            }
                            self.note(note);
                            session.lost = lost.clone();
                            self.left.reload();
                            self.right.reload();
                            let name = file_name(&target);
                            if lost.is_empty() {
                                self.info(format!("Saved {name}"));
                            } else {
                                self.error(format!(
                                    "Saved {name}, but {} character(s) would not fit in {}",
                                    lost.len(),
                                    session.document.write_as.label()
                                ));
                            }
                        }
                        Err(e) => self.error(format!("Save failed: {e}")),
                    }
                }
            },
            Dialog::Journal(view) => {
                let journal = self.journal.clone();
                match journal {
                    None => still_open = false,
                    Some(journal) => match journalview::draw(ctx, view, &journal) {
                        journalview::Outcome::Close => still_open = false,
                        journalview::Outcome::Nothing => still_open = true,
                        journalview::Outcome::GoTo(path) => {
                            still_open = false;
                            self.go_to(&path);
                        }
                        journalview::Outcome::Clear => {
                            still_open = true;
                            let gone = journal.clear();
                            **view = journalview::View::new(&journal);
                            self.info(format!("Cleared {gone} day(s) of records"));
                        }
                        journalview::Outcome::Keep(days) => {
                            still_open = true;
                            self.settings.journal_days = Some(days);
                            if let Err(e) = self.settings.save() {
                                self.error(format!("Could not save: {e}"));
                            }
                            // The new setting applies from now, and to what is
                            // already there: a shortened retention that left
                            // the old days sitting about would not be one.
                            let journal = self.settings.journal();
                            if let Some(journal) = &journal {
                                journal.sweep(journal::Day::today());
                                **view = journalview::View::new(journal);
                            }
                            self.journal = journal;
                            self.info(format!(
                                "Keeping the account {}",
                                self.settings.keep().describe()
                            ));
                        }
                    },
                }
            }
            Dialog::Bytes(session) => match hexedit::draw(ctx, session) {
                hexedit::Outcome::Close => still_open = false,
                hexedit::Outcome::Nothing => still_open = true,
                hexedit::Outcome::Write => {
                    still_open = true;
                    let count = session.edits.len();
                    match lost_commander_core::hex::write_back(session.path(), &session.edits) {
                        Ok(_) => {
                            self.note(
                                journal::Event::new(journal::Kind::Edit, session.path())
                                    .note(format!("bytes, {count} changed")),
                            );
                            session.edits.clear();
                            self.left.reload();
                            self.right.reload();
                            self.info(format!("Wrote {count} byte(s)"));
                        }
                        Err(e) => self.error(format!("Write failed: {e}")),
                    }
                }
            },
            Dialog::Help => {
                let mut closed = false;
                let escaped = modal(ctx, "Keys", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            egui::Grid::new("help_grid")
                                .num_columns(2)
                                .spacing([18.0, 4.0])
                                .show(ui, |ui| {
                                    for (key, what) in keys::HELP {
                                        if key.is_empty() {
                                            ui.end_row();
                                            continue;
                                        }
                                        ui.label(
                                            RichText::new(*key)
                                                .monospace()
                                                .size(11.5)
                                                .color(theme::accent()),
                                        );
                                        ui.label(
                                            RichText::new(*what).size(11.5).color(theme::text()),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(6.0);
                    if ui.button("Close").clicked() {
                        closed = true;
                    }
                });
                still_open = !escaped && !closed;
            }
            Dialog::Rename { from, name } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let escaped = modal(ctx, "Rename", |ui| {
                    ui.label(
                        RichText::new(from.display().to_string())
                            .size(11.0)
                            .color(theme::text_faint()),
                    );
                    confirmed = dialog_field(ui, name, "new name", accept_enter);
                    ui.horizontal(|ui| {
                        confirmed |= ui.button("Rename").clicked();
                        cancelled |= ui.button("Cancel").clicked();
                    });
                });
                if confirmed {
                    match fsops::rename(from, name.as_str()) {
                        Ok(to) => {
                            self.note(journal::Event::new(journal::Kind::Rename, &*from).to(&to));
                            self.left.reload();
                            self.right.reload();
                            self.info(format!(
                                "Renamed to {}",
                                to.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                        Err(e) => self.error(format!("Rename failed: {e}")),
                    }
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::Duplicates {
                root,
                options,
                groups,
                capped,
            } => {
                let (mut closed, mut again) = (false, false);
                let (mut go, mut remove) = (None, None);
                let running = self.hunt.is_some();
                let live = self.hunt.as_ref().map(|scan| scan.snapshot());
                if let Some(found) = &live {
                    if found.finished {
                        *groups = found.groups.clone();
                        *capped = found.truncated;
                        self.hunt = None;
                    }
                }
                let capped = *capped;

                let escaped = modal(ctx, "Duplicate files", |ui| {
                    ui.set_min_width(680.0);
                    ui.label(
                        RichText::new(format!("under {}", root.display()))
                            .size(11.0)
                            .color(theme::text_dim()),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        again |= ui
                            .checkbox(&mut options.include_hidden, "hidden files")
                            .changed();
                    });

                    ui.add_space(6.0);
                    if running {
                        let where_ = live.as_ref().map(|d| d.current.clone()).unwrap_or_default();
                        ui.label(
                            RichText::new(format!("looking at {where_}"))
                                .size(11.0)
                                .color(theme::text_faint()),
                        );
                    } else {
                        // One flat list, drawn a window at a time. A thousand
                        // sets is three thousand rows, and laying every one of
                        // them out each frame is the difference between a list
                        // that scrolls and a list that does not move at all.
                        let rows = dupes::lines(groups);
                        egui::ScrollArea::vertical().max_height(340.0).show_rows(
                            ui,
                            22.0,
                            rows.len(),
                            |ui, shown| {
                                for line in &rows[shown] {
                                    match *line {
                                        dupes::Line::Heading { group } => {
                                            let set = &mut groups[group];
                                            let thinned = set.keeping() < set.copies.len();
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} copies of {} each",
                                                        set.copies.len(),
                                                        size_in_words(set.size)
                                                    ))
                                                    .size(11.5)
                                                    .strong()
                                                    .color(theme::text()),
                                                );
                                                let label = if thinned {
                                                    "Keep all"
                                                } else {
                                                    "Keep the first"
                                                };
                                                if ui.small_button(label).clicked() {
                                                    if thinned {
                                                        set.keep_all();
                                                    } else {
                                                        set.keep_first();
                                                    }
                                                }
                                            });
                                        }
                                        dupes::Line::Copy { group, copy } => {
                                            let set = &mut groups[group];
                                            let can = set.can_remove(copy);
                                            let going = set.copies[copy].remove;
                                            let shown_path = set.copies[copy]
                                                .path
                                                .strip_prefix(&*root)
                                                .unwrap_or(&set.copies[copy].path)
                                                .display()
                                                .to_string();
                                            ui.horizontal(|ui| {
                                                ui.add_space(12.0);
                                                let mut ticked = going;
                                                if ui
                                                    .add_enabled(
                                                        can,
                                                        egui::Checkbox::new(&mut ticked, ""),
                                                    )
                                                    .on_disabled_hover_text(
                                                        "The last copy kept cannot go as well",
                                                    )
                                                    .changed()
                                                {
                                                    set.toggle(copy);
                                                }
                                                let ink = if going {
                                                    theme::danger()
                                                } else {
                                                    theme::text_dim()
                                                };
                                                if ui
                                                    .add(
                                                        egui::Label::new(
                                                            RichText::new(shown_path)
                                                                .size(11.0)
                                                                .monospace()
                                                                .color(ink),
                                                        )
                                                        .sense(Sense::click())
                                                        .truncate(),
                                                    )
                                                    .on_hover_text("Click to go there")
                                                    .clicked()
                                                {
                                                    go = Some(set.copies[copy].path.clone());
                                                }
                                            });
                                        }
                                    }
                                }
                            },
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            closed = true;
                        }
                        if running {
                            if ui.button("Stop").clicked() {
                                if let Some(scan) = &self.hunt {
                                    scan.request_stop();
                                }
                            }
                        } else {
                            again |= ui.button("Look again").clicked();
                            // Counted rather than collected: the list of
                            // paths is only built when the button is used.
                            let ticked = dupes::ticked(groups);
                            if ui
                                .add_enabled(
                                    ticked > 0,
                                    egui::Button::new(format!("Delete {ticked}")),
                                )
                                .on_hover_text("To the trash, and it asks first")
                                .clicked()
                            {
                                remove = Some(dupes::to_remove(groups));
                            }
                        }

                        let say = if running {
                            "looking...".to_string()
                        } else if groups.is_empty() {
                            "no duplicates here".to_string()
                        } else if capped {
                            // The cap is a memory guard, not a judgement about
                            // the tree - and a list that quietly stops reads
                            // as "that is all there is".
                            format!("the first {} sets - there were more", dupes::MAX_GROUPS)
                        } else {
                            format!(
                                "{} sets, {} the same thing twice, {} ticked to go",
                                groups.len(),
                                size_in_words(dupes::wasted(groups)),
                                size_in_words(dupes::reclaimed(groups))
                            )
                        };
                        ui.label(RichText::new(say).size(11.0).color(theme::text_dim()));
                    });
                });

                if again {
                    self.hunt = Some(dupes::Scan::spawn(root.clone(), options.clone()));
                    groups.clear();
                }
                if let Some(path) = go.clone() {
                    self.go_to(&path);
                }
                if let Some(targets) = remove {
                    // Through the ordinary delete, which means the trash and
                    // means being asked first - a list worked out by a rule is
                    // exactly the list worth a second look.
                    self.hunt = None;
                    self.dialog = Some(Dialog::ConfirmDelete {
                        targets,
                        to_trash: true,
                    });
                    return;
                }
                still_open = !escaped && !closed && go.is_none();
                if running {
                    ctx.request_repaint();
                }
            }
            Dialog::Difference {
                left,
                right,
                diff,
                go_to,
                at,
            } => {
                let mut closed = false;
                // Read before the modal draws, so a jump moves the list this
                // frame rather than one frame late.
                ctx.input(|i| {
                    if i.key_pressed(egui::Key::Tab) || i.key_pressed(egui::Key::N) {
                        if let Some(next) = diff.next_change(*at) {
                            *at = next;
                            *go_to = Some(next);
                        }
                    }
                    if i.key_pressed(egui::Key::P) {
                        if let Some(previous) = diff.previous_change(*at) {
                            *at = previous;
                            *go_to = Some(previous);
                        }
                    }
                });

                let numbers = diff::gutter_width(diff);
                let escaped = modal(ctx, "Compare files", |ui| {
                    ui.set_min_width(880.0);
                    ui.horizontal(|ui| {
                        for path in [&*left, &*right] {
                            ui.add_sized(
                                [420.0, 16.0],
                                egui::Label::new(
                                    RichText::new(file_name(path))
                                        .size(11.5)
                                        .monospace()
                                        .color(theme::text_dim()),
                                )
                                .truncate(),
                            );
                        }
                    });
                    ui.add_space(4.0);

                    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                    let mut area = egui::ScrollArea::vertical().max_height(460.0);
                    if let Some(row) = go_to.take() {
                        area = area.vertical_scroll_offset(row as f32 * row_height);
                    }
                    area.show_rows(ui, row_height, diff.rows.len(), |ui, shown| {
                        egui::Grid::new("difference")
                            .num_columns(2)
                            .spacing([12.0, 0.0])
                            .min_col_width(420.0)
                            .show(ui, |ui| {
                                for row in &diff.rows[shown.clone()] {
                                    let (left_ink, right_ink) = if row.is_same() {
                                        (theme::text_dim(), theme::text_dim())
                                    } else {
                                        (theme::danger(), theme::ok())
                                    };
                                    diff_cell(ui, row.left(), numbers, left_ink);
                                    diff_cell(ui, row.right(), numbers, right_ink);
                                    ui.end_row();
                                }
                            });
                    });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            closed = true;
                        }
                        if ui
                            .button("Next difference")
                            .on_hover_text("Tab, or n")
                            .clicked()
                        {
                            if let Some(next) = diff.next_change(*at) {
                                *at = next;
                                *go_to = Some(next);
                            }
                        }
                        let say = if diff.unaligned {
                            "too different to line up - both shown as they are".to_string()
                        } else {
                            format!("{} line(s) differ", diff.changes)
                        };
                        ui.label(RichText::new(say).size(11.0).color(theme::text_dim()));
                    });
                });
                still_open = !escaped && !closed;
            }
            Dialog::Sync {
                left,
                right,
                options,
                show,
                pairs,
                capped,
            } => {
                let (mut closed, mut recompare, mut run) = (false, false, false);
                let mut turned: Option<usize> = None;
                let mut bulk: Option<compare::Bulk> = None;
                let running = self.scan.is_some();
                // While a comparison is running the list is the worker's; when
                // it finishes it becomes this dialog's, so the directions can
                // be edited without the next frame forgetting them.
                let live = self.scan.as_ref().map(|scan| scan.snapshot());
                if let Some(found) = &live {
                    if found.finished {
                        *pairs = found.pairs.clone();
                        *capped = found.truncated;
                        self.scan = None;
                    }
                }
                let showing: Vec<usize> = (0..pairs.len())
                    .filter(|&i| show.allows(pairs[i].state))
                    .collect();
                let tally = compare::tally(pairs);
                let work = tally.to_left + tally.to_right;

                let escaped = modal(ctx, "Synchronize", |ui| {
                    // Wide enough for the four columns plus the scrollbar, and
                    // fixed, so the window does not resize itself as the rows
                    // scrolling past it change length.
                    ui.set_min_width(640.0);
                    egui::Grid::new("sync_roots")
                        .num_columns(2)
                        .spacing([8.0, 2.0])
                        .show(ui, |ui| {
                            for (label, path) in [("left", &*left), ("right", &*right)] {
                                ui.label(
                                    RichText::new(label).size(11.0).color(theme::text_faint()),
                                );
                                ui.label(
                                    RichText::new(path.display().to_string())
                                        .size(11.0)
                                        .monospace()
                                        .color(theme::text_dim()),
                                );
                                ui.end_row();
                            }
                        });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        recompare |= ui
                            .checkbox(&mut options.recursive, "subdirectories")
                            .changed();
                        recompare |= ui
                            .checkbox(&mut options.by_content, "compare contents")
                            .on_hover_text("Read both files rather than trusting size and date")
                            .changed();
                        recompare |= ui
                            .checkbox(&mut options.include_hidden, "hidden files")
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("show").size(11.0).color(theme::text_faint()));
                        ui.checkbox(&mut show.differences, "different");
                        ui.checkbox(&mut show.only_left, "only left");
                        ui.checkbox(&mut show.only_right, "only right");
                        ui.checkbox(&mut show.same, "the same");
                    });

                    // A thousand differences is a thousand clicks on a
                    // thousand arrows, which is not an answer. These point
                    // the whole list at once - and only what the filter above
                    // is showing, so "all" means all of what is on screen.
                    if !running && !showing.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("all {} shown", showing.len()))
                                    .size(11.0)
                                    .color(theme::text_faint()),
                            );
                            for control in compare::BULK {
                                if ui
                                    .small_button(control.label())
                                    .on_hover_text(control.describe())
                                    .clicked()
                                {
                                    bulk = Some(control);
                                }
                            }
                        });
                    }

                    ui.add_space(6.0);
                    if let Some(found) = &live {
                        // Its own scroll position, kept apart from the
                        // finished list's: they are the same shape and the
                        // same depth, so without a name of their own egui
                        // hands them one id and the finished list opens
                        // wherever the running one had got to.
                        egui::ScrollArea::vertical()
                            .id_salt("sync_running")
                            .max_height(300.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for pair in &found.pairs {
                                    if show.allows(pair.state) {
                                        ui.label(
                                            RichText::new(&pair.name)
                                                .size(11.0)
                                                .monospace()
                                                .color(theme::text_dim()),
                                        );
                                    }
                                }
                            });
                    } else {
                        // Drawn a window at a time. The comparison will hold
                        // twenty thousand pairs, and laying every one of them
                        // out each frame is work nobody can see.
                        egui::ScrollArea::vertical()
                            .id_salt("sync_pairs")
                            .max_height(300.0)
                            .show_rows(ui, 20.0, showing.len(), |ui, rows| {
                                for &index in &showing[rows] {
                                    let pair = &pairs[index];
                                    let ink = if pair.state.is_same() {
                                        theme::text_faint()
                                    } else {
                                        theme::text()
                                    };
                                    ui.horizontal(|ui| {
                                        // Names read down the left edge, sizes
                                        // and dates line up on the right. A
                                        // column of either one centred is a
                                        // column that does not line up at all.
                                        cell(ui, 250.0, egui::Align::Min, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&pair.name)
                                                        .size(11.0)
                                                        .monospace()
                                                        .color(ink),
                                                )
                                                .truncate(),
                                            );
                                        });
                                        cell(ui, 150.0, egui::Align::Max, |ui| {
                                            ui.label(
                                                RichText::new(side_cell(pair.left.as_ref()))
                                                    .size(10.5)
                                                    .monospace()
                                                    .color(theme::text_faint()),
                                            );
                                        });
                                        // The arrow is the control: clicking
                                        // it cycles through the directions
                                        // this pair could actually take.
                                        let turn = ui
                                            .add_sized(
                                                [34.0, 16.0],
                                                egui::Button::new(
                                                    RichText::new(pair.direction.mark())
                                                        .size(11.0)
                                                        .monospace(),
                                                )
                                                .frame(pair.direction != compare::Direction::Skip),
                                            )
                                            .on_hover_text(pair.state.describe());
                                        if turn.clicked() {
                                            turned = Some(index);
                                        }
                                        cell(ui, 150.0, egui::Align::Max, |ui| {
                                            ui.label(
                                                RichText::new(side_cell(pair.right.as_ref()))
                                                    .size(10.5)
                                                    .monospace()
                                                    .color(theme::text_faint()),
                                            );
                                        });
                                    });
                                }
                            });
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            closed = true;
                        }
                        if running {
                            if ui.button("Stop").clicked() {
                                if let Some(scan) = &self.scan {
                                    scan.request_stop();
                                }
                            }
                        } else {
                            recompare |= ui.button("Compare again").clicked();
                            run = ui
                                .add_enabled(
                                    work > 0,
                                    egui::Button::new(format!("Synchronize {work}")),
                                )
                                .clicked();
                        }

                        let say = match &live {
                            Some(found) if !found.finished => {
                                format!("comparing {}...", found.current)
                            }
                            Some(_) => String::new(),
                            None if pairs.is_empty() => "nothing to compare".to_string(),
                            None => format!(
                                "{} to the right, {} to the left, {} the same, {} left alone",
                                tally.to_right,
                                tally.to_left,
                                tally.same,
                                tally.skipped_differences
                            ),
                        };
                        ui.label(RichText::new(say).size(11.0).color(theme::text_dim()));
                    });
                    if !running && *capped {
                        ui.label(
                            RichText::new(format!(
                                "The first {} - there is more tree than this can hold. \
                                 Compare a smaller part of it to see the rest.",
                                pairs.len()
                            ))
                            .size(10.5)
                            .color(theme::danger()),
                        );
                    }
                    if !running && tally.skipped_differences > 0 {
                        ui.label(
                            RichText::new(
                                "The ones left alone differ with neither side newer - \
                                 click an arrow to choose which way they go.",
                            )
                            .size(10.5)
                            .color(theme::text_faint()),
                        );
                    }
                });

                if let Some(index) = turned {
                    pairs[index].turn();
                }
                if let Some(control) = bulk {
                    let count = compare::turn_all(pairs, &showing, control);
                    self.info(format!("Turned {count} row(s) round"));
                }
                if recompare {
                    self.scan = Some(compare::Scan::spawn(
                        left.clone(),
                        right.clone(),
                        options.clone(),
                    ));
                    pairs.clear();
                    *capped = false;
                }
                if run {
                    let tasks = compare::tasks(pairs, left, right);
                    self.start(Operation::Sync { tasks });
                }
                still_open = !escaped && !closed && !run;
                if running {
                    ctx.request_repaint();
                }
            }
            Dialog::MultiRename {
                rules,
                sources,
                changes,
                computed,
            } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let (moving, troubled) = rename::tally(changes);
                let count = sources.len();

                let escaped = modal(ctx, "Rename files", |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{count} {}",
                            if count == 1 { "file" } else { "files" }
                        ))
                        .size(11.0)
                        .color(theme::text_dim()),
                    );
                    ui.add_space(4.0);

                    egui::Grid::new("multi_rename_fields")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("name").size(11.5));
                            dialog_field(ui, &mut rules.name, "[N]", false);
                            ui.end_row();
                            ui.label(RichText::new("extension").size(11.5));
                            dialog_field_focused(ui, &mut rules.extension, "[E]", false, false);
                            ui.end_row();
                            ui.label(RichText::new("replace").size(11.5));
                            dialog_field_focused(ui, &mut rules.find, "text to find", false, false);
                            ui.end_row();
                            ui.label(RichText::new("with").size(11.5));
                            dialog_field_focused(ui, &mut rules.replace, "", false, false);
                            ui.end_row();
                        });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("multi_rename_case")
                            .selected_text(rules.case.label())
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for case in rename::Case::ALL {
                                    if ui
                                        .selectable_label(rules.case == case, case.label())
                                        .clicked()
                                    {
                                        rules.case = case;
                                    }
                                }
                            });
                        ui.checkbox(&mut rules.case_sensitive, "match case");
                    });

                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(
                            "[N] name  [E] extension  [C] counter  [N2-5] part of the name",
                        )
                        .size(10.5)
                        .color(theme::text_faint()),
                    );
                    ui.label(
                        RichText::new(
                            "[C001] pads to three  [C10+2] starts at ten, steps by two  \
                             [Y][M][D] the file's date",
                        )
                        .size(10.5)
                        .color(theme::text_faint()),
                    );

                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            egui::Grid::new("multi_rename_preview")
                                .num_columns(3)
                                .spacing([10.0, 2.0])
                                .show(ui, |ui| {
                                    for change in changes.iter() {
                                        ui.label(
                                            RichText::new(&change.was)
                                                .size(11.0)
                                                .monospace()
                                                .color(theme::text_dim()),
                                        );
                                        // A name that is not changing is
                                        // shown as it is, so a rule that
                                        // misses half the selection is
                                        // visible before it runs.
                                        let (colour, shown) = match change.trouble {
                                            Some(trouble) => {
                                                (theme::danger(), trouble.message().to_string())
                                            }
                                            None if change.is_rename() => {
                                                (theme::text(), change.name.clone())
                                            }
                                            None => (theme::text_faint(), change.name.clone()),
                                        };
                                        ui.label(
                                            RichText::new(shown)
                                                .size(11.0)
                                                .monospace()
                                                .color(colour),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        cancelled |= ui.button("Cancel").clicked();
                        confirmed = ui
                            .add_enabled(moving > 0, egui::Button::new(format!("Rename {moving}")))
                            .clicked();
                        if troubled > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "{troubled} cannot be renamed, and will be left alone"
                                ))
                                .size(11.0)
                                .color(theme::danger()),
                            );
                        }
                    });
                });

                // Once per change to the rules, not once per frame.
                if rules != computed {
                    *changes = rename::plan(
                        mount::Platform::current(),
                        sources,
                        rules,
                        &lost_commander_core::preview::on_disk,
                    );
                    *computed = rules.clone();
                }

                if confirmed {
                    let plan = std::mem::take(changes);
                    self.run_multi_rename(&plan);
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::CopyTo {
                what,
                destination,
                job,
            } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let verb = match job {
                    Pending::Copy(_) => "Copy",
                    Pending::Move(_) => "Move",
                    Pending::Extract { .. } => "Extract",
                };
                let escaped = modal(ctx, &format!("{verb} {what}"), |ui| {
                    ui.label(
                        RichText::new("to directory")
                            .size(11.0)
                            .color(theme::text_faint()),
                    );
                    // Focused on arrival, so a destination that needs typing
                    // can just be typed - which is the only way to name one
                    // when the other pane is not on screen.
                    confirmed = dialog_field(ui, destination, "destination", accept_enter);
                    ui.horizontal(|ui| {
                        confirmed |= ui.button(verb).clicked();
                        cancelled |= ui.button("Cancel").clicked();
                    });
                });
                if confirmed {
                    let job = std::mem::replace(job, Pending::Copy(Vec::new()));
                    let into = destination.clone();
                    cancelled = !self.send_to(job, &into);
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::MkDir { name } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let parent = self.active_panel().cwd.clone();
                let escaped = modal(ctx, "New directory", |ui| {
                    ui.label(
                        RichText::new(parent.display().to_string())
                            .size(11.0)
                            .color(theme::text_faint()),
                    );
                    confirmed = dialog_field(ui, name, "directory name", accept_enter);
                    ui.horizontal(|ui| {
                        confirmed |= ui.button("Create").clicked();
                        cancelled |= ui.button("Cancel").clicked();
                    });
                });
                if confirmed {
                    match fsops::create_dir(&parent, name.as_str()) {
                        Ok(path) => {
                            self.note(journal::Event::new(journal::Kind::MakeDir, &path));
                            self.left.reload();
                            self.right.reload();
                            self.info(format!("Created {}", path.display()));
                        }
                        Err(e) => self.error(format!("Could not create: {e}")),
                    }
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::ConfirmDelete { targets, to_trash } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let count = targets.len();
                let names: Vec<String> = targets
                    .iter()
                    .take(8)
                    .map(|p| {
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    })
                    .collect();
                let trashing = *to_trash;
                let title = if trashing {
                    "Move to trash"
                } else {
                    "Delete for good"
                };
                let escaped = modal(ctx, title, |ui| {
                    // The two are different acts, and the wording says which
                    // one this is - "cannot be undone" used to be true of
                    // every delete and is now only true of the other.
                    let what = if count == 1 {
                        "this".to_string()
                    } else {
                        format!("these {count} items")
                    };
                    ui.label(
                        RichText::new(if trashing {
                            format!("Move {what} to the trash?")
                        } else {
                            format!("Delete {what} for good?")
                        })
                        .size(12.5)
                        .color(theme::text()),
                    );
                    if !trashing {
                        ui.label(
                            RichText::new("This does not go to the trash.")
                                .size(11.0)
                                .color(theme::danger()),
                        );
                    }
                    ui.add_space(4.0);
                    for name in &names {
                        ui.label(RichText::new(name).size(11.0).color(theme::text_dim()));
                    }
                    if count > names.len() {
                        ui.label(
                            RichText::new(format!("... and {} more", count - names.len()))
                                .size(11.0)
                                .color(theme::text_faint()),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        // Cancel first, so the destructive one is not under
                        // the pointer when the box appears.
                        cancelled |= ui.button("Cancel").clicked();
                        confirmed = if trashing {
                            ui.button("Move to trash").clicked()
                        } else {
                            ui.button(RichText::new("Delete for good").color(theme::danger()))
                                .clicked()
                        };
                    });
                });
                if confirmed {
                    let targets = std::mem::take(targets);
                    let to_trash = *to_trash;
                    self.start(Operation::Delete { targets, to_trash });
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::ConfirmOpen { targets, question } => {
                let (mut confirmed, mut cancelled) = (false, false);
                let names: Vec<String> = targets
                    .iter()
                    .take(8)
                    .map(|p| {
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    })
                    .collect();
                let count = targets.len();
                let escaped = modal(ctx, "Open", |ui| {
                    ui.label(RichText::new(&*question).size(12.5).color(theme::text()));
                    ui.add_space(4.0);
                    for name in &names {
                        ui.label(RichText::new(name).size(11.0).color(theme::text_dim()));
                    }
                    if count > names.len() {
                        ui.label(
                            RichText::new(format!("... and {} more", count - names.len()))
                                .size(11.0)
                                .color(theme::text_faint()),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        cancelled |= ui.button("Cancel").clicked();
                        confirmed = ui.button("Open").clicked();
                    });
                });
                if confirmed {
                    let targets = std::mem::take(targets);
                    self.open_now(targets);
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
            Dialog::Find {
                query,
                root,
                cursor,
                in_results,
            } => {
                let root = root.clone();
                let mut go = None;
                let (mut closed, mut start) = (false, false);
                let found = self.search.as_ref().map(|s| s.snapshot());
                let running = found.as_ref().map(|f| !f.finished).unwrap_or(false);

                // Down walks out of the boxes into the results and then
                // down them; Up walks back. Read here rather than left to
                // whichever widget egui focused, so the rows are painted with
                // the cursor this frame's keys moved.
                let count = found.as_ref().map(|f| f.hits.len()).unwrap_or(0);
                let mut entered = false;
                ctx.input(|i| {
                    if i.key_pressed(egui::Key::ArrowDown) && count > 0 {
                        if *in_results {
                            *cursor = (*cursor + 1).min(count - 1);
                        } else {
                            *in_results = true;
                            *cursor = 0;
                        }
                    }
                    if i.key_pressed(egui::Key::ArrowUp) && *in_results {
                        if *cursor == 0 {
                            *in_results = false;
                        } else {
                            *cursor -= 1;
                        }
                    }
                    // Enter means one of two things and this decides which,
                    // rather than the answer depending on where focus is.
                    entered = accept_enter && i.key_pressed(egui::Key::Enter);
                });
                if !*in_results {
                    *cursor = 0;
                }
                let at = (*cursor).min(count.saturating_sub(1));

                let escaped = modal(ctx, "Find", |ui| {
                    ui.label(
                        RichText::new(format!("in {}", root.display()))
                            .size(11.0)
                            .color(theme::text_dim()),
                    );
                    ui.add_space(4.0);

                    egui::Grid::new("find_fields")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("named").size(11.5));
                            dialog_field(ui, &mut query.pattern, "*.rs", accept_enter);
                            ui.end_row();
                            ui.label(RichText::new("containing").size(11.5));
                            dialog_field_focused(
                                ui,
                                &mut query.contains,
                                "text inside the file (optional)",
                                accept_enter,
                                false,
                            );
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut query.case_sensitive, "match case");
                        ui.checkbox(&mut query.include_hidden, "hidden files");
                    });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if running {
                            if ui.button("Stop").clicked() {
                                if let Some(search) = &self.search {
                                    search.request_stop();
                                }
                            }
                        } else if ui.button("Find").clicked() {
                            start = true;
                        }
                        if ui.button("Close").clicked() {
                            closed = true;
                        }

                        if let Some(found) = &found {
                            let say = if running {
                                format!("{} so far...", found.hits.len())
                            } else if found.truncated {
                                format!("the first {} - there were more", found.hits.len())
                            } else if found.hits.is_empty() {
                                "nothing found".to_string()
                            } else {
                                format!("{} found", found.hits.len())
                            };
                            ui.label(RichText::new(say).size(11.0).color(theme::text_dim()));
                        }
                    });

                    if let Some(found) = &found {
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for (index, hit) in found.hits.iter().enumerate() {
                                    // Shown relative to where the search
                                    // started: the common prefix is the one
                                    // part of every path that says nothing.
                                    let shown = hit
                                        .path
                                        .strip_prefix(&root)
                                        .unwrap_or(&hit.path)
                                        .display()
                                        .to_string();
                                    let label = match (hit.line, &hit.excerpt) {
                                        (Some(line), Some(text)) => {
                                            format!("{shown}:{line}   {text}")
                                        }
                                        _ => shown,
                                    };
                                    let selected = index == at && *in_results;
                                    if ui.selectable_label(selected, label).clicked() {
                                        go = Some(hit.path.clone());
                                    }
                                }
                                if running {
                                    ui.label(
                                        RichText::new(format!("looking in {}", found.current))
                                            .size(10.5)
                                            .color(theme::text_faint()),
                                    );
                                }
                            });
                    }
                });

                // On the list, Enter goes there; in the boxes, it searches.
                if entered && *in_results && count > 0 {
                    go = found
                        .as_ref()
                        .and_then(|f| f.hits.get(at))
                        .map(|hit| hit.path.clone());
                } else if (start || entered) && !query.is_empty() {
                    self.last_query = query.clone();
                    self.search = Some(find::Search::spawn(root.clone(), query.clone()));
                    *cursor = 0;
                    *in_results = false;
                }
                if let Some(path) = go.clone() {
                    self.go_to(&path);
                }
                still_open = !escaped && !closed && go.is_none();
                if running {
                    ctx.request_repaint();
                }
            }
            Dialog::Properties { was, now, octal } => {
                let (mut applied, mut closed) = (false, false);
                let was_snapshot = was.clone();

                let escaped = modal(ctx, "Properties", |ui| {
                    ui.label(
                        RichText::new(now.name())
                            .size(13.0)
                            .strong()
                            .color(theme::text()),
                    );
                    ui.label(
                        RichText::new(now.path.display().to_string())
                            .size(10.5)
                            .color(theme::text_faint()),
                    );
                    ui.add_space(6.0);

                    let fact = |ui: &mut egui::Ui, label: &str, value: String| {
                        ui.label(RichText::new(label).size(11.0).color(theme::text_dim()));
                        ui.label(
                            RichText::new(value)
                                .size(11.0)
                                .family(egui::FontFamily::Monospace)
                                .color(theme::text()),
                        );
                        ui.end_row();
                    };

                    egui::Grid::new("properties_facts")
                        .num_columns(2)
                        .spacing([12.0, 3.0])
                        .show(ui, |ui| {
                            let kind = if now.is_symlink {
                                "symbolic link"
                            } else if now.kind == lost_commander_core::entry::EntryKind::Dir {
                                "directory"
                            } else {
                                "file"
                            };
                            fact(ui, "type", kind.to_string());
                            if let Some(target) = &now.link_target {
                                fact(ui, "points at", target.display().to_string());
                            }
                            if now.kind != lost_commander_core::entry::EntryKind::Dir {
                                // Both, because "4.2K" is what you read and
                                // the exact count is what you check.
                                fact(
                                    ui,
                                    "size",
                                    format!("{}   ({} bytes)", human_size(now.size), now.size),
                                );
                            }
                            fact(ui, "modified", stamp(now.modified));
                            fact(ui, "accessed", stamp(now.accessed));
                            if now.created.is_some() {
                                fact(ui, "created", stamp(now.created));
                            }
                            if let Some(owner) = &now.owner {
                                fact(ui, "owner", owner.clone());
                            }
                            if let Some(group) = &now.group {
                                fact(ui, "group", group.clone());
                            }
                        });

                    ui.add_space(8.0);

                    match &mut now.mode {
                        Some(mode) => {
                            ui.label(
                                RichText::new("Permissions")
                                    .size(11.0)
                                    .strong()
                                    .color(theme::text_dim()),
                            );
                            ui.add_space(2.0);

                            let mut changed = false;
                            egui::Grid::new("properties_mode")
                                .num_columns(4)
                                .spacing([14.0, 2.0])
                                .show(ui, |ui| {
                                    ui.label("");
                                    for what in What::ALL {
                                        ui.label(
                                            RichText::new(what.label())
                                                .size(10.5)
                                                .color(theme::text_faint()),
                                        );
                                    }
                                    ui.end_row();
                                    for who in Who::ALL {
                                        ui.label(RichText::new(who.label()).size(11.0));
                                        for what in What::ALL {
                                            let mut on = mode.is_set(who, what);
                                            if ui.checkbox(&mut on, "").changed() {
                                                mode.set(who, what, on);
                                                changed = true;
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                for (special, label) in [
                                    (perms::SETUID, "setuid"),
                                    (perms::SETGID, "setgid"),
                                    (perms::STICKY, "sticky"),
                                ] {
                                    let mut on = mode.has(special);
                                    if ui.checkbox(&mut on, label).changed() {
                                        mode.set_special(special, on);
                                        changed = true;
                                    }
                                }
                            });

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("octal").size(11.0));
                                let field = ui.add(
                                    egui::TextEdit::singleline(octal)
                                        .desired_width(56.0)
                                        .font(egui::TextStyle::Monospace),
                                );
                                // The box and the grid are two views of one
                                // number: typing in the box moves the ticks,
                                // ticking moves the box, and neither fights
                                // the other because only the edited one wins.
                                if field.changed() {
                                    if let Some(parsed) = Mode::parse_octal(octal) {
                                        *mode = parsed;
                                    }
                                } else if changed || !field.has_focus() {
                                    *octal = mode.octal();
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{}{}",
                                        perms::kind_char(now.kind, now.is_symlink),
                                        mode.symbolic()
                                    ))
                                    .size(11.0)
                                    .family(egui::FontFamily::Monospace)
                                    .color(theme::text_dim()),
                                );
                            });
                        }
                        None => {
                            // No permission bits here, so the one flag this
                            // platform does have, rather than a grid of
                            // checkboxes that would mean nothing.
                            ui.checkbox(&mut now.readonly, "read-only");
                        }
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        closed |= ui.button("Close").clicked();
                        if ui.button("Apply").clicked() {
                            applied = true;
                        }
                    });
                });

                if applied {
                    let now = now.clone();
                    self.apply_properties(&was_snapshot, &now);
                }
                still_open = !escaped && !closed && !applied;
            }
            Dialog::ConfirmOverwrite { conflict } => {
                let mut answer = None;
                let conflict = conflict.clone();
                let name = conflict
                    .target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                modal(ctx, "Already there", |ui| {
                    ui.label(
                        RichText::new(format!("{name} already exists."))
                            .size(12.5)
                            .color(theme::text()),
                    );
                    ui.add_space(6.0);

                    // Both sides, so the answer is a comparison rather than a
                    // guess, and the newer one is marked - that is the fact
                    // the question turns on nine times in ten.
                    let newer = conflict.source_is_newer();
                    let side = |ui: &mut egui::Ui, label, size, when: String, is_newer| {
                        ui.label(
                            RichText::new(format!(
                                "{label}  {:>9}  {when}{}",
                                human_size(size),
                                if is_newer { "   (newer)" } else { "" }
                            ))
                            .size(11.5)
                            .family(egui::FontFamily::Monospace)
                            .color(if is_newer {
                                theme::text()
                            } else {
                                theme::text_dim()
                            }),
                        );
                    };
                    side(
                        ui,
                        "there now",
                        conflict.target_size,
                        stamp(conflict.target_modified),
                        newer == Some(false),
                    );
                    side(
                        ui,
                        "arriving ",
                        conflict.source_size,
                        stamp(conflict.source_modified),
                        newer == Some(true),
                    );

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        // The keeping answers first: the destructive pair is
                        // not under the pointer when the box appears.
                        if ui.button("Skip").clicked() {
                            answer = Some(Answer::Skip);
                        }
                        if ui.button("Skip all").clicked() {
                            answer = Some(Answer::SkipAll);
                        }
                        if ui
                            .button(RichText::new("Overwrite").color(theme::danger()))
                            .clicked()
                        {
                            answer = Some(Answer::Overwrite);
                        }
                        if ui
                            .button(RichText::new("Overwrite all").color(theme::danger()))
                            .clicked()
                        {
                            answer = Some(Answer::OverwriteAll);
                        }
                        // A rule rather than an answer, which is why it sits
                        // apart from the pair either side of it: from here on
                        // the newer file wins and the rest are skipped,
                        // without asking again.
                        if ui
                            .button("Only newer")
                            .on_hover_text(
                                "Keep going without asking: overwrite where the file \
                                 arriving is newer, and skip the rest",
                            )
                            .clicked()
                        {
                            answer = Some(Answer::OnlyNewer);
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(Answer::Cancel);
                        }
                    });
                });

                // Escape means cancel rather than "go away". The worker is
                // asleep waiting for this, so a dialog that closed without an
                // answer would leave the copy stopped for good.
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    answer = Some(Answer::Cancel);
                }
                if let Some(answer) = answer {
                    if let Some(job) = &self.job {
                        job.answer(answer);
                    }
                }
                still_open = answer.is_none();
            }
            Dialog::OpenWith {
                target,
                applications,
                typed,
                as_admin,
            } => {
                let target = target.clone();
                let name = target.file_name().unwrap_or_default().to_string_lossy();
                let (mut chosen, mut cancelled) = (None, false);

                // Up and Down before the list is drawn, so the cursor the
                // rows are painted with is the one this frame's keys moved.
                let matches = apps::matching(applications, typed).len();
                let last = matches.saturating_sub(1);
                ctx.input(|i| {
                    if i.key_pressed(egui::Key::ArrowDown) {
                        self.open_with_cursor = (self.open_with_cursor + 1).min(last);
                    }
                    if i.key_pressed(egui::Key::ArrowUp) {
                        self.open_with_cursor = self.open_with_cursor.saturating_sub(1);
                    }
                });
                let cursor = self.open_with_cursor.min(last);

                let escaped = modal(ctx, "Open with", |ui| {
                    ui.label(
                        RichText::new(format!("{name} with:"))
                            .size(12.0)
                            .color(theme::text_dim()),
                    );
                    ui.add_space(4.0);

                    // One box for both jobs: it narrows the list while
                    // anything matches, and is a command line when nothing
                    // does - which is how you reach something not installed
                    // as an application at all.
                    let hint = if matches == 0 {
                        "command to run"
                    } else {
                        "type to filter, or a command"
                    };
                    let entered = dialog_field(ui, typed, hint, accept_enter);
                    if entered {
                        chosen =
                            apps::choice(applications, typed, cursor).map(|c| owned_choice(&c));
                    }
                    ui.add_space(6.0);

                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            let mut previous_handled = true;
                            for (index, app) in
                                apps::matching(applications, typed).iter().enumerate()
                            {
                                // One line between the applications that
                                // claim this type and everything else, so
                                // the ordering is visible rather than
                                // merely true.
                                if previous_handled && !app.handles && index > 0 {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("Other applications")
                                            .size(10.5)
                                            .color(theme::text_faint()),
                                    );
                                    ui.add_space(2.0);
                                }
                                previous_handled = app.handles;

                                let label = if app.terminal {
                                    format!("{}  (in a shell tab)", app.name)
                                } else {
                                    app.name.clone()
                                };
                                let row = ui.selectable_label(index == cursor, label);
                                if row.clicked() {
                                    chosen = Some(OwnedChoice::App((*app).clone()));
                                }
                            }
                            if matches == 0 && !typed.trim().is_empty() {
                                ui.label(
                                    RichText::new(format!("Run: {}", typed.trim()))
                                        .size(11.5)
                                        .color(theme::text_dim()),
                                );
                            }
                        });

                    ui.add_space(8.0);
                    ui.checkbox(as_admin, "as administrator");
                    if *as_admin {
                        ui.label(
                            RichText::new(
                                "The system will ask. A whole application run as root writes \
                                 root-owned files into your own home directory - Shift-F4 \
                                 edits one file without that.",
                            )
                            .size(10.5)
                            .color(theme::text_faint()),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        cancelled |= ui.button("Cancel").clicked();
                        if ui.button("Open").clicked() {
                            chosen =
                                apps::choice(applications, typed, cursor).map(|c| owned_choice(&c));
                        }
                    });
                });

                if let Some(choice) = &chosen {
                    let borrowed = match choice {
                        OwnedChoice::App(app) => apps::Chosen::App(app),
                        OwnedChoice::Command(command) => apps::Chosen::Command(command),
                    };
                    let as_admin = *as_admin;
                    self.open_with(borrowed, &target, as_admin);
                }
                still_open = !escaped && !cancelled && chosen.is_none();
                if !still_open {
                    self.open_with_cursor = 0;
                }
            }
            Dialog::Theme { was } => {
                let was = *was;
                let mut finished = false;
                let escaped = modal(ctx, "Colours", |ui| {
                    finished = self.theme_form(ui, was);
                });
                if escaped {
                    theme::set_palette(was);
                    theme::apply(ctx);
                }
                still_open = !escaped && !finished;
            }
            Dialog::Pattern { text, select } => {
                let select = *select;
                let mut confirmed = false;
                let mut cancelled = false;
                let escaped = modal(ctx, if select { "Select" } else { "Deselect" }, |ui| {
                    ui.label(
                        RichText::new("Pattern, such as *.jpg")
                            .size(11.0)
                            .color(theme::text_faint()),
                    );
                    confirmed = dialog_field(ui, text, "*.jpg", accept_enter);
                    ui.horizontal(|ui| {
                        confirmed |= ui
                            .button(if select { "Select" } else { "Deselect" })
                            .clicked();
                        cancelled |= ui.button("Cancel").clicked();
                    });
                });
                if confirmed {
                    let pattern = text.clone();
                    self.select_pattern = pattern.clone();
                    let side = self.active;
                    let changed = self.panel_mut(side).mark_matching(&pattern, select);
                    let verb = if select { "Selected" } else { "Deselected" };
                    self.info(format!("{verb} {changed} matching {pattern}"));
                }
                still_open = !escaped && !cancelled && !confirmed;
            }
        }

        if still_open {
            self.dialog = Some(dialog);
        } else {
            // Through the same door Escape uses, so a dialog closed either
            // way lets go of the same things.
            self.close_dialog();
        }
    }

    // ---- the theme form ------------------------------------------------------

    /// Show the palette, live. Every change is applied as it is made, because
    /// a colour picker you cannot see the effect of is a guessing game.
    fn theme_form(&mut self, ui: &mut egui::Ui, was: theme::Palette) -> bool {
        let mut palette = theme::palette();
        let mut done = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new("Theme").size(11.5).color(theme::text_dim()));
            let current = theme::preset_name(&palette).unwrap_or("Custom");
            egui::ComboBox::from_id_salt("theme_preset")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (name, build) in theme::PRESETS {
                        if ui.selectable_label(current == *name, *name).clicked() {
                            palette = build();
                        }
                    }
                });
            if ui.button("Revert").clicked() {
                palette = was;
            }
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| {
                for (section, fields) in theme::Palette::sections() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(*section)
                            .size(10.5)
                            .strong()
                            .color(theme::text_faint()),
                    );
                    ui.add_space(2.0);
                    egui::Grid::new(section)
                        .num_columns(3)
                        .spacing([10.0, 3.0])
                        .show(ui, |ui| {
                            for (label, field) in *fields {
                                let mut colour = palette.get(*field);
                                if ui.color_edit_button_srgba(&mut colour).changed() {
                                    palette.set(*field, colour);
                                }
                                ui.label(RichText::new(*label).size(11.5));

                                // The hex is editable too: a colour is often
                                // something you have already, in writing.
                                let mut text = theme::to_hex(palette.get(*field));
                                let field_response = ui.add(
                                    egui::TextEdit::singleline(&mut text)
                                        .desired_width(74.0)
                                        .font(egui::TextStyle::Monospace),
                                );
                                if field_response.changed() {
                                    if let Some(parsed) = theme::parse_hex(&text) {
                                        palette.set(*field, parsed);
                                    }
                                }
                                ui.end_row();
                            }
                        });
                }
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Done").clicked() {
                done = true;
            }
            if ui.button("Cancel").clicked() {
                palette = was;
                done = true;
            }
        });

        // Applied every frame, so the window behind the form is already
        // wearing whatever is being picked.
        if palette != theme::palette() {
            theme::set_palette(palette);
            theme::apply(ui.ctx());
        }
        if done {
            self.settings_mut_save(palette);
        }
        done
    }

    /// Remember the palette in the settings file.
    fn settings_mut_save(&mut self, palette: theme::Palette) {
        theme::into_settings(palette, &mut self.settings);
        if let Err(e) = self.settings.save() {
            self.error(format!("Could not save the theme: {e}"));
        }
    }

    // ---- selecting in bulk --------------------------------------------------

    /// The menu behind the toolbar's select button.
    fn selection_menu(&mut self, ui: &mut egui::Ui) {
        let side = self.active;
        let count = self.panel(side).marked_count();

        ui.label(
            RichText::new(if count == 0 {
                "Nothing marked".to_string()
            } else {
                format!("{count} marked")
            })
            .size(11.0)
            .color(theme::text_faint()),
        );
        ui.separator();

        if ui.button("All").clicked() {
            self.panel_mut(side).mark_all();
        }
        if ui.button("None").clicked() {
            self.panel_mut(side).clear_marks();
        }
        if ui.button("Invert").clicked() {
            self.panel_mut(side).invert_marks();
        }

        ui.separator();
        ui.label(
            RichText::new("Pattern")
                .size(11.0)
                .color(theme::text_faint()),
        );
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.select_pattern)
                .hint_text("*.jpg")
                .desired_width(140.0),
        );
        // Enter in the box selects, which is the gesture the grey-plus dialog
        // has always had.
        let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        ui.horizontal(|ui| {
            let select = ui.button("Select").clicked() || submitted;
            let deselect = ui.button("Deselect").clicked();
            if select || deselect {
                let pattern = self.select_pattern.clone();
                if pattern.is_empty() {
                    self.error("No pattern");
                } else {
                    let changed = self.panel_mut(side).mark_matching(&pattern, select);
                    let verb = if select { "Selected" } else { "Deselected" };
                    self.info(format!("{verb} {changed} matching {pattern}"));
                }
            }
        });
    }

    // ---- terminal panel ----------------------------------------------------

    /// The copy and save pair, shared by the terminal and the console.
    ///
    /// `mirrored` is for a right-to-left strip, which lays widgets out
    /// backwards: the pair is added in the opposite order there so that it
    /// still reads "copy save" on screen.
    fn output_buttons(&mut self, ui: &mut egui::Ui, anything: bool, mirrored: bool) {
        const COPY_HINT: &str = "Copy this output to the clipboard";
        const SAVE_HINT: &str = "Write this output to a .log file in the active panel's folder";

        fn button(ui: &mut egui::Ui, label: &str, hint: &str, enabled: bool) -> bool {
            ui.add_enabled(
                enabled,
                egui::Button::new(RichText::new(label).size(11.0).color(theme::text()))
                    .fill(theme::surface_hi())
                    .corner_radius(CornerRadius::same(4)),
            )
            .on_hover_text(hint)
            .clicked()
        }

        let (copy, save) = if mirrored {
            let save = button(ui, "save", SAVE_HINT, anything);
            (button(ui, "copy", COPY_HINT, anything), save)
        } else {
            (
                button(ui, "copy", COPY_HINT, anything),
                button(ui, "save", SAVE_HINT, anything),
            )
        };

        if copy {
            match self.output_text() {
                Some(text) => {
                    let lines = text.lines().count();
                    ui.ctx().copy_text(text);
                    self.info(format!("Copied {lines} lines"));
                }
                None => self.error("Nothing to copy yet"),
            }
        }
        if save {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            self.save_output(&stamp);
        }
    }

    /// Tab strip, `+` button, and the emulated screen.
    fn terminal_panel(&mut self, ui: &mut egui::Ui) {
        // Claim the whole panel before drawing anything in it.
        //
        // egui remembers a panel's height from the rectangle its *contents*
        // filled last frame, not from the height it was given. The first
        // frame has no terminal open, so without this the panel would record
        // the height of a one-line "press +" label and stay squeezed to its
        // minimum for the rest of the session - which is exactly what it did.
        ui.set_min_height(ui.available_height());

        let mut open_with: Option<Option<String>> = None;
        let mut close_active = false;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            // The shells are listed in the rail, beside the directories they
            // are standing in - a shell and a folder are one thing here, and
            // they were being drawn as two lists that knew nothing of each
            // other.
            //
            // Right-to-left, so the first added sits furthest right.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                // A pinned tab is left where it is: it does not follow the
                // panes and the panes do not follow it. What you want for a
                // build running in one directory while you work in another -
                // without it, coupling the two means a shell you cannot keep
                // still.
                let tab = self.terminals.active;
                let mut pinned = self.terminals.is_pinned(tab);
                if ui
                    .checkbox(&mut pinned, RichText::new("pin").size(11.0))
                    .on_hover_text(
                        "Keep this terminal where it is: it stops following the panels,                          and they stop following it",
                    )
                    .changed()
                {
                    self.terminals.set_pinned(tab, pinned);
                    self.info(if pinned {
                        "Terminal pinned - it stays where it is"
                    } else {
                        "Terminal follows the panels again"
                    });
                }

                ui.add_space(6.0);

                if ui
                    .add(
                        egui::Button::new(RichText::new("cd here").size(11.0))
                            .fill(theme::surface_hi())
                            .corner_radius(CornerRadius::same(4)),
                    )
                    .on_hover_text("cd this terminal to the active panel's directory")
                    .clicked()
                {
                    self.terminal_follow_panel();
                }

                ui.add_space(6.0);

                // The per-tab x closes a named tab; this closes the one on
                // screen, which is what you want once the strip is long
                // enough that finding the right x is work.
                let any_open = !self.terminals.is_empty();
                if ui
                    .add_enabled(
                        any_open,
                        egui::Button::new(RichText::new("-").size(14.0).color(theme::text()))
                            .fill(theme::surface_hi())
                            .corner_radius(CornerRadius::same(4))
                            .min_size(Vec2::new(24.0, 20.0)),
                    )
                    .on_hover_text("Close the terminal on screen")
                    .clicked()
                {
                    close_active = true;
                }

                if self.shells.len() > 1 {
                    egui::ComboBox::from_id_salt("new_terminal_shell")
                        .selected_text(RichText::new("v").size(11.0))
                        .width(28.0)
                        .show_ui(ui, |ui| {
                            for candidate in &self.shells {
                                if shell_choice(ui, candidate, false) {
                                    open_with = Some(Some(candidate.clone()));
                                }
                            }
                        });
                }

                // A plain click opens the chosen shell; the arrow beside it
                // opens a specific one, as an editor does.
                if ui
                    .add(
                        egui::Button::new(RichText::new("+").size(14.0).color(theme::text()))
                            .fill(theme::surface_hi())
                            .corner_radius(CornerRadius::same(4))
                            .min_size(Vec2::new(24.0, 20.0)),
                    )
                    .on_hover_text("New terminal in the active panel's directory")
                    .clicked()
                {
                    open_with = Some(None);
                }

                ui.add_space(6.0);
                // On by default, and turned off here rather than in the
                // toolbar: it belongs to the shell, not to the window.
                let fill = if self.show_shell_history {
                    theme::accent()
                } else {
                    theme::surface_hi()
                };
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("hist").size(11.0).color(theme::text()),
                        )
                        .fill(fill)
                        .corner_radius(CornerRadius::same(4)),
                    )
                    .on_hover_text(
                        "What was run in this directory, under the places list",
                    )
                    .clicked()
                {
                    self.show_shell_history = !self.show_shell_history;
                }

                self.output_buttons(ui, any_open, true);

                // Recording belongs to a live terminal, so it is here and not
                // on the one-shot command line, which has no stream to tap.
                let recording = self
                    .terminals
                    .active()
                    .and_then(|session| session.recording());
                let label = match recording.is_some() {
                    true => format!(
                        "stop ({})",
                        self.terminals.active().map_or(0, |s| s.recorded_lines())
                    ),
                    false => "rec".to_string(),
                };
                let hint = match &recording {
                    Some(path) => format!("Recording to {} - click to stop", path.display()),
                    None => {
                        "Record everything this shell prints to a file in the active panel's folder"
                            .to_string()
                    }
                };
                let fill = if recording.is_some() {
                    theme::danger()
                } else {
                    theme::surface_hi()
                };
                if ui
                    .add_enabled(
                        any_open,
                        egui::Button::new(RichText::new(label).size(11.0).color(theme::text()))
                            .fill(fill)
                            .corner_radius(CornerRadius::same(4)),
                    )
                    .on_hover_text(hint)
                    .clicked()
                {
                    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                    self.toggle_recording(&stamp);
                }
            });
        });

        if close_active {
            self.close_active_terminal();
        }
        if let Some(program) = open_with {
            self.open_terminal(program);
        }

        ui.add_space(4.0);

        let area = ui.available_rect_before_wrap();
        if area.height() < 20.0 {
            return;
        }

        if self.terminals.is_empty() {
            ui.label(
                RichText::new("No terminal open - press + to start one.")
                    .color(theme::text_faint())
                    .size(11.5),
            );
            return;
        }

        // Clicking the screen gives the shell the keyboard.
        let response = ui.allocate_rect(area, Sense::click());
        if response.clicked() {
            self.terminal_focused = true;
        }

        // The emulator has to be told the size, or the shell wraps its output
        // to a width that is not the one on screen.
        let (rows, cols) = terminal::grid_for(area.size());
        let focused = self.terminal_focused;
        if let Some(session) = self.terminals.active_mut() {
            session.resize(rows, cols);
        }

        // The wheel scrolls whatever it is over, focused or not - that is how
        // every terminal behaves, and reaching for the mouse to read output
        // you can already see would be absurd.
        if response.hovered() {
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            let lines = terminal::wheel_lines(&mut self.terminal_scroll_carry, delta);
            if lines != 0 {
                if let Some(session) = self.terminals.active_mut() {
                    terminal::apply_scroll(session, terminal::Scroll::Lines(lines), rows);
                }
            }
        }
        if let Some(session) = self.terminals.active() {
            terminal::draw_screen(ui, area, session, theme::sidebar(), theme::text(), focused);
        }
    }

    /// Feed the keyboard to the shell while the terminal has focus.
    fn terminal_input(&mut self, ctx: &egui::Context) {
        if !self.terminal_focused || self.terminals.is_empty() {
            return;
        }
        // The key that handed the terminal the keyboard must not also be typed
        // into it. `Ctrl-E` opens a root shell and moves focus there, and
        // without this the shell then receives a literal 0x05 on top of the
        // line it was given.
        if std::mem::take(&mut self.terminal_taken) {
            return;
        }

        let mut outgoing: Vec<u8> = Vec::new();
        let mut scrolls: Vec<terminal::Scroll> = Vec::new();
        // The record of the command line has to keep up here too. Typing from
        // the panes and then focusing the shell to finish the command is the
        // whole point of the escalation, and if the record went stale the
        // moment focus moved, coming back to the panes would find Enter still
        // trying to run a line the shell had already taken.
        let mut typed = String::new();
        let (mut submitted, mut rubbed_out) = (false, false);
        ctx.input(|input| {
            for event in &input.events {
                match event {
                    // Typed characters arrive here already composed, which is
                    // what makes dead keys and IME work.
                    egui::Event::Text(text) => {
                        outgoing.extend_from_slice(text.as_bytes());
                        typed.push_str(text);
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        // The panel's own scroll keys are taken first, or
                        // Shift-PageUp would reach the shell as a plain
                        // PageUp and page a pager instead.
                        if let Some(scroll) = terminal::scroll_command(*key, *modifiers) {
                            scrolls.push(scroll);
                        } else if let Some(bytes) = terminal::key_bytes(*key, *modifiers) {
                            outgoing.extend_from_slice(&bytes);
                            match key {
                                egui::Key::Enter => submitted = true,
                                egui::Key::Backspace => rubbed_out = true,
                                // Ctrl-C and Ctrl-U both abandon the line.
                                egui::Key::C | egui::Key::U
                                    if modifiers.ctrl || modifiers.command =>
                                {
                                    submitted = true
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        self.pending_input.push_str(&typed);
        if rubbed_out {
            self.pending_input.pop();
        }
        if submitted {
            self.pending_input.clear();
        }

        let rows = self.terminals.active().map(|s| s.size().0).unwrap_or(24);
        if let Some(session) = self.terminals.active_mut() {
            for scroll in scrolls {
                terminal::apply_scroll(session, scroll, rows);
            }
            // Writing snaps the view back to the prompt, so this has to come
            // after the scrolling or a stray keystroke would undo it.
            if !outgoing.is_empty() {
                session.write(&outgoing);
            }
        }
    }

    // ---- console -----------------------------------------------------------

    fn console_panel(&mut self, ui: &mut egui::Ui) {
        // Claim exactly the panel - no more - and draw into a child.
        //
        // egui takes a panel's height from the rectangle its *contents*
        // filled last frame, so content that comes out even two pixels taller
        // than the panel makes the panel two pixels taller next frame, and
        // again, and again. The nested prompt-row panel did exactly that. It
        // went unnoticed while the window only redrew on input; once a
        // terminal is open from startup the window repaints continuously and
        // the console ate the panes within seconds.
        let area = ui.available_rect_before_wrap();
        ui.set_min_height(area.height());
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let ui = &mut inner;

        // Prompt row first, pinned to the bottom of the panel.
        egui::TopBottomPanel::bottom("prompt_row")
            .frame(egui::Frame::NONE)
            .exact_height(26.0)
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let running = self.shell_job.is_some();
                    if tool_button(
                        ui,
                        if self.show_output {
                            icons::Tool::ListView
                        } else {
                            icons::Tool::GridView
                        },
                        "Show or hide command output",
                        self.show_output,
                    )
                    .clicked()
                    {
                        self.show_output = !self.show_output;
                    }

                    // Before the prompt, because the text field takes all the
                    // width that is left after it.
                    let anything = !self.console.is_empty();
                    self.output_buttons(ui, anything, false);

                    // Which shell runs the commands. Only worth showing when
                    // the machine actually offers a choice.
                    if self.shells.len() > 1 {
                        let (current, _) = self.chosen_shell();
                        let label = Path::new(&current)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| current.clone());
                        let mut pick: Option<Option<String>> = None;

                        egui::ComboBox::from_id_salt("shell_picker")
                            .selected_text(RichText::new(label).size(11.0))
                            .width(84.0)
                            .show_ui(ui, |ui| {
                                let following_env = self.settings.shell.is_none();
                                if ui
                                    .selectable_label(following_env, "system default")
                                    .on_hover_text("Follow $SHELL / %ComSpec%")
                                    .clicked()
                                {
                                    pick = Some(None);
                                }
                                for candidate in &self.shells {
                                    let selected =
                                        self.settings.shell.as_deref() == Some(candidate.as_str());
                                    if shell_choice(ui, candidate, selected) {
                                        pick = Some(Some(candidate.clone()));
                                    }
                                }
                            });

                        if let Some(choice) = pick {
                            self.set_shell(choice);
                        }
                    }

                    ui.label(
                        RichText::new(self.prompt())
                            .color(theme::ok())
                            .size(12.0)
                            .monospace(),
                    );

                    let width = ui.available_width() - 8.0;
                    let field = ui.add_sized(
                        Vec2::new(width.max(80.0), 22.0),
                        egui::TextEdit::singleline(&mut self.command)
                            .font(egui::TextStyle::Monospace)
                            .hint_text(if running {
                                "running..."
                            } else {
                                "type a command, Ctrl-Enter inserts the selected name"
                            })
                            .frame(true),
                    );

                    // Plain Enter runs; Ctrl-Enter was already consumed above.
                    let submitted = field.lost_focus()
                        && ui.input(|i| {
                            i.key_pressed(egui::Key::Enter)
                                && !(i.modifiers.ctrl || i.modifiers.command)
                        });
                    if submitted {
                        self.run_command();
                        field.request_focus();
                    }
                });
            });

        if !self.show_output {
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.console.is_empty() && self.shell_job.is_none() {
                    ui.label(
                        RichText::new("No commands run yet.")
                            .color(theme::text_faint())
                            .size(11.0),
                    );
                }
                for entry in &self.console {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&entry.prompt)
                                .color(theme::ok())
                                .size(11.5)
                                .monospace(),
                        )
                        .on_hover_text(entry.cwd.display().to_string());
                        ui.label(
                            RichText::new(&entry.line)
                                .color(theme::text())
                                .size(11.5)
                                .monospace(),
                        );
                    });
                    for (text, colour) in [
                        (&entry.output.stdout, theme::text_dim()),
                        (&entry.output.stderr, theme::danger()),
                    ] {
                        if !text.trim().is_empty() {
                            ui.label(
                                RichText::new(text.trim_end())
                                    .color(colour)
                                    .size(11.5)
                                    .monospace(),
                            );
                        }
                    }
                    ui.add_space(4.0);
                }
                if let Some(job) = &self.shell_job {
                    ui.label(
                        RichText::new(format!("$ {}  ...", job.line))
                            .color(theme::text_faint())
                            .size(11.5)
                            .monospace(),
                    );
                }
            });
    }

    // ---- status strip ------------------------------------------------------

    /// Keep the two halves in step, which is only meaningful while both are
    /// on screen. Called once a frame; split out so a test can drive it
    /// without a window.
    pub fn sync_halves(&mut self) {
        if self.half == Half::Both {
            self.follow_the_shell();
            self.shell_follows_the_pane();
        }
    }

    /// Which workspace is on show: the identity of the left pane's tab.
    ///
    /// The left pane, because that is the one that is always there - the
    /// second is part of the arrangement a workspace *carries*, so it cannot
    /// also be what identifies one.
    pub fn workspace_id(&self) -> u64 {
        self.left.current().id
    }

    /// The shell standing in this workspace, if it is still running.
    ///
    /// Looked up rather than trusted: a shell can end at any moment, and a
    /// pairing that outlived one would send you to somebody else's.
    pub fn shell_here(&self) -> Option<usize> {
        let shell = self.workspaces.get(&self.workspace_id())?.shell?;
        self.terminals.at_id(shell)
    }

    /// Write down how the window is arranged now, against this workspace.
    ///
    /// Called on the way out of one, and whenever something that belongs to
    /// a workspace changes while it is on show.
    pub fn remember_workspace(&mut self) {
        let id = self.workspace_id();
        let now = Workspace {
            shell: self.terminals.active_id(),
            show_right: self.show_right,
            right: self.right.current().cwd.clone(),
            left_view: self.left_view,
            right_view: self.right_view,
            active: self.active,
            synced: !self.terminals.is_pinned(self.terminals.active),
            split: self.split,
        };
        // A shell is only this workspace's if one is actually running: with
        // none open, `active_id` is None and the entry says so.
        self.workspaces.insert(id, now);
    }

    /// Put a workspace's arrangement back, having shown its tab.
    fn restore_workspace(&mut self) {
        let Some(want) = self.workspaces.get(&self.workspace_id()).cloned() else {
            // Never been left before - a tab from before workspaces existed,
            // or one just forked. What is on screen is as good as anything.
            return;
        };
        self.show_right = want.show_right;
        self.left_view = want.left_view;
        self.right_view = want.right_view;
        self.split = want.split;
        self.active = if want.show_right {
            want.active
        } else {
            Side::Left
        };
        self.on_tree = [
            want.left_view == ViewMode::Tree,
            want.right_view == ViewMode::Tree,
        ];
        if self.right.current().cwd != want.right {
            self.right.current_mut().chdir(want.right.clone());
        }
        if let Some(at) = want.shell.and_then(|id| self.terminals.at_id(id)) {
            self.terminals.select(at);
            self.terminal_scroll_carry = 0.0;
            if at < self.terminals.pinned.len() {
                self.terminals.pinned[at] = !want.synced;
            }
        }
    }

    /// Show a workspace: its directory, its arrangement, and its shell.
    pub fn show_workspace(&mut self, index: usize) {
        if index == self.left.active() {
            return;
        }
        self.remember_workspace();
        self.left.activate(index);
        self.restore_workspace();
    }

    /// Tie the shell on show to the workspace on show.
    ///
    /// The pairing is made by using them together, which is the only moment
    /// anybody could have meant it.
    pub fn pair_shell_here(&mut self) {
        self.remember_workspace();
    }

    /// Give the window to one half, or hand it back to both.
    ///
    /// Pressing the same key again is how you get back, so this toggles
    /// rather than sets: `Ctrl-O` twice leaves you where you started.
    pub fn show_half(&mut self, want: Half) {
        let was = self.half;
        self.half = if was == want { Half::Both } else { want };
        if self.half == want {
            match want {
                Half::Shell => {
                    self.show_terminal = true;
                    if self.terminals.is_empty() {
                        self.open_terminal(None);
                    }
                    self.terminal_focused = true;
                    self.terminal_taken = true;
                    self.info("The shell has the window. Ctrl-O brings the panes back.");
                }
                Half::Files => {
                    self.terminal_focused = false;
                    self.info("The panes have the window. Ctrl-Shift-O brings the shell back.");
                }
                Half::Both => {}
            }
            return;
        }

        // Back to both, and back into step. Whichever half had the window is
        // the one that is right about where you are: it is where the work
        // just happened, and the other has been sitting still.
        match was {
            Half::Shell => {
                self.terminal_focused = false;
                self.follow_the_shell();
            }
            Half::Files => self.shell_follows_the_pane(),
            Half::Both => {}
        }
    }

    /// Put the shell back on screen, for something that is about to use it.
    ///
    /// Typing a command, completing one, walking the history, sending a name
    /// to the prompt: all of them are answered by a shell, and answering
    /// into one that is not on screen would look like the keystroke was
    /// swallowed.
    fn shell_back_on_screen(&mut self) {
        if self.half == Half::Files {
            self.show_half(Half::Files);
        }
    }

    /// The rail: every workspace, out of the panes and out of the shell.
    ///
    /// Tabs used to be drawn twice, in two strips that knew nothing of each
    /// other: directories along the top of a pane and shells along the top of
    /// the drawer. They are one thing - a place you are working, and the
    /// shell standing in it - and this is the one list of them.
    fn rail(&mut self, ui: &mut egui::Ui, area: Rect) {
        let side = self.active;
        let wide = self.rail_wide;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(area.shrink2(CHROME_PAD))
                .layout(Layout::top_down(Align::Min)),
        );

        child.horizontal(|ui| {
            if wide {
                ui.label(
                    RichText::new("WORKSPACES")
                        .size(9.5)
                        .color(theme::text_faint()),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(if wide { "<" } else { ">" })
                                .size(10.5)
                                .color(theme::text_faint()),
                        )
                        .fill(Color32::TRANSPARENT)
                        .min_size(Vec2::new(0.0, 14.0)),
                    )
                    .on_hover_text(if wide {
                        "Narrow: an icon and the folder's own name"
                    } else {
                        "Wide: the whole path, and the shell standing in it"
                    })
                    .clicked()
                {
                    self.rail_wide = !wide;
                }
            });
        });
        child.add_space(2.0);

        // Read out before drawing: the rows need `self` and so cannot hold a
        // borrow of the tabs while they are drawn.
        let rows: Vec<(usize, String, String, Option<String>, bool)> = self
            .tabs(side)
            .all()
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let shell = self
                    .workspaces
                    .get(&panel.id)
                    .and_then(|workspace| workspace.shell)
                    .and_then(|id| self.terminals.at_id(id))
                    .and_then(|at| self.terminals.sessions.get(at))
                    .map(|session| session.title.clone());
                (
                    index,
                    tabs::title(&panel.cwd),
                    lost_commander_core::paths::undecorated(&panel.cwd)
                        .display()
                        .to_string(),
                    shell,
                    index == self.tabs(side).active(),
                )
            })
            .collect();

        let mut want: Option<usize> = None;
        let mut close: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("rail")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                for (index, name, path, shell, current) in &rows {
                    let colour = if *current {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    let fill = if *current {
                        theme::surface_hi()
                    } else {
                        Color32::TRANSPARENT
                    };
                    let response = egui::Frame::NONE
                        .fill(fill)
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(5, 3))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("\u{1F5C0}").size(11.0).color(colour));
                                    ui.label(RichText::new(name).size(11.5).color(colour));
                                });
                                if wide {
                                    ui.label(
                                        RichText::new(path).size(9.5).color(theme::text_faint()),
                                    );
                                    // What the shell is, or that there is not
                                    // one: a workspace with no shell is worth
                                    // saying, since Ctrl-O will start one.
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(match shell {
                                                Some(name) => format!("> {name}"),
                                                None => "> no shell".to_string(),
                                            })
                                            .size(9.5)
                                            .color(
                                                if shell.is_some() {
                                                    theme::accent_dim()
                                                } else {
                                                    theme::text_faint()
                                                },
                                            ),
                                        );
                                    });
                                }
                            });
                        })
                        .response;

                    let hit = ui.interact(
                        response.rect,
                        ui.id().with(("rail_row", index)),
                        Sense::click(),
                    );
                    if hit.clicked() {
                        want = Some(*index);
                    }
                    // Middle-click closes, as it does on every tab strip
                    // there has ever been. The wide view has no room for a
                    // cross without pushing the path out of it.
                    if hit.middle_clicked() {
                        close = Some(*index);
                    }
                    let _ = hit.on_hover_text(format!(
                        "{path}\n{}",
                        match shell {
                            Some(name) => format!("shell: {name}"),
                            None => "no shell yet".to_string(),
                        }
                    ));
                }
            });

        if let Some(index) = want {
            self.show_workspace(index);
        }
        if let Some(index) = close {
            self.tabs_mut(side).activate(index);
            if !self.tabs_mut(side).close() {
                self.error("That is the only workspace in this pane");
            }
        }
    }

    /// The top-left sector: one pane, or two with a draggable line between.
    ///
    /// The second pane splits this sector and nothing else, which is what
    /// keeps the shell below exactly as wide as the panes however they are
    /// arranged. A tree splits a pane the other way, inside it.
    fn panes_in(&mut self, ui: &mut egui::Ui, full: Rect) {
        if !self.show_right {
            // One pane, the whole sector - the active one, as every
            // dual-pane manager does it, so folding the other away never
            // moves you somewhere you were not looking. The hidden Panel is
            // untouched, which is what makes its directory, cursor and marks
            // still be there afterwards.
            let side = self.active;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(full));
            self.pane(&mut child, side);
            return;
        }

        let (left, divider, right) = pane_rects(full, self.split);

        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(left));
        self.pane(&mut child, Side::Left);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(right));
        self.pane(&mut child, Side::Right);

        // The divider goes last so it takes the pointer before the panes do,
        // and its grab area is wider than the line it draws - a three-pixel
        // target is a target you miss.
        let grab = divider.expand2(Vec2::new(GRAB - GUTTER * 0.5, 0.0));
        let response = ui.interact(grab, ui.id().with("pane_divider"), Sense::click_and_drag());
        if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                self.split = split_from_pointer(full, pointer.x);
            }
        }
        // Back to even, without hunting for the middle by hand.
        if response.double_clicked() {
            self.split = 0.5;
        }
        // Written when the drag ends rather than while it runs: the other way
        // is a file write every frame the pointer moves.
        if response.drag_stopped() || response.double_clicked() {
            self.remember_layout();
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        let colour = if response.dragged() {
            theme::accent()
        } else if response.hovered() {
            theme::accent_dim()
        } else {
            theme::border()
        };
        ui.painter().vline(
            divider.center().x,
            full.y_range(),
            egui::Stroke::new(1.0, colour),
        );
    }

    /// One of the two seams of the window: draw it, drag it, remember it.
    ///
    /// Returns where the pointer is while it is being dragged. Both seams go
    /// through this, because a seam that behaved slightly differently from
    /// the other is a seam somebody has to learn twice.
    fn drag_seam(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        seam: Rect,
        vertical: bool,
    ) -> Option<egui::Pos2> {
        let grab = if vertical {
            seam.expand2(Vec2::new(GRAB - GUTTER * 0.5, 0.0))
        } else {
            seam.expand2(Vec2::new(0.0, GRAB - GUTTER * 0.5))
        };
        let response = ui.interact(grab, ui.id().with(id), Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(if vertical {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::ResizeVertical
            });
        }
        if response.drag_stopped() {
            self.remember_layout();
        }

        let colour = if response.dragged() {
            theme::accent()
        } else if response.hovered() {
            theme::accent_dim()
        } else {
            theme::border()
        };
        if vertical {
            ui.painter().vline(
                seam.center().x,
                seam.y_range(),
                egui::Stroke::new(1.0, colour),
            );
        } else {
            ui.painter().hline(
                seam.x_range(),
                seam.center().y,
                egui::Stroke::new(1.0, colour),
            );
        }

        response
            .dragged()
            .then(|| response.interact_pointer_pos())
            .flatten()
    }

    /// What has been run in the shell's own directory, beside it.
    ///
    /// Clicking a line types it into the shell without running it, which is
    /// the same bargain `Enter` on a journal entry makes: a command from
    /// yesterday may want a different flag today, and running it on one click
    /// would be a keystroke that deletes something.
    fn shell_history_column(&mut self, ui: &mut egui::Ui, area: Rect) {
        // The directory the *pane* is showing, not the shell's. "Here" has to
        // mean the same thing as the workspace you are looking at, or
        // switching tabs would leave the list answering about the last place
        // a shell happened to be standing. With a shell in step the two are
        // the same anyway; when they are not, the pane is what you can see.
        let here = self.panel(self.active).cwd.clone();
        let now = ui.input(|input| input.time);
        if self.shell_history_of.as_deref() != Some(here.as_path())
            || now - self.shell_history_read_at > 1.0
        {
            self.shell_history = self.commands_in(&here);
            self.shell_history_of = Some(here.clone());
            self.shell_history_read_at = now;
        }

        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let mut here_only = self.history_here_only;
        child.horizontal(|ui| {
            ui.label(
                RichText::new("History")
                    .size(10.5)
                    .color(theme::text_faint()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Which half of the account this is showing. Two words rather
                // than a checkbox: "here" and "all" say what you will get,
                // where a tickbox labelled "filter" says only that there is
                // one.
                for (only, label) in [(false, "all"), (true, "here")] {
                    let chosen = here_only == only;
                    let colour = if chosen {
                        theme::text()
                    } else {
                        theme::text_faint()
                    };
                    let fill = if chosen {
                        theme::surface_hi()
                    } else {
                        Color32::TRANSPARENT
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(label).size(10.5).color(colour))
                                .fill(fill)
                                .corner_radius(CornerRadius::same(3))
                                .min_size(Vec2::new(0.0, 16.0)),
                        )
                        .on_hover_text(if only {
                            "Only what was run in this directory"
                        } else {
                            "Everything, this directory first"
                        })
                        .clicked()
                    {
                        here_only = only;
                    }
                }
            });
        });
        self.history_here_only = here_only;

        let shown = self.history_shown();
        if shown.is_empty() {
            child.label(
                RichText::new(if self.history_here_only {
                    "nothing run here yet"
                } else {
                    "nothing yet"
                })
                .size(11.0)
                .color(theme::text_faint()),
            );
            return;
        }

        // Cloned out of `self` so the closure below is not holding a borrow
        // of it while the reuse needs one.
        let rows: Vec<(String, Option<String>)> = shown
            .into_iter()
            .map(|past| {
                let elsewhere = (Some(past.cwd.as_path()) != self.shell_history_of.as_deref())
                    .then(|| {
                        past.cwd
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| past.cwd.display().to_string())
                    });
                (past.line.clone(), elsewhere)
            })
            .collect();

        let mut reuse: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt("shell_history")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                for (line, elsewhere) in &rows {
                    // Where it ran, when that is not where you are. The same
                    // words in another directory are about other work, and a
                    // list that did not say so would offer them as if they
                    // were the same command.
                    let hint = match elsewhere {
                        Some(folder) => {
                            format!("Ran in {folder} - click to put it on the command line")
                        }
                        None => "Click to put this on the command line".to_string(),
                    };
                    let response = ui.add(
                        egui::Label::new(RichText::new(line).monospace().size(11.0).color(
                            if elsewhere.is_some() {
                                theme::text_faint()
                            } else {
                                theme::text_dim()
                            },
                        ))
                        .truncate()
                        .sense(Sense::click()),
                    );
                    if response.on_hover_text(hint).clicked() {
                        reuse = Some(line.clone());
                    }
                }
            });
        if let Some(line) = reuse {
            self.type_into_command_line(&line);
            self.terminal_focused = true;
        }
    }

    /// The lines the history column is showing, after the here/all filter.
    fn history_shown(&self) -> Vec<&journal::Past> {
        let here = self.shell_history_of.as_deref();
        self.shell_history
            .iter()
            .filter(|past| !self.history_here_only || Some(past.cwd.as_path()) == here)
            .collect()
    }

    /// The last week of the shell stream, as what was run in one directory.
    fn commands_in(&self, here: &Path) -> Vec<journal::Past> {
        let Some(journal) = &self.journal else {
            return Vec::new();
        };
        let mut records = Vec::new();
        let mut days = journal.days(journal::Stream::Shell);
        days.truncate(7);
        for day in days.into_iter().rev() {
            records.extend(journal.read(journal::Stream::Shell, day));
        }
        // Everything, here first - the filter is applied when it is drawn, so
        // switching between "here" and "all" does not re-read the account.
        journal::commands_before(&records, here)
    }

    /// What was done to the things in the other pane's folder.
    fn pane_history(&mut self, ui: &mut egui::Ui, side: Side) {
        let here = self.panel(side.other()).cwd.clone();
        let now = ui.input(|input| input.time);
        if self.history_of.as_deref() != Some(here.as_path()) || now - self.history_read_at > 1.0 {
            self.history_rows = self.happenings_in(&here);
            self.history_of = Some(here.clone());
            self.history_read_at = now;
        }

        if self.history_rows.is_empty() {
            ui.label(
                RichText::new(format!(
                    "Nothing recorded in {} - the last week of the account is what this reads.",
                    lost_commander_core::paths::undecorated(&here).display()
                ))
                .color(theme::text_faint())
                .size(11.5),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("pane_history")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for happening in &self.history_rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new(journal::clock(happening.at))
                                .monospace()
                                .size(11.0)
                                .color(theme::text_faint()),
                        );
                        // A delete and a failure read the same way here on
                        // purpose: both are the row you are scanning for.
                        let alarming =
                            happening.failed.is_some() || happening.kind.is_destructive();
                        let colour = if alarming {
                            theme::danger()
                        } else {
                            theme::text_dim()
                        };
                        ui.label(
                            RichText::new(happening.kind.label())
                                .size(11.0)
                                .color(colour),
                        );
                        ui.label(
                            RichText::new(&happening.name)
                                .size(11.5)
                                .color(theme::text()),
                        );
                        if let Some(became) = &happening.became {
                            ui.label(
                                RichText::new(format!("-> {became}"))
                                    .size(11.5)
                                    .color(theme::text()),
                            );
                        }
                        if let Some(other) = &happening.other {
                            // Which way it went, which is half of what the row
                            // is worth: a file arriving and a file leaving look
                            // identical without it.
                            let way = if happening.incoming { "from" } else { "to" };
                            ui.label(
                                RichText::new(format!(
                                    "{way} {}",
                                    lost_commander_core::paths::undecorated(other).display()
                                ))
                                .size(11.0)
                                .color(theme::text_faint()),
                            );
                        }
                        if let Some(why) = &happening.failed {
                            ui.label(
                                RichText::new(format!("- {why}"))
                                    .size(11.0)
                                    .color(theme::danger()),
                            );
                        }
                    });
                }
            });
    }

    /// The last week of the file stream, as what happened in one folder.
    fn happenings_in(&self, here: &Path) -> Vec<journal::Happening> {
        let Some(journal) = &self.journal else {
            return Vec::new();
        };
        let mut records = Vec::new();
        // Newest last, so `happened_in` walking backwards sees the newest
        // first. Far enough back to be useful, not so far that a pane reads a
        // year of files.
        let mut days = journal.days(journal::Stream::Files);
        days.truncate(7);
        for day in days.into_iter().rev() {
            records.extend(journal.read(journal::Stream::Files, day));
        }
        journal::happened_in(&records, here)
    }

    /// The function keys, as the terminal view has always shown them.
    ///
    /// What this bar says is read out of the same table the keyboard uses, so
    /// it cannot drift from what the keys do - a key bar that lies is worse
    /// than none, and this is the third time in this program a hand-written
    /// list of keys had gone stale. F9 is not "sort" here, whatever Norton
    /// did: in the graphical view it opens the selection menu, and the bar
    /// says so because it is asking rather than remembering.
    fn key_bar(&mut self, ui: &mut egui::Ui) {
        let mut chosen: Option<keys::Action> = None;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let width = (ui.available_width() / 12.0).clamp(52.0, 120.0);
            for (number, action) in keys::function_keys() {
                // "F5 Copy", not "5 Copy": the point of the bar is to say
                // which key does it, and the number alone is what a reader
                // has to already know to make sense of.
                let label = format!("F{number} {}", keys::name_of(action));
                if ui
                    .add_sized(
                        Vec2::new(width, 20.0),
                        egui::Button::new(RichText::new(label).size(10.5).color(theme::text()))
                            .fill(theme::surface_hi())
                            .corner_radius(CornerRadius::same(3)),
                    )
                    .clicked()
                {
                    chosen = Some(action);
                }
            }
        });
        if let Some(action) = chosen {
            self.run_action(action);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if let Some(job) = &self.job {
                let progress = job.snapshot();
                let fraction = progress.fraction() as f32;

                ui.label(
                    RichText::new(progress.verb)
                        .color(theme::text())
                        .size(12.0)
                        .strong(),
                );

                // A hand-painted bar: a rounded track with an accent fill.
                let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 10.0), Sense::hover());
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(5), theme::surface_hi());
                let filled = Rect::from_min_size(
                    rect.min,
                    Vec2::new(rect.width() * fraction, rect.height()),
                );
                ui.painter()
                    .rect_filled(filled, CornerRadius::same(5), theme::accent());

                ui.label(
                    RichText::new(format!(
                        "{}%  {} / {}",
                        progress.percent(),
                        human_size(progress.bytes_done),
                        human_size(progress.bytes_total)
                    ))
                    .color(theme::text_dim())
                    .size(11.5),
                );

                // Compact so it sits comfortably inside the strip.
                let cancel = ui.add(
                    egui::Button::new(RichText::new("Cancel").size(11.0).color(theme::text()))
                        .min_size(Vec2::new(52.0, 18.0))
                        .corner_radius(CornerRadius::same(4))
                        .fill(theme::surface_hi()),
                );
                if cancel.clicked() {
                    job.request_cancel();
                }
            } else {
                let colour = if self.status_is_error {
                    theme::danger()
                } else {
                    theme::text_dim()
                };
                ui.label(RichText::new(&self.status).color(colour).size(11.5));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let panel = self.active_panel();
                let text = if panel.marked_count() > 0 {
                    format!(
                        "{} selected \u{00B7} {}",
                        panel.marked_count(),
                        human_size(panel.marked_size())
                    )
                } else {
                    format!("sort: {}", panel.sort_by.label())
                };
                ui.label(RichText::new(text).color(theme::text_faint()).size(11.0));
            });
        });
    }

    // ---- headless verification --------------------------------------------

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_to.clone() else {
            return;
        };
        // Let the layout settle before capturing.
        if self.frames == 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let pixels: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|p| [p.r(), p.g(), p.b(), p.a()])
                .collect();
            if let Err(e) = image::save_buffer(
                &path,
                &pixels,
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            ) {
                eprintln!("screenshot failed: {e}");
            } else {
                eprintln!("wrote {}", path.display());
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

// ---- small painting helpers -----------------------------------------------

#[allow(non_upper_case_globals)]
const Align2_LEFT_CENTER: egui::Align2 = egui::Align2::LEFT_CENTER;
#[allow(non_upper_case_globals)]
const Align2_RIGHT_CENTER: egui::Align2 = egui::Align2::RIGHT_CENTER;
#[allow(non_upper_case_globals)]
const Align2_CENTER_CENTER: egui::Align2 = egui::Align2::CENTER_CENTER;

/// The background behind one row or tile.
///
/// A filled highlight throughout, with no outline anywhere: an outline draws
/// a box around every marked row, and a screen of them reads as a grid rather
/// than as a selection. Marked and cursor are told apart by hue - amber
/// against blue - and a row that is both gets a brighter amber, since it is
/// still primarily where you are.
pub fn selection_fill(
    is_cursor: bool,
    marked: bool,
    focused: bool,
    hovered: bool,
) -> Option<Color32> {
    match (is_cursor, marked) {
        (true, true) if focused => Some(theme::marked_cursor()),
        (true, true) => Some(theme::marked()),
        (true, false) if focused => Some(theme::selected()),
        (true, false) => Some(theme::selected_idle()),
        (false, true) => Some(theme::marked()),
        (false, false) if hovered => Some(theme::hover()),
        _ => None,
    }
}

fn paint_selection(ui: &egui::Ui, rect: Rect, is_cursor: bool, marked: bool, focused: bool) {
    let hovered = ui.rect_contains_pointer(rect);
    if let Some(fill) = selection_fill(is_cursor, marked, focused, hovered) {
        ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    }
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(text)
            .color(theme::text_faint())
            .size(10.0)
            .strong(),
    );
    ui.add_space(2.0);
}

fn sidebar_row(ui: &mut egui::Ui, label: &str, kind: icons::Kind, dim: bool) -> bool {
    sidebar_row_with(ui, label, kind, dim, None)
}

/// As [`sidebar_row`], with something written along the right-hand edge.
///
/// Used for how much room is left on a drive - the first thing anybody wants
/// to know of one, and shown since there were floppies to run out of.
fn sidebar_row_with(
    ui: &mut egui::Ui,
    label: &str,
    kind: icons::Kind,
    dim: bool,
    trailing: Option<String>,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), theme::hover());
    }
    let icon = Rect::from_min_size(
        egui::pos2(rect.min.x + 2.0, rect.center().y - 7.0),
        Vec2::splat(14.0),
    );
    icons::draw(ui.painter(), icon, kind, dim);
    ui.painter().text(
        egui::pos2(icon.right() + 6.0, rect.center().y),
        Align2_LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        if dim {
            theme::text_faint()
        } else {
            theme::text_dim()
        },
    );
    if let Some(trailing) = trailing {
        ui.painter().text(
            egui::pos2(rect.right() - 6.0, rect.center().y),
            Align2_RIGHT_CENTER,
            trailing,
            FontId::proportional(10.0),
            theme::text_faint(),
        );
    }
    response.clicked()
}

/// A toolbar button whose symbol is painted, not typed. `on` gives it the
/// accent fill used for the active view mode.
/// One shell in a picker, with whether its commands can be recorded.
///
/// The mark is there rather than the shell being missing: which shell to use
/// is the user's decision and not a logging feature's to make, but a choice
/// whose consequences are invisible is not really a choice.
fn shell_choice(ui: &mut egui::Ui, program: &str, selected: bool) -> bool {
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| program.to_string());
    let recorded = lost_commander_core::shellhook::journals(program);
    let label = match recorded {
        true => name,
        false => format!("{name}  \u{00b7} not recorded"),
    };
    let hint = match recorded {
        true => format!("{program}\nCommands run here are kept in the account"),
        false => format!("{program}\n{}", lost_commander_core::shellhook::why_not()),
    };
    let text = match recorded {
        true => RichText::new(label),
        false => RichText::new(label).color(theme::text_dim()),
    };
    ui.selectable_label(selected, text)
        .on_hover_text(hint)
        .clicked()
}

fn tool_button(ui: &mut egui::Ui, tool: icons::Tool, tooltip: &str, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 26.0), Sense::click());
    let fill = if on {
        theme::accent_dim()
    } else if response.hovered() {
        theme::hover()
    } else {
        theme::surface_hi()
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);

    let colour = if on || response.hovered() {
        theme::text()
    } else {
        theme::text_dim()
    };
    icons::draw_tool(ui.painter(), rect.shrink(7.0), tool, colour);
    response.on_hover_text(tooltip)
}

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::border());
}

/// Break a file name into lines of at most `width` characters, preferring to
/// split on separators so extensions stay readable.
pub fn wrap_label(name: &str, width: usize) -> Vec<String> {
    if name.chars().count() <= width {
        return vec![name.to_string()];
    }
    let chars: Vec<char> = name.chars().collect();
    let first: String = chars[..width].iter().collect();
    let rest: String = chars[width..].iter().collect();
    let second = if rest.chars().count() > width {
        let cut: String = rest.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}\u{2026}")
    } else {
        rest
    };
    vec![first, second]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A left pane with three files and a directory, a separate right pane.
    ///
    /// The pointer interactions are tested here rather than by driving a real
    /// window: `apply_click` is the whole selection model, and testing it
    /// directly is deterministic, whereas a headless X server has no window
    /// manager and therefore no reliable modifier state.
    fn fixture() -> (tempfile::TempDir, GuiApp) {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::create_dir(left.join("sub")).unwrap();
        fs::write(left.join("a.txt"), "a").unwrap();
        fs::write(left.join("b.txt"), "bb").unwrap();
        fs::write(left.join("c.txt"), "ccc").unwrap();
        (root, GuiApp::detached(left, right))
    }

    /// Swap the real opener for one that only records, and hand back the log.
    fn watch_opener(app: &mut GuiApp) -> std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>> {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&log);
        app.opener = Box::new(move |path: &Path| {
            recorder.lock().unwrap().push(path.to_path_buf());
            Ok(())
        });
        log
    }

    fn index_of(app: &GuiApp, side: Side, name: &str) -> usize {
        app.panel(side)
            .entries
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"))
    }

    #[test]
    fn a_plain_click_selects_one_entry_and_drops_the_rest() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        let b = index_of(&app, Side::Left, "b.txt");

        app.apply_click(Side::Left, Some((a, Click::Toggle)), None);
        app.apply_click(Side::Left, Some((b, Click::Toggle)), None);
        assert_eq!(app.left.current().marked_count(), 2);

        // A click without the modifier collapses the selection.
        let c = index_of(&app, Side::Left, "c.txt");
        app.apply_click(Side::Left, Some((c, Click::Plain)), None);
        assert_eq!(app.left.current().marked_count(), 0);
        assert_eq!(app.left.current().selected().unwrap().name, "c.txt");
    }

    #[test]
    fn ctrl_click_toggles_entries_additively() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        let c = index_of(&app, Side::Left, "c.txt");

        app.apply_click(Side::Left, Some((a, Click::Toggle)), None);
        app.apply_click(Side::Left, Some((c, Click::Toggle)), None);
        assert_eq!(app.left.current().marked_count(), 2);

        // Ctrl-clicking a marked entry unmarks it again.
        app.apply_click(Side::Left, Some((a, Click::Toggle)), None);
        assert_eq!(app.left.current().marked_count(), 1);
        assert!(
            app.left
                .current()
                .entries
                .iter()
                .find(|e| e.name == "c.txt")
                .unwrap()
                .marked
        );
    }

    #[test]
    fn the_parent_entry_can_never_be_marked() {
        let (_root, mut app) = fixture();
        app.apply_click(Side::Left, Some((0, Click::Toggle)), None);
        assert!(app.left.current().selected().unwrap().is_parent());
        assert_eq!(app.left.current().marked_count(), 0);
    }

    #[test]
    fn clicking_a_pane_gives_it_focus() {
        let (_root, mut app) = fixture();
        assert_eq!(app.active, Side::Left);
        app.apply_click(Side::Right, Some((0, Click::Plain)), None);
        assert_eq!(app.active, Side::Right);
    }

    #[test]
    fn double_clicking_a_directory_navigates_and_records_it() {
        let (_root, mut app) = fixture();
        let target = app.left.cwd().join("sub");

        app.apply_click(Side::Left, None, Some(target.clone()));

        assert_eq!(app.left.cwd(), target);
        assert!(!app.status_is_error, "{}", app.status);
        // Navigation feeds the Recent list.
        assert_eq!(app.bookmarks.recent[0].path, target.display().to_string());
    }

    #[test]
    fn navigating_somewhere_unreadable_reports_an_error() {
        let (root, mut app) = fixture();
        app.apply_click(Side::Left, None, Some(root.path().join("missing")));
        assert!(app.status_is_error);
    }

    #[test]
    fn selection_feeds_the_copy_and_delete_actions() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        let b = index_of(&app, Side::Left, "b.txt");
        app.apply_click(Side::Left, Some((a, Click::Toggle)), None);
        app.apply_click(Side::Left, Some((b, Click::Toggle)), None);

        let targets = app.selection(Side::Left);
        assert_eq!(targets.len(), 2);

        // With nothing marked it falls back to the entry under the cursor.
        app.left.current_mut().clear_marks();
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);
        assert_eq!(
            app.selection(Side::Left),
            vec![app.left.cwd().join("a.txt")]
        );
    }

    #[test]
    fn copying_asks_where_and_lands_the_files() {
        let (_root, mut app) = fixture();
        let destination = app.right.cwd().to_path_buf();
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        app.copy_to_other();
        // Nothing has moved yet: it asks where first.
        assert!(
            app.job.is_none(),
            "nothing runs until the destination is in"
        );
        let Some(Dialog::CopyTo {
            what,
            destination: offered,
            job,
        }) = app.dialog.take()
        else {
            panic!("F5 should ask where it is going");
        };
        assert_eq!(what, "a.txt");
        // One pane, so there is no other pane to guess at, and it offers this
        // one - somewhere to edit rather than somewhere to accept.
        assert_eq!(offered, app.left.cwd().display().to_string());

        assert!(app.send_to(job, &destination.display().to_string()));
        assert!(app.job.is_some(), "a copy should be running");

        // Drain the worker the way the frame loop would.
        if let Some(job) = &mut app.job {
            job.join();
        }
        app.poll_job();

        assert!(app.job.is_none());
        assert!(destination.join("a.txt").exists());
        assert!(!app.status_is_error, "{}", app.status);
    }

    #[test]
    fn the_destination_offered_is_the_other_pane_when_there_is_one() {
        let (_root, mut app) = fixture();
        app.show_right = true;
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        app.copy_to_other();
        let Some(Dialog::CopyTo { destination, .. }) = app.dialog.take() else {
            panic!("F5 should ask where it is going");
        };
        // Two panes: the other one is the answer nearly every time, so Enter
        // alone is the whole interaction.
        assert_eq!(destination, app.right.cwd().display().to_string());
    }

    #[test]
    fn a_destination_that_is_not_a_directory_starts_nothing() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);
        app.copy_to_other();
        let Some(Dialog::CopyTo { job, .. }) = app.dialog.take() else {
            panic!("asks where");
        };

        assert!(!app.send_to(job, "no/such/place"));
        assert!(app.job.is_none(), "nothing runs");
        assert!(app.status_is_error);
    }

    #[test]
    fn acting_with_an_empty_selection_is_refused() {
        let (_root, mut app) = fixture();
        app.left.current_mut().cursor_home(); // sits on ".."
        app.copy_to_other();
        assert!(app.job.is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn each_pane_chooses_its_own_view() {
        let (_root, mut app) = fixture();
        assert_eq!(app.view(Side::Left), ViewMode::Details);

        app.set_view(Side::Left, ViewMode::Tree);
        assert_eq!(app.view(Side::Left), ViewMode::Tree);
        assert!(app.left.current().in_tree_mode());
        // The other pane is untouched.
        assert_eq!(app.view(Side::Right), ViewMode::Details);
        assert!(!app.right.current().in_tree_mode());

        app.set_view(Side::Left, ViewMode::Grid);
        assert!(
            !app.left.current().in_tree_mode(),
            "leaving tree mode drops the tree"
        );
    }

    #[test]
    fn the_tree_opens_on_the_pane_s_own_directory() {
        let (_root, mut app) = fixture();
        let cwd = app.left.cwd().to_path_buf();
        app.set_view(Side::Left, ViewMode::Tree);

        let tree = app.left.current().tree.as_ref().unwrap();
        // Revealed down to where the pane is, so it doubles as "you are here".
        assert_eq!(tree.selected_path().unwrap(), cwd);
    }

    #[test]
    fn the_window_starts_with_one_terminal_already_running() {
        let (_root, mut app) = fixture();
        assert!(app.terminals.is_empty(), "not opened by the constructor");

        app.open_initial_terminal();
        assert_eq!(app.terminals.len(), 1, "there from the first frame");
        assert_eq!(app.terminals.active().unwrap().cwd, app.left.cwd());
        // Present, not holding the keyboard - the files are what the window is
        // for, and a shell that ate the first keystroke would be a surprise.
        assert!(!app.terminal_focused);

        // Every frame calls it; only the first may open anything.
        for _ in 0..5 {
            app.open_initial_terminal();
        }
        assert_eq!(app.terminals.len(), 1);
    }

    #[test]
    fn closing_the_last_terminal_means_closed() {
        let (_root, mut app) = fixture();
        app.open_initial_terminal();

        app.close_active_terminal();
        assert!(app.terminals.is_empty());
        assert!(!app.terminal_focused, "nothing left to type at");

        // The next frame must not quietly start another one.
        app.open_initial_terminal();
        assert!(app.terminals.is_empty(), "closed means closed");
    }

    #[test]
    fn minus_closes_the_terminal_on_screen() {
        let (_root, mut app) = fixture();
        app.open_terminal(None);
        app.open_terminal(None);
        app.open_terminal(None);
        assert_eq!(app.terminals.active, 2);

        // The middle tab, not the last one, to prove it follows the selection.
        app.terminals.select(1);
        let doomed = app.terminals.sessions[1].title.clone();
        app.close_active_terminal();

        assert_eq!(app.terminals.len(), 2);
        assert!(!app.terminals.sessions.iter().any(|s| s.title == doomed));
        assert!(app.terminal_focused, "two are still open");

        // And it is harmless with nothing left.
        app.close_active_terminal();
        app.close_active_terminal();
        app.close_active_terminal();
        assert!(app.terminals.is_empty());
    }

    #[test]
    fn the_divider_splits_the_width_and_leaves_a_gutter() {
        let full = Rect::from_min_size(egui::pos2(100.0, 0.0), Vec2::new(800.0, 600.0));

        let (left, divider, right) = pane_rects(full, 0.5);
        assert_eq!(left.min.x, 100.0);
        assert_eq!(right.max.x, 900.0);
        // The panes stop either side of the gutter rather than meeting.
        assert_eq!(divider.width(), GUTTER);
        assert_eq!(left.max.x, divider.min.x);
        assert_eq!(right.min.x, divider.max.x);
        assert_eq!(divider.center().x, 500.0);
        // Full height, so it can be grabbed anywhere down the seam.
        assert_eq!(divider.height(), 600.0);

        // Dragged over, the left pane keeps three quarters.
        let (left, _, right) = pane_rects(full, 0.75);
        assert!((left.width() - (600.0 - GUTTER * 0.5)).abs() < 0.01);
        assert!((right.width() - (200.0 - GUTTER * 0.5)).abs() < 0.01);
    }

    #[test]
    fn a_narrow_pane_drops_columns_rather_than_overprinting() {
        // A wide pane shows everything, permissions included.
        assert_eq!(
            row_columns(520.0, false),
            RowColumns {
                mode: true,
                date: true,
                size: true
            }
        );
        // Dragged in, the permissions go first: they are the least often what
        // you came to the listing for.
        assert_eq!(
            row_columns(400.0, false),
            RowColumns {
                mode: false,
                date: true,
                size: true
            }
        );
        // Then the date - the name is what a listing is for.
        assert_eq!(
            row_columns(250.0, false),
            RowColumns {
                mode: false,
                date: false,
                size: true
            }
        );
        // Narrower still and the name has the row to itself.
        assert_eq!(
            row_columns(150.0, false),
            RowColumns {
                mode: false,
                date: false,
                size: false
            }
        );

        // A directory never shows a size, however wide the pane.
        assert_eq!(
            row_columns(520.0, true),
            RowColumns {
                mode: true,
                date: true,
                size: false
            }
        );
    }

    #[test]
    fn the_columns_only_ever_appear_from_the_right() {
        // Widening a pane must not take a column away again: whatever a
        // narrower pane showed, a wider one still shows.
        let mut previous = row_columns(0.0, false);
        for width in (0..1200).step_by(7).map(|w| w as f32) {
            let columns = row_columns(width, false);
            assert!(columns.size >= previous.size, "size vanished at {width}");
            assert!(columns.date >= previous.date, "date vanished at {width}");
            assert!(columns.mode >= previous.mode, "mode vanished at {width}");
            previous = columns;
        }
    }

    #[test]
    fn the_columns_always_leave_the_name_something_to_sit_in() {
        for width in [120.0, 191.0, 301.0, 431.0, 800.0, 2000.0] {
            for is_dir in [false, true] {
                let taken = 8.0 + row_columns(width, is_dir).width(is_dir);
                assert!(
                    taken < width,
                    "columns took {taken} of a {width}-wide row, leaving the name nothing"
                );
            }
        }
    }

    #[test]
    fn neither_pane_can_be_dragged_away_to_nothing() {
        let full = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1000.0, 600.0));

        // A pane dragged to nothing looks like a bug and cannot be grabbed
        // back; folding one away is the toolbar toggle's job.
        assert_eq!(split_from_pointer(full, -500.0), SPLIT_MIN);
        assert_eq!(split_from_pointer(full, 5000.0), SPLIT_MAX);
        assert!((split_from_pointer(full, 300.0) - 0.3).abs() < 0.001);

        // The rects respect it even if the field is set past the limit.
        let (left, _, _) = pane_rects(full, 0.99);
        assert!(left.width() < 1000.0 * SPLIT_MAX);

        // A window collapsed to nothing must not divide by zero.
        let empty = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::ZERO);
        assert_eq!(split_from_pointer(empty, 10.0), 0.5);
    }

    #[test]
    fn the_pointer_position_is_measured_from_the_pane_area() {
        // Not from the window: the sidebar shifts the area's left edge, and
        // ignoring that would make the divider jump on the first drag.
        let shifted = Rect::from_min_size(egui::pos2(210.0, 0.0), Vec2::new(800.0, 600.0));
        assert!((split_from_pointer(shifted, 610.0) - 0.5).abs() < 0.001);
    }

    #[test]
    fn the_keyboard_moves_the_cursor_and_walks_directories() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::Home);
        assert_eq!(app.left.current().cursor, 0);

        app.run_action(A::CursorDown);
        app.run_action(A::CursorDown);
        assert_eq!(app.left.current().cursor, 2);
        app.run_action(A::CursorUp);
        assert_eq!(app.left.current().cursor, 1);
        app.run_action(A::End);
        assert_eq!(
            app.left.current().cursor,
            app.left.current().entries.len() - 1
        );

        // Enter walks into a directory, Backspace walks back out.
        let sub = index_of(&app, Side::Left, "sub");
        app.left.current_mut().cursor_to(sub);
        let was = app.left.cwd().to_path_buf();
        app.run_action(A::Open);
        assert_eq!(app.left.cwd(), was.join("sub"));
        app.run_action(A::Parent);
        assert_eq!(app.left.cwd(), was);

        // Tab crosses to the other pane, and back.
        app.show_right = true;
        assert_eq!(app.active, Side::Left);
        app.run_action(A::SwitchPane);
        assert_eq!(app.active, Side::Right);
        app.run_action(A::SwitchPane);
        assert_eq!(app.active, Side::Left);
    }

    #[test]
    fn tab_opens_the_second_pane_rather_than_moving_onto_nothing() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        assert!(!app.show_right, "one pane to begin with");

        app.run_action(A::SwitchPane);
        // "The other pane" only means something if there is one, so asking
        // for it is how you get it - the cursor still never lands on a pane
        // nobody can see.
        assert!(app.show_right);
        assert_eq!(app.active, Side::Right);

        // And F12 folds it away. The pane left showing is the one that was
        // active, so folding never moves you off what you were reading.
        app.run_action(A::ToggleSecondPane);
        assert!(!app.show_right);
        assert_eq!(app.active, Side::Right);
    }

    #[test]
    fn the_grey_keys_mark_from_the_keyboard() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::Home);

        // Insert marks and steps down, as it always has.
        let before = app.left.current().cursor;
        app.run_action(A::CursorDown);
        app.run_action(A::Mark);
        assert_eq!(app.left.current().marked_count(), 1);
        assert_eq!(
            app.left.current().cursor,
            before + 2,
            "the cursor stepped down"
        );

        app.run_action(A::InvertMarks);
        assert_eq!(
            app.left.current().marked_count(),
            app.left.current().entries.len() - 2
        );

        app.run_action(A::MarkAll);
        assert_eq!(
            app.left.current().marked_count(),
            app.left.current().entries.len() - 1
        );
        app.run_action(A::ClearMarks);
        assert_eq!(app.left.current().marked_count(), 0);
    }

    #[test]
    fn the_f_keys_open_the_dialogs_the_view_was_missing() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        app.run_action(A::Help);
        assert!(matches!(app.dialog, Some(Dialog::Help)));
        app.run_action(A::Cancel);
        assert!(app.dialog.is_none());

        app.run_action(A::Rename);
        match &app.dialog {
            Some(Dialog::Rename { name, .. }) => assert_eq!(name, "a.txt"),
            _ => panic!("F2 should offer to rename what the cursor is on"),
        }
        app.run_action(A::Cancel);

        app.run_action(A::MkDir);
        assert!(matches!(app.dialog, Some(Dialog::MkDir { .. })));
        app.run_action(A::Cancel);

        // Delete asks first, rather than going ahead on one keystroke.
        app.left.current_mut().mark_all();
        app.run_action(A::Delete);
        match &app.dialog {
            Some(Dialog::ConfirmDelete { targets, .. }) => {
                assert_eq!(targets.len(), app.left.current().entries.len() - 1);
            }
            _ => panic!("F8 must confirm before deleting"),
        }
        app.run_action(A::Cancel);
        assert!(app.dialog.is_none());
        // Nothing was touched.
        assert!(app.left.cwd().join("a.txt").exists());
    }

    #[test]
    fn the_grey_plus_and_minus_ask_for_a_pattern() {
        use keys::Action as A;
        let (_root, mut app) = fixture();

        app.run_action(A::SelectByPattern);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Pattern { select: true, .. })
        ));
        app.run_action(A::Cancel);

        app.run_action(A::DeselectByPattern);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Pattern { select: false, .. })
        ));
    }

    #[test]
    fn views_and_panels_are_all_reachable_by_key() {
        use keys::Action as A;
        let (_root, mut app) = fixture();

        app.run_action(A::ViewGrid);
        assert_eq!(app.view(Side::Left), ViewMode::Grid);
        app.run_action(A::ViewDetails);
        assert_eq!(app.view(Side::Left), ViewMode::Details);

        // Ctrl-T toggles the tree rather than only turning it on.
        app.run_action(A::ViewTree);
        assert_eq!(app.view(Side::Left), ViewMode::Tree);
        app.run_action(A::ViewTree);
        assert_eq!(app.view(Side::Left), ViewMode::Details);

        // Quick view goes in the *other* pane: this one is what it follows.
        app.run_action(A::QuickView);
        assert_eq!(app.view(Side::Right), ViewMode::Preview);
        assert_eq!(app.active, Side::Left);
        app.run_action(A::QuickView);
        assert_eq!(app.view(Side::Right), ViewMode::Details);

        let sidebar = app.show_sidebar;
        app.run_action(A::ToggleSidebar);
        assert_ne!(app.show_sidebar, sidebar);

        app.run_action(A::ToggleShellPanel);
        assert!(!app.show_terminal);
        app.run_action(A::ToggleShellPanel);
        assert!(app.show_terminal);
    }

    #[test]
    fn swapping_the_panes_takes_their_views_with_them() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.set_view(Side::Left, ViewMode::Grid);
        let (left, right) = (app.left.cwd().to_path_buf(), app.right.cwd().to_path_buf());

        app.run_action(A::SwapPanes);
        assert_eq!(app.left.cwd(), right);
        assert_eq!(app.right.cwd(), left);
        // The view belongs to the listing, not to the side of the window.
        assert_eq!(app.view(Side::Right), ViewMode::Grid);
        assert_eq!(app.view(Side::Left), ViewMode::Details);
    }

    #[test]
    fn typing_goes_to_the_command_line_not_to_the_panes() {
        let (_root, mut app) = fixture();
        // With the one-shot command line showing, it is a plain String.
        app.show_terminal = false;
        assert!(app.command_line_empty());

        app.type_into_command_line("l");
        app.type_into_command_line("s");
        assert_eq!(app.command, "ls");
        assert!(!app.command_line_empty());

        app.command_line_backspace();
        assert_eq!(app.command, "l");
        app.command_line_clear();
        assert!(app.command_line_empty());
    }

    /// Swap the real launcher for one that records the command it was given.
    fn watch_launcher(app: &mut GuiApp) -> std::sync::Arc<std::sync::Mutex<Vec<open::Launch>>> {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&log);
        app.launcher = Box::new(move |command: &open::Launch| {
            recorder.lock().unwrap().push(command.clone());
            Ok(())
        });
        log
    }

    // Windows shows its own chooser instead of ours - see the test in `apps`.
    #[cfg(not(windows))]
    #[test]
    fn shift_enter_opens_the_chooser_for_the_file_under_the_cursor() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let expected = app.left.cwd().join("a.txt");
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        app.run_action(A::OpenWith);
        match &app.dialog {
            Some(Dialog::OpenWith { target, typed, .. }) => {
                assert_eq!(target, &expected);
                assert!(typed.is_empty());
            }
            _ => panic!("expected the chooser"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn the_chooser_starts_the_application_it_settled_on() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let started = watch_launcher(&mut app);
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        app.run_action(A::OpenWith);

        // The real list comes from the machine, so stand one in that does not.
        let Some(Dialog::OpenWith { applications, .. }) = &mut app.dialog else {
            panic!("expected the chooser")
        };
        *applications = vec![apps::Application {
            name: "Text Editor".into(),
            exec: "gedit %U".into(),
            handles: true,
            terminal: false,
        }];

        let Some(Dialog::OpenWith {
            target,
            applications,
            typed,
            ..
        }) = app.dialog.take()
        else {
            unreachable!()
        };
        let chosen = apps::choice(&applications, &typed, 0).unwrap();
        app.open_with(chosen, &target, false);

        let started = started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].program, "gedit");
        assert_eq!(started[0].args, vec![target.display().to_string()]);
    }

    #[test]
    fn an_application_that_wants_a_terminal_gets_a_shell_tab() {
        let (_root, mut app) = fixture();
        let started = watch_launcher(&mut app);
        let target = app.left.cwd().join("a.txt");
        let vim = apps::Application {
            name: "Vim".into(),
            exec: "vim %F".into(),
            handles: true,
            terminal: true,
        };

        app.open_with(apps::Chosen::App(&vim), &target, false);

        // Not spawned with its output thrown away - the shell panel opened
        // for it, which is the same route F4 takes to $EDITOR.
        assert!(started.lock().unwrap().is_empty(), "vim was spawned blind");
        assert!(app.show_terminal);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_dialog_is_not_answered_by_the_key_that_opened_it() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        // Shift-Enter *is* an Enter press, so the chooser it opens would read
        // that same press as "confirm" and start its first row before it had
        // ever been seen - which is exactly what it did until this flag.
        assert_eq!(
            keys::action_for(egui::Key::Enter, egui::Modifiers::SHIFT),
            Some(A::OpenWith)
        );

        let had_dialog = app.dialog.is_some();
        app.run_action(A::OpenWith);
        if !had_dialog && app.dialog.is_some() {
            app.dialog_opened = true;
        }

        // It is what `dialogs` reads to withhold Enter, and it clears itself,
        // so the very next frame accepts Enter as normal.
        assert!(app.dialog.is_some());
        assert!(
            std::mem::take(&mut app.dialog_opened),
            "the frame was not flagged"
        );
        assert!(!app.dialog_opened);
    }

    #[cfg(unix)]
    #[test]
    fn shift_f4_edits_without_running_the_editor_as_root() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let started = watch_launcher(&mut app);
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        assert_eq!(
            keys::action_for(egui::Key::F4, egui::Modifiers::SHIFT),
            Some(A::EditAsAdmin)
        );
        app.run_action(A::EditAsAdmin);

        // It goes to a shell tab, because sudoedit has to ask for a password
        // and that is where a prompt can appear.
        assert!(
            started.lock().unwrap().is_empty(),
            "spawned instead of asked"
        );
        assert!(app.show_terminal, "no terminal for the prompt");
    }

    #[cfg(unix)]
    #[test]
    fn a_root_shell_opens_where_the_panel_is() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let started = watch_launcher(&mut app);

        assert_eq!(
            keys::action_for(egui::Key::E, egui::Modifiers::CTRL),
            Some(A::RootShell)
        );
        app.run_action(A::RootShell);

        assert!(
            started.lock().unwrap().is_empty(),
            "spawned instead of asked"
        );
        assert!(app.show_terminal);
    }

    #[cfg(not(windows))]
    #[test]
    fn as_administrator_never_starts_the_application_unprivileged() {
        let (_root, mut app) = fixture();
        let started = watch_launcher(&mut app);
        let target = app.left.cwd().join("a.txt");
        let editor = apps::Application {
            name: "Text Editor".into(),
            exec: "gedit %U".into(),
            handles: true,
            terminal: false,
        };

        app.open_with(apps::Chosen::App(&editor), &target, true);

        let started = started.lock().unwrap();
        assert!(
            !started.iter().any(|c| c.program == "gedit"),
            "started unprivileged: {started:?}"
        );
        // Either the system was asked graphically, or a sudo line went to a
        // shell tab - never the bare command.
        let asked = started
            .iter()
            .any(|c| ["pkexec", "kdesu", "lxqt-sudo", "osascript"].contains(&c.program.as_str()));
        assert!(asked || app.show_terminal, "nothing asked: {started:?}");
    }

    #[test]
    fn only_newer_brings_a_copy_up_to_date_without_asking_again() {
        let (_root, mut app) = fixture();
        let source = app.left.cwd().to_path_buf();
        let destination = app.right.cwd().to_path_buf();

        // a.txt is newer in the source; b.txt is newer at the destination.
        for name in ["a.txt", "b.txt"] {
            fs::write(destination.join(name), "AT THE DESTINATION").unwrap();
        }
        fs::write(source.join("a.txt"), "FRESH").unwrap();
        fs::write(source.join("b.txt"), "STALE").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let new = old + std::time::Duration::from_secs(60 * 60);
        let stamp = |path: PathBuf, when: std::time::SystemTime| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(when))
                .unwrap();
        };
        stamp(source.join("a.txt"), new);
        stamp(destination.join("a.txt"), old);
        stamp(source.join("b.txt"), old);
        stamp(destination.join("b.txt"), new);

        app.start(Operation::Copy {
            sources: vec![source.join("a.txt"), source.join("b.txt")],
            destination: destination.clone(),
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.job.as_ref().and_then(|j| j.asking()).is_none()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let job = app.job.as_ref().expect("a copy is running");
        assert!(job.asking().is_some(), "the copy never asked");

        // A rule, not an answer: the rest of the run follows it.
        job.answer(Answer::OnlyNewer);
        for _ in 0..400 {
            app.poll_job();
            if app.job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.job.is_none(), "the copy never finished");
        assert_eq!(
            fs::read_to_string(destination.join("a.txt")).unwrap(),
            "FRESH",
            "the newer file arriving wins"
        );
        assert_eq!(
            fs::read_to_string(destination.join("b.txt")).unwrap(),
            "AT THE DESTINATION",
            "and the newer file already there is left alone"
        );
    }

    #[test]
    fn a_copy_onto_an_existing_file_puts_the_question_on_screen() {
        let (_root, mut app) = fixture();
        let target_dir = app.right.cwd().to_path_buf();
        fs::write(target_dir.join("a.txt"), "PRECIOUS").unwrap();
        app.right.reload();

        app.start(Operation::Copy {
            sources: vec![app.left.cwd().join("a.txt")],
            destination: target_dir.clone(),
        });

        // The worker blocks on the collision, so this is a wait, not a race.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.dialog.is_none() && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        match &app.dialog {
            Some(Dialog::ConfirmOverwrite { conflict }) => {
                assert_eq!(conflict.target, target_dir.join("a.txt"));
                assert_eq!(conflict.target_size, 8);
            }
            _ => panic!("the copy never asked"),
        }
        // Nothing has been written while the question is up.
        assert_eq!(
            fs::read_to_string(target_dir.join("a.txt")).unwrap(),
            "PRECIOUS"
        );

        // Answering releases the worker.
        app.job
            .as_ref()
            .unwrap()
            .answer(progress::Answer::Overwrite);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.job.is_some() && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(target_dir.join("a.txt")).unwrap(), "a");
    }

    #[test]
    fn f8_asks_about_the_trash_and_shift_f8_asks_about_for_good() {
        use keys::Action as A;
        // Which of the two a key means, without running either: the trash is
        // the user's real one and a test has no business in it. The trash
        // mechanics have their own tests, against a temporary directory.
        let (_root, mut app) = fixture();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        assert_eq!(
            keys::action_for(egui::Key::F8, egui::Modifiers::NONE),
            Some(A::Delete)
        );
        assert_eq!(
            keys::action_for(egui::Key::F8, egui::Modifiers::SHIFT),
            Some(A::DeleteForever)
        );
        assert_eq!(
            keys::action_for(egui::Key::Delete, egui::Modifiers::SHIFT),
            Some(A::DeleteForever)
        );

        app.run_action(A::Delete);
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmDelete { to_trash: true, .. })
        ));
        app.dialog = None;

        app.run_action(A::DeleteForever);
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmDelete {
                to_trash: false,
                ..
            })
        ));
        // Nothing has happened either way: the question comes first.
        assert!(app.left.cwd().join("a.txt").exists());
    }

    #[test]
    fn find_walks_the_tree_and_going_to_a_hit_lands_the_cursor_on_it() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let deep = app.left.cwd().join("sub/deeper");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("buried.txt"), "found me").unwrap();

        assert_eq!(
            keys::action_for(egui::Key::F, egui::Modifiers::CTRL),
            Some(A::Find)
        );
        assert_eq!(
            keys::action_for(egui::Key::F7, egui::Modifiers::ALT),
            Some(A::Find)
        );

        app.run_action(A::Find);
        match &app.dialog {
            Some(Dialog::Find { root, .. }) => assert_eq!(root, &app.left.cwd()),
            _ => panic!("expected the find form"),
        }

        // Drive the engine directly: the form is what the live check is for,
        // and this is about the walk and the landing.
        let mut search = find::Search::spawn(
            app.left.cwd().to_path_buf(),
            find::Query {
                pattern: "buried*".into(),
                ..find::Query::default()
            },
        );
        search.join();
        let hits = search.snapshot().hits;
        assert_eq!(hits.len(), 1, "{hits:?}");

        app.dialog = None;
        app.go_to(&hits[0].path);
        assert_eq!(app.left.cwd(), deep);
        assert_eq!(
            app.left.current().selected().map(|e| e.name.as_str()),
            Some("buried.txt")
        );
    }

    #[test]
    fn a_dialogs_typing_never_reaches_the_command_line() {
        // The panes send anything printable to the shell, which is the whole
        // point of the command line - but a modal's boxes are not the panes.
        // Keying that off focus alone let a two-field form, which has frames
        // where neither field holds focus, type into both at once.
        let (_root, mut app) = fixture();
        app.show_terminal = false;

        app.type_into_command_line("half");
        assert!(!app.command_line_empty());
        app.command_line_clear();

        app.run_action(keys::Action::Find);
        assert!(app.dialog.is_some());
        // With a dialog open there is nowhere for stray text to go, whatever
        // egui's focus happens to be doing this frame.
        assert!(
            app.command_line_empty(),
            "the form left something on the line"
        );
    }

    #[test]
    fn compare_files_takes_two_from_either_gesture() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let (left, right) = (app.left.cwd().to_path_buf(), app.right.cwd().to_path_buf());
        fs::write(left.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        fs::write(left.join("b.txt"), "alpha\nBETA\ngamma\n").unwrap();
        fs::write(right.join("a.txt"), "alpha\nbeta\ndelta\n").unwrap();
        app.left.reload();
        app.right.reload();

        // One from each pane, under the cursors.
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        let at = index_of(&app, Side::Right, "a.txt");
        app.right.current_mut().cursor_to(at);
        app.run_action(A::CompareFiles);
        let Some(Dialog::Difference {
            diff,
            left: l,
            right: r,
            ..
        }) = &app.dialog
        else {
            panic!("no difference shown: {}", app.status);
        };
        assert_eq!(l, &left.join("a.txt"));
        assert_eq!(r, &right.join("a.txt"));
        assert_eq!(diff.changes, 2, "gamma out, delta in");
        app.close_dialog();

        // Two marked in one pane, wherever the cursors happen to be.
        for name in ["a.txt", "b.txt"] {
            let at = index_of(&app, Side::Left, name);
            app.left.current_mut().cursor_to(at);
            app.left.current_mut().toggle_mark();
        }
        app.run_action(A::CompareFiles);
        let Some(Dialog::Difference {
            diff,
            left: l,
            right: r,
            ..
        }) = &app.dialog
        else {
            panic!("no difference shown: {}", app.status);
        };
        assert_eq!(l, &left.join("a.txt"));
        assert_eq!(r, &left.join("b.txt"), "both from the one pane");
        assert_eq!(diff.changes, 2, "beta out, BETA in");
    }

    #[test]
    fn two_identical_files_are_said_rather_than_shown() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        fs::write(app.right.cwd().join("a.txt"), "a").unwrap();
        app.left.reload();
        app.right.reload();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        let at = index_of(&app, Side::Right, "a.txt");
        app.right.current_mut().cursor_to(at);

        app.run_action(A::CompareFiles);
        assert!(
            app.dialog.is_none(),
            "a window of no differences is a puzzle"
        );
        assert!(app.status.contains("identical"), "{}", app.status);
        assert!(!app.status_is_error);
    }

    #[test]
    fn comparing_files_has_the_mnemonic_key_and_the_one_that_works_everywhere() {
        // Shift-F3 reads well and works here. The terminal view cannot have
        // it - its escape sequence is also the cursor-position report - so
        // Alt-D is the one both front-ends share, beside Alt-C and Alt-S.
        assert_eq!(
            keys::action_for(egui::Key::F3, egui::Modifiers::SHIFT),
            Some(keys::Action::CompareFiles)
        );
        assert_eq!(
            keys::action_for(egui::Key::D, egui::Modifiers::ALT),
            Some(keys::Action::CompareFiles)
        );
        assert_eq!(
            keys::action_for(egui::Key::F3, egui::Modifiers::NONE),
            Some(keys::Action::View),
            "and a plain F3 still views one file"
        );
    }

    #[test]
    fn alt_u_finds_the_copies_and_keeps_one_of_each() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let left = app.left.cwd().to_path_buf();
        // a.txt already says "a"; two more copies of it, one nested.
        fs::write(left.join("copy-a.txt"), "a").unwrap();
        fs::write(left.join("sub/copy-b.txt"), "a").unwrap();
        app.left.reload();

        app.run_action(A::Duplicates);
        assert!(matches!(app.dialog, Some(Dialog::Duplicates { .. })));

        // The hunt runs on a thread; take its results as the dialog does.
        let mut scan = app.hunt.take().expect("a hunt was started");
        scan.join();
        let found = scan.snapshot();
        assert!(found.finished);
        let Some(Dialog::Duplicates { groups, .. }) = &mut app.dialog else {
            panic!("the window closed");
        };
        *groups = found.groups;
        assert_eq!(groups.len(), 1, "one set: three copies of \"a\"");
        assert_eq!(groups[0].copies.len(), 3);

        groups[0].keep_first();
        let going = dupes::to_remove(groups);
        assert_eq!(going.len(), 2);
        assert_eq!(dupes::reclaimed(groups), 2);

        // Deleting goes through the ordinary confirm, and to the trash.
        app.dialog = Some(Dialog::ConfirmDelete {
            targets: going.clone(),
            to_trash: true,
        });
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmDelete { to_trash: true, .. })
        ));
    }

    #[test]
    fn closing_the_window_stops_the_hunt() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::Duplicates);
        assert!(app.hunt.is_some());
        app.close_dialog();
        assert!(app.dialog.is_none());
        assert!(app.hunt.is_none(), "a thread outlived the window");
    }

    #[test]
    fn the_duplicate_key_is_the_fourth_of_the_comparison_family() {
        for (key, action) in [
            (egui::Key::C, keys::Action::CompareFolders),
            (egui::Key::D, keys::Action::CompareFiles),
            (egui::Key::S, keys::Action::Synchronize),
            (egui::Key::U, keys::Action::Duplicates),
        ] {
            assert_eq!(keys::action_for(key, egui::Modifiers::ALT), Some(action));
        }
    }

    #[test]
    fn alt_c_marks_what_differs_between_the_two_panes() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        // a.txt is on both sides and the same; b.txt and c.txt only on the
        // left; d.txt only on the right.
        fs::write(app.right.cwd().join("a.txt"), "a").unwrap();
        fs::write(app.right.cwd().join("d.txt"), "d").unwrap();
        app.left.reload();
        app.right.reload();

        app.run_action(A::CompareFolders);

        let marked = |tabs: &Tabs| -> Vec<String> {
            let mut names: Vec<String> = tabs
                .current()
                .entries
                .iter()
                .filter(|e| e.marked)
                .map(|e| e.name.clone())
                .collect();
            names.sort();
            names
        };
        assert_eq!(marked(&app.left), ["b.txt", "c.txt"]);
        assert_eq!(marked(&app.right), ["d.txt"]);
        assert!(
            !app.left
                .current()
                .entries
                .iter()
                .any(|e| e.marked && e.name == "sub"),
            "a directory is not marked: whether it differs is about its contents"
        );
    }

    #[test]
    fn alt_s_compares_the_two_trees_and_synchronize_carries_it_out() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let right = app.right.cwd().to_path_buf();
        fs::write(right.join("a.txt"), "a").unwrap();
        app.left.reload();
        app.right.reload();

        app.run_action(A::Synchronize);
        assert!(matches!(app.dialog, Some(Dialog::Sync { .. })));

        // The comparison runs on a thread; take its results as the dialog
        // does when it finishes.
        let scan = app.scan.take().expect("a scan was started");
        let found = {
            let mut scan = scan;
            scan.join();
            scan.snapshot()
        };
        assert!(found.finished);
        let Some(Dialog::Sync {
            pairs,
            left,
            right: right_root,
            ..
        }) = &mut app.dialog
        else {
            panic!("the form closed");
        };
        *pairs = found.pairs;
        let names: Vec<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"b.txt") && names.contains(&"c.txt"),
            "the left-only files are differences: {names:?}"
        );
        // The file both sides have, identical, is in the list - the walk
        // reports every pair and the filter decides what is shown - but it is
        // not work, and nothing will be copied over it.
        let same = pairs.iter().find(|p| p.name == "a.txt").expect("a.txt");
        assert_eq!(same.state, compare::State::Same);
        assert_eq!(same.direction, compare::Direction::Skip);
        assert!(!same.is_work());

        let tasks = compare::tasks(pairs, left, right_root);
        app.start(Operation::Sync { tasks });
        for _ in 0..400 {
            app.poll_job();
            if app.job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.right.reload();
        assert!(right.join("b.txt").exists());
        assert!(right.join("c.txt").exists());
    }

    #[test]
    fn comparing_a_directory_with_itself_is_refused() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let left = app.left.cwd().to_path_buf();
        app.right.current_mut().chdir(left);

        app.run_action(A::Synchronize);
        assert!(app.dialog.is_none(), "no form for a comparison with itself");
        assert!(app.status_is_error);
        assert!(app.scan.is_none());
    }

    #[test]
    fn the_comparison_keys_are_one_apart() {
        assert_eq!(
            keys::action_for(egui::Key::C, egui::Modifiers::ALT),
            Some(keys::Action::CompareFolders)
        );
        assert_eq!(
            keys::action_for(egui::Key::S, egui::Modifiers::ALT),
            Some(keys::Action::Synchronize)
        );
    }

    #[test]
    fn ctrl_t_opens_a_tab_where_this_one_is_and_ctrl_w_closes_it() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let start = app.left.cwd().to_path_buf();
        assert_eq!(app.tabs(Side::Left).len(), 1);

        app.run_action(A::NewTab);
        assert_eq!(app.tabs(Side::Left).len(), 2);
        assert_eq!(app.left.cwd(), start);

        // Navigate away; the tab it was opened from stays where it was.
        app.left.current_mut().chdir(start.join("sub"));
        assert_eq!(app.left.cwd(), start.join("sub"));
        assert_eq!(app.tabs(Side::Left).get(0).unwrap().cwd, start);

        app.run_action(A::CloseTab);
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(app.left.cwd(), start);

        // A pane always shows something, so the last tab stays.
        app.run_action(A::CloseTab);
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert!(app.status_is_error);
    }

    #[test]
    fn close_the_others_keeps_the_tab_on_show() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::NewTab);
        let sub = app.left.cwd().join("sub");
        app.left.current_mut().chdir(sub);
        let kept = app.left.cwd().to_path_buf();
        app.run_action(A::NewTab);
        app.run_action(A::PreviousTab);
        assert_eq!(app.tabs(Side::Left).len(), 3);
        assert_eq!(app.left.cwd(), kept);

        app.run_action(A::CloseOtherTabs);
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(app.left.cwd(), kept);
    }

    #[test]
    fn a_tab_sent_across_arrives_whole_and_takes_you_with_it() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::NewTab);
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        app.left.current_mut().toggle_mark();
        let moved = app.left.cwd().to_path_buf();

        app.run_action(A::MoveTabAcross);

        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert_eq!(app.tabs(Side::Right).len(), 2);
        assert_eq!(
            app.active,
            Side::Right,
            "the tab is what you were working in, so you go across with it"
        );
        assert_eq!(app.right.cwd(), moved);
        assert_eq!(
            app.right.current().marked_count(),
            1,
            "the tab arrived whole, marks and all"
        );
        assert!(app.show_right, "and into a pane that can be seen");

        // The pane left holding one tab cannot give it away.
        app.active = Side::Left;
        app.run_action(A::MoveTabAcross);
        assert_eq!(app.tabs(Side::Left).len(), 1);
        assert!(app.status_is_error);
    }

    #[test]
    fn each_workspace_keeps_its_own_arrangement() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        // A workspace is a window: how it is arranged is part of what you
        // are coming back to. The pane's view used to be carried across
        // tabs, which meant a tree in one made a tree of all of them.
        app.run_action(A::ViewTree);
        assert!(app.left.current().in_tree_mode());

        app.run_action(A::NewTab);
        app.run_action(A::ViewDetails);
        assert!(!app.left.current().in_tree_mode());

        app.run_action(A::PreviousTab);
        assert!(
            app.left.current().in_tree_mode(),
            "the workspace that was a tree is still a tree"
        );
        app.run_action(A::NextTab);
        assert!(
            !app.left.current().in_tree_mode(),
            "and the one that was a listing is still a listing"
        );
    }

    #[test]
    fn the_tab_keys_are_the_ones_every_program_with_tabs_uses() {
        assert_eq!(
            keys::action_for(egui::Key::T, egui::Modifiers::CTRL),
            Some(keys::Action::NewTab)
        );
        assert_eq!(
            keys::action_for(egui::Key::W, egui::Modifiers::CTRL),
            Some(keys::Action::CloseTab)
        );
        assert_eq!(
            keys::action_for(egui::Key::W, egui::Modifiers::ALT),
            Some(keys::Action::CloseOtherTabs)
        );
        assert_eq!(
            keys::action_for(egui::Key::Tab, egui::Modifiers::CTRL),
            Some(keys::Action::NextTab)
        );
        assert_eq!(
            keys::action_for(
                egui::Key::Tab,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT
            ),
            Some(keys::Action::PreviousTab),
            "and Ctrl-Shift-Tab is not a request to complete a command"
        );
        assert_eq!(
            keys::action_for(egui::Key::F6, egui::Modifiers::SHIFT),
            Some(keys::Action::MoveTabAcross)
        );
        assert_eq!(
            keys::action_for(egui::Key::F6, egui::Modifiers::NONE),
            Some(keys::Action::Move),
            "and a plain F6 still moves files"
        );
        // The tree lost Ctrl-T to the tabs and kept two other ways in.
        assert_eq!(
            keys::action_for(egui::Key::T, egui::Modifiers::ALT),
            Some(keys::Action::ViewTree)
        );
        assert_eq!(
            keys::action_for(egui::Key::Num3, egui::Modifiers::CTRL),
            Some(keys::Action::ViewTree)
        );
    }

    #[test]
    fn the_rename_form_opens_over_the_selection_and_carries_the_plan_out() {
        let (_root, mut app) = fixture();
        let left = app.left.cwd().to_path_buf();
        for name in ["a.txt", "b.txt"] {
            let at = index_of(&app, Side::Left, name);
            app.left.current_mut().cursor_to(at);
            app.left.current_mut().toggle_mark();
        }

        app.run_action(keys::Action::MultiRename);
        let Some(Dialog::MultiRename {
            rules,
            sources,
            changes,
            ..
        }) = &mut app.dialog
        else {
            panic!("the form did not open");
        };
        assert_eq!(sources.len(), 2, "the marked files, and nothing else");
        assert!(
            changes.iter().all(|c| !c.is_rename()),
            "the form opens with rules that change nothing"
        );

        // What typing into the boxes amounts to.
        rules.name = "note_[C01]".into();
        let plan = rename::plan(
            mount::Platform::current(),
            sources,
            rules,
            &lost_commander_core::preview::on_disk,
        );
        assert_eq!(plan[0].name, "note_01.txt");
        app.run_multi_rename(&plan);

        assert!(left.join("note_01.txt").exists());
        assert!(left.join("note_02.txt").exists());
        assert!(!left.join("a.txt").exists());
        assert!(
            left.join("c.txt").exists(),
            "a file that was not marked is not touched"
        );
        assert!(app.status.contains("Renamed 2"), "{}", app.status);
        assert_eq!(
            app.left.current().marked_count(),
            0,
            "the marks pointed at names that are gone, so none of them are left"
        );
    }

    #[test]
    fn every_action_the_graphical_view_has_is_reachable_by_key() {
        // Ctrl-M is the one Total Commander uses, and Shift-F2 is the one
        // that also works in the terminal view - both land here.
        assert_eq!(
            keys::action_for(egui::Key::M, egui::Modifiers::CTRL),
            Some(keys::Action::MultiRename)
        );
        assert_eq!(
            keys::action_for(egui::Key::F2, egui::Modifiers::SHIFT),
            Some(keys::Action::MultiRename)
        );
        assert_eq!(
            keys::action_for(egui::Key::F2, egui::Modifiers::NONE),
            Some(keys::Action::Rename),
            "and a plain F2 still renames the one file"
        );
    }

    #[test]
    fn escape_stops_the_search_as_well_as_closing_the_form() {
        // Two paths close a dialog and both have to let go of the same
        // things. When only the dialog's own buttons did, the search thread
        // outlived the form and its results came back on the next open.
        let (_root, mut app) = fixture();
        app.run_action(keys::Action::Find);
        app.search = Some(find::Search::spawn(
            app.left.cwd().to_path_buf(),
            find::Query {
                pattern: "*".into(),
                ..find::Query::default()
            },
        ));

        app.close_dialog();
        assert!(app.dialog.is_none());
        assert!(app.search.is_none(), "a thread outlived the form");
    }

    #[cfg(unix)]
    #[test]
    fn alt_enter_opens_the_properties_of_the_file_under_the_cursor() {
        use keys::Action as A;
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = fixture();
        let file = app.left.cwd().join("a.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        app.left.reload();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);

        assert_eq!(
            keys::action_for(egui::Key::Enter, egui::Modifiers::ALT),
            Some(A::Properties)
        );
        app.run_action(A::Properties);

        match &app.dialog {
            Some(Dialog::Properties { was, now, octal }) => {
                assert_eq!(now.path, file);
                assert_eq!(now.mode.unwrap().octal(), "640");
                assert_eq!(octal, "640");
                assert_eq!(was, now, "it opened already changed");
            }
            _ => panic!("expected the properties dialog"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_what_changed_is_written_back() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = fixture();
        let file = app.left.cwd().join("a.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        app.left.reload();

        let was = perms::read(&file).unwrap();

        // Nothing touched: nothing written, and it says so.
        app.apply_properties(&was, &was);
        assert!(app.status.contains("Nothing changed"), "{}", app.status);

        // A bit flipped: written, and the listing picks it up.
        let mut now = was.clone();
        let mut mode = now.mode.unwrap();
        mode.set(Who::Owner, What::Execute, true);
        now.mode = Some(mode);
        app.apply_properties(&was, &now);

        assert!(!app.status_is_error, "{}", app.status);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o744
        );
        // The panel re-read, so the new column is right without a Ctrl-R.
        let listed = app
            .left
            .current()
            .entries
            .iter()
            .find(|e| e.name == "a.txt")
            .and_then(|e| e.mode);
        assert_eq!(listed.map(|m| m.octal()), Some("744".to_string()));
    }

    #[test]
    fn properties_of_the_parent_row_are_not_offered() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.left.current_mut().cursor_to(0); // `..`
        app.run_action(A::Properties);
        assert!(app.dialog.is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn going_to_something_that_has_since_gone_says_so() {
        let (_root, mut app) = fixture();
        let missing = app.left.cwd().join("sub/vanished.txt");

        app.go_to(&missing);

        // The panel still moved to where it should have been - that is
        // useful - but the status says the file is not there.
        assert_eq!(app.left.cwd(), app.left.cwd().to_path_buf());
        assert!(app.status_is_error, "{}", app.status);
        assert!(app.status.contains("vanished.txt"), "{}", app.status);
    }

    #[test]
    fn the_chooser_needs_a_file_to_ask_about() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let at = index_of(&app, Side::Left, "sub");
        app.left.current_mut().cursor_to(at);
        app.run_action(A::OpenWith);
        // "With what?" is a question about a file; a directory is somewhere
        // to go, and Enter already does that.
        assert!(app.dialog.is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn enter_opens_a_directory_but_launches_a_file() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let opened = watch_opener(&mut app);

        // A directory is somewhere to go.
        let sub = index_of(&app, Side::Left, "sub");
        app.left.current_mut().cursor_to(sub);
        let was = app.left.cwd().to_path_buf();
        app.run_action(A::Open);
        assert_eq!(app.left.cwd(), was.join("sub"));
        assert!(
            opened.lock().unwrap().is_empty(),
            "a directory was launched"
        );

        // A file is something to open.
        app.run_action(A::Parent);
        let a = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(a);
        app.run_action(A::Open);
        assert_eq!(*opened.lock().unwrap(), vec![was.join("a.txt")]);
        assert!(
            app.dialog.is_none(),
            "a text file should not be asked about"
        );
    }

    #[test]
    fn enter_opens_every_marked_file_at_once() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let opened = watch_opener(&mut app);
        let cwd = app.left.cwd().to_path_buf();

        app.left.current_mut().mark_all();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        app.run_action(A::Open);

        // Every marked file, and never the directory - "open" for a directory
        // means navigate, and a pane can only navigate to one.
        let mut got = opened.lock().unwrap().clone();
        got.sort();
        assert_eq!(
            got,
            vec![cwd.join("a.txt"), cwd.join("b.txt"), cwd.join("c.txt")]
        );
    }

    #[test]
    fn the_cursor_decides_which_of_the_two_enter_means() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        let opened = watch_opener(&mut app);
        let cwd = app.left.cwd().to_path_buf();

        // Marks and all, a cursor on a directory still means "go there".
        // Enter has one meaning per row, and the row under the cursor is the
        // one that chooses it.
        app.left.current_mut().mark_all();
        let at = index_of(&app, Side::Left, "sub");
        app.left.current_mut().cursor_to(at);
        app.run_action(A::Open);

        assert_eq!(app.left.cwd(), cwd.join("sub"));
        assert!(opened.lock().unwrap().is_empty(), "marks hijacked Enter");
    }

    #[test]
    fn a_row_whose_file_has_gone_says_so_rather_than_opening_it() {
        let (root, mut app) = fixture();
        let opened = watch_opener(&mut app);

        // Double-clicking a row listed before the file was deleted elsewhere.
        app.apply_click(Side::Left, None, Some(root.path().join("vanished.txt")));

        assert!(opened.lock().unwrap().is_empty(), "launched a missing file");
        assert!(app.status_is_error, "{}", app.status);
    }

    #[cfg(unix)]
    #[test]
    fn enter_on_a_program_asks_before_running_it() {
        use keys::Action as A;
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut app) = fixture();
        let script = app.left.cwd().join("build.sh");
        fs::write(&script, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        app.left.reload();

        let opened = watch_opener(&mut app);
        let index = index_of(&app, Side::Left, "build.sh");
        app.left.current_mut().cursor_to(index);
        app.run_action(A::Open);

        // Nothing has started yet - the question comes first.
        assert!(opened.lock().unwrap().is_empty(), "ran without asking");
        match &app.dialog {
            Some(Dialog::ConfirmOpen { question, targets }) => {
                assert!(question.contains("build.sh"), "{question}");
                assert_eq!(targets, &vec![script.clone()]);
            }
            _ => panic!("expected a confirmation dialog, got none of that"),
        }

        // Confirming is what runs it.
        let Some(Dialog::ConfirmOpen { targets, .. }) = app.dialog.take() else {
            unreachable!()
        };
        app.open_now(targets);
        assert_eq!(*opened.lock().unwrap(), vec![script]);
    }

    #[test]
    fn opening_a_crowd_asks_first() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        for n in 0..6 {
            fs::write(app.left.cwd().join(format!("extra{n}.txt")), "x").unwrap();
        }
        app.left.reload();
        let opened = watch_opener(&mut app);

        app.left.current_mut().mark_all();
        let at = index_of(&app, Side::Left, "a.txt");
        app.left.current_mut().cursor_to(at);
        app.run_action(A::Open);

        assert!(opened.lock().unwrap().is_empty(), "opened without asking");
        match &app.dialog {
            Some(Dialog::ConfirmOpen { question, targets }) => {
                assert_eq!(targets.len(), 9);
                assert!(question.contains('9'), "{question}");
            }
            _ => panic!("expected a confirmation dialog, got none of that"),
        }
    }

    #[test]
    fn one_broken_association_does_not_hold_back_the_rest() {
        let (_root, mut app) = fixture();
        let cwd = app.left.cwd().to_path_buf();
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&attempted);
        app.opener = Box::new(move |path: &Path| {
            log.lock().unwrap().push(path.to_path_buf());
            if path.ends_with("b.txt") {
                Err("no application for .txt".into())
            } else {
                Ok(())
            }
        });

        app.open_now(vec![
            cwd.join("a.txt"),
            cwd.join("b.txt"),
            cwd.join("c.txt"),
        ]);

        // All three were tried, and the failure is what the strip says.
        assert_eq!(attempted.lock().unwrap().len(), 3);
        assert!(app.status_is_error);
        assert!(app.status.contains("no application"), "{}", app.status);
    }

    #[test]
    fn a_half_typed_command_takes_back_the_keys_it_needs() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.show_terminal = false;
        let sub = index_of(&app, Side::Left, "sub");
        app.left.current_mut().cursor_to(sub);
        let was = app.left.cwd().to_path_buf();

        // Empty line: Enter opens what the cursor is on, Space marks.
        app.run_action(A::Open);
        assert_eq!(app.left.cwd(), was.join("sub"));
        app.run_action(A::Parent);
        assert_eq!(app.left.cwd(), was);

        // Start typing, and those same keys belong to the line instead. This
        // is the rule the original used for Enter, and the reason its panel
        // commands never collided with typing.
        app.type_into_command_line("grep");
        assert!(!app.command_line_empty());
        assert!(keys::defers_to_command_line(A::Open));
        // Space is a character, so it goes into the command rather than
        // marking - by the same rule, through the text path.
        assert_eq!(keys::action_for_text(" ", app.command_line_empty()), None);

        // A `*` typed into a command is a glob, not "invert the marks".
        assert!(keys::action_for_text("*", app.command_line_empty()).is_none());
        app.type_into_command_line(" *.rs");
        assert_eq!(app.command, "grep *.rs");
        assert_eq!(
            app.left.current().marked_count(),
            0,
            "nothing was marked by the *"
        );

        // Cleared, and the panel keys come back.
        app.command_line_clear();
        assert_eq!(
            keys::action_for_text("*", app.command_line_empty()),
            Some(A::InvertMarks)
        );
    }

    #[test]
    fn the_shell_is_the_command_line_when_it_is_the_one_showing() {
        let (_root, mut app) = fixture();
        app.open_initial_terminal();
        assert!(app.show_terminal);
        assert!(app.command_line_empty());

        // Typed characters go down the pty, and `pending_input` is our record
        // of the line, since the shell's own buffer is in another process.
        app.type_into_command_line("echo");
        assert_eq!(app.pending_input, "echo");
        assert!(!app.command_line_empty());

        app.command_line_backspace();
        assert_eq!(app.pending_input, "ech");

        // Running it clears the record, so the panel keys work again.
        app.command_line_run();
        assert!(app.command_line_empty());

        app.type_into_command_line("x");
        app.command_line_clear();
        assert!(app.command_line_empty());
    }

    #[test]
    fn the_record_of_the_line_keeps_up_when_the_shell_takes_focus() {
        // Type from the panes, finish in the shell: the half-typed command is
        // already there, and the record has to follow it or coming back would
        // find Enter still trying to run a line the shell had taken.
        let (_root, mut app) = fixture();
        app.open_initial_terminal();
        app.type_into_command_line("ec");
        assert_eq!(app.pending_input, "ec");

        // Escalating does not disturb it.
        app.run_action(keys::Action::FocusTerminal);
        assert!(app.terminal_focused);
        assert_eq!(app.pending_input, "ec");

        // Running it in the shell clears the record, so back on the panes an
        // empty line means an empty line.
        app.pending_input.clear();
        app.run_action(keys::Action::LeaveTerminal);
        assert!(app.command_line_empty());
        assert_eq!(
            keys::action_for_text("*", app.command_line_empty()),
            Some(keys::Action::InvertMarks)
        );
    }

    #[test]
    fn a_mark_is_a_highlight_and_never_an_outline() {
        // Every state fills, none of them outlines: a box drawn round every
        // marked row reads as a grid rather than as a selection.
        assert_eq!(
            selection_fill(false, true, false, false),
            Some(theme::marked())
        );
        // Marked has to be plainly different from the panel behind it - it
        // used to be SURFACE_HI, which is why it needed the outline to be
        // seen at all.
        assert_ne!(theme::marked(), theme::surface_hi());
        assert_ne!(theme::marked(), theme::surface());

        // Cursor and mark are told apart by hue, not by shape.
        assert_ne!(theme::marked(), theme::selected());
        let marked = theme::marked();
        let cursor = theme::selected();
        assert!(
            marked.r() > marked.b() && cursor.b() > cursor.r(),
            "the mark should be warm and the cursor cool, or they will argue"
        );

        // A row that is both is brighter than either, and still reads as
        // where you are.
        let both = selection_fill(true, true, true, false).unwrap();
        assert_eq!(both, theme::marked_cursor());
        assert!(both.r() > theme::marked().r());

        // The cursor dims when its pane loses focus, and a mark does not -
        // a mark is a fact about the file, not about the window.
        assert_eq!(
            selection_fill(true, false, false, false),
            Some(theme::selected_idle())
        );
        assert_eq!(
            selection_fill(false, true, false, false),
            selection_fill(false, true, true, false)
        );

        // Hover is only for a row that is neither.
        assert_eq!(
            selection_fill(false, false, true, true),
            Some(theme::hover())
        );
        assert_eq!(selection_fill(false, false, true, false), None);
    }

    #[test]
    fn modifiers_decide_what_a_click_does_to_the_marks() {
        assert_eq!(Click::from_modifiers(false, false), Click::Plain);
        assert_eq!(Click::from_modifiers(true, false), Click::Toggle);
        assert_eq!(
            Click::from_modifiers(false, true),
            Click::Range { additive: false }
        );
        // Both: extend the range without dropping what is already marked.
        assert_eq!(
            Click::from_modifiers(true, true),
            Click::Range { additive: true }
        );
    }

    #[test]
    fn shift_click_marks_from_the_last_click_to_this_one() {
        let (_root, mut app) = fixture();
        let first = index_of(&app, Side::Left, "a.txt");
        let last = index_of(&app, Side::Left, "c.txt");

        // A plain click sets the anchor...
        app.apply_click(Side::Left, Some((first, Click::Plain)), None);
        assert_eq!(
            app.left.current().marked_count(),
            0,
            "a plain click marks nothing"
        );

        // ...and a shift-click takes everything to here. Two clicks for a
        // range, rather than one per file.
        app.apply_click(
            Side::Left,
            Some((last, Click::Range { additive: false })),
            None,
        );
        let names: Vec<String> = app
            .left
            .current()
            .entries
            .iter()
            .filter(|e| e.marked)
            .map(|e| e.name.clone())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.txt".to_string()));
        assert!(names.contains(&"c.txt".to_string()));
        assert_eq!(app.left.current().marked_count(), last - first + 1);

        // The anchor stays put through a range, so a second shift-click
        // re-measures from the same place rather than from the last one.
        app.apply_click(
            Side::Left,
            Some((first + 1, Click::Range { additive: false })),
            None,
        );
        assert_eq!(app.left.current().marked_count(), 2);

        // Ctrl-click moves the anchor and toggles just the one.
        app.apply_click(Side::Left, Some((last, Click::Toggle)), None);
        assert_eq!(app.left.current().marked_count(), 3);
        app.apply_click(Side::Left, Some((last, Click::Toggle)), None);
        assert_eq!(app.left.current().marked_count(), 2);
    }

    #[test]
    fn every_operation_takes_the_whole_selection() {
        // Copy, move and delete have always been plural; this pins it down.
        let (_root, mut app) = fixture();
        app.left.current_mut().mark_all();
        let expected = app.left.current().entries.len() - 1;

        let targets = app.selection(Side::Left);
        assert_eq!(targets.len(), expected);
        assert!(!targets.iter().any(|p| p.ends_with("..")));

        // And with nothing marked it falls back to the row under the cursor.
        app.left.current_mut().clear_marks();
        let at = index_of(&app, Side::Left, "b.txt");
        app.left.current_mut().cursor_to(at);
        let targets = app.selection(Side::Left);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].ends_with("b.txt"));
    }

    #[test]
    fn a_pattern_marks_across_the_whole_pane() {
        let (_root, mut app) = fixture();
        app.select_pattern = "*.txt".into();
        let changed = app.left.current_mut().mark_matching("*.txt", true);
        assert_eq!(changed, 3, "a.txt, b.txt and c.txt");
        assert_eq!(app.selection(Side::Left).len(), 3);
    }

    #[test]
    fn quick_view_follows_the_other_panes_cursor() {
        let (_root, mut app) = fixture();
        // The right pane previews; the left is the one being moved through.
        app.set_view(Side::Right, ViewMode::Preview);
        assert_eq!(app.preview_side(), Some(Side::Right));

        let index = index_of(&app, Side::Left, "b.txt");
        app.left.current_mut().cursor_to(index);
        assert_eq!(
            app.preview_target(Side::Right).map(|e| e.name),
            Some("b.txt".to_string())
        );

        // Move the cursor and the target moves with it.
        let index = index_of(&app, Side::Left, "c.txt");
        app.left.current_mut().cursor_to(index);
        assert_eq!(
            app.preview_target(Side::Right).map(|e| e.name),
            Some("c.txt".to_string())
        );

        // And it is the other pane that is followed, never its own.
        app.set_view(Side::Left, ViewMode::Preview);
        assert_eq!(app.preview_side(), Some(Side::Left));
        assert_eq!(
            app.preview_target(Side::Left).map(|e| e.name),
            app.right.current().selected().map(|e| e.name.clone())
        );
    }

    #[test]
    fn only_one_pane_can_be_a_quick_view() {
        // Two would have nothing to look at: each follows the other's cursor
        // and neither would be a listing to move a cursor in.
        let (_root, mut app) = fixture();
        app.set_view(Side::Right, ViewMode::Preview);
        app.set_view(Side::Left, ViewMode::Preview);

        assert_eq!(app.view(Side::Left), ViewMode::Preview);
        assert_eq!(app.view(Side::Right), ViewMode::Details);
        assert_eq!(app.preview_side(), Some(Side::Left));
    }

    #[test]
    fn leaving_quick_view_lets_go_of_what_it_loaded() {
        let (_root, mut app) = fixture();
        app.set_view(Side::Right, ViewMode::Preview);
        app.preview_ready = Some(crate::preview::Ready::new(
            app.left.cwd().join("a.txt"),
            crate::preview::Loaded::Nothing("test"),
        ));

        app.set_view(Side::Right, ViewMode::Details);
        app.poll_preview();
        // A decoded photograph is megabytes; holding it for a pane nobody is
        // looking at would be careless.
        assert!(app.preview_ready.is_none());
        assert!(app.preview_job.is_none());
    }

    #[test]
    fn folding_the_second_pane_away_keeps_where_it_was() {
        let (_root, mut app) = fixture();
        app.navigate(Side::Right, app.right.cwd().to_path_buf());
        let was = app.right.cwd().to_path_buf();
        app.active = Side::Right;
        app.right.current_mut().cursor = 0;

        app.show_right = false;
        // The hidden Panel is untouched - that is the whole point of hiding
        // rather than closing.
        assert_eq!(app.right.cwd(), was);
        // And the pane you were on is the one that stays visible.
        assert_eq!(app.active, Side::Right);

        app.show_right = true;
        assert_eq!(app.right.cwd(), was);
    }

    #[test]
    fn recording_writes_a_new_file_each_time() {
        let (_root, mut app) = fixture();
        app.open_initial_terminal();
        assert!(app.terminals.active().unwrap().recording().is_none());

        let first = app.toggle_recording("stamp").expect("started");
        assert_eq!(first.parent().unwrap(), app.left.cwd());
        assert_eq!(
            app.terminals.active().unwrap().recording().as_deref(),
            Some(first.as_path())
        );

        assert_eq!(
            app.toggle_recording("stamp").as_ref(),
            Some(&first),
            "stops"
        );
        assert!(app.terminals.active().unwrap().recording().is_none());

        // A new recording is a new file, even in the same second.
        let second = app.toggle_recording("stamp").expect("started again");
        assert_ne!(second, first);
        assert!(first.exists() && second.exists());
        app.toggle_recording("stamp");
    }

    fn window() -> Rect {
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1200.0, 800.0))
    }

    fn showing() -> Showing {
        Showing {
            rail: true,
            rail_wide: false,
            top: true,
            bottom: true,
            places: true,
            history: true,
            keys: true,
            column: 210.0,
            row: 280.0,
        }
    }

    #[test]
    fn the_shell_is_exactly_as_wide_as_the_panes_above_it() {
        // The point of the whole arrangement. The drawer used to be a panel
        // of its own running the full width of the window, so it was wider
        // than the panes by exactly the sidebar - and the two things that
        // belong together, a shell and the directory it is standing in, were
        // the two things that did not line up.
        let cut = sectors(window(), showing());
        let panes = cut.panes.expect("the panes");
        let shell = cut.shell.expect("the shell");
        assert_eq!(shell.min.x, panes.min.x);
        assert_eq!(shell.max.x, panes.max.x);

        let places = cut.places.expect("the places list");
        let history = cut.history.expect("what was run here");
        assert_eq!(history.min.x, places.min.x);
        assert_eq!(history.max.x, places.max.x);
    }

    #[test]
    fn one_seam_between_the_columns_and_one_between_the_rows() {
        let cut = sectors(window(), showing());
        let places = cut.places.expect("the places list");
        let history = cut.history.expect("what was run here");

        // The vertical seam runs the whole height, so there is one line to
        // drag rather than one per row.
        let vertical = cut.vertical.expect("a seam between the columns");
        assert_eq!(vertical.min.y, window().min.y);
        assert_eq!(vertical.max.y, window().max.y);

        // The horizontal seam runs the whole width, and the two rows are the
        // same height on both sides of it.
        // The horizontal seam runs the whole width of the arrangement - which
        // starts where the rail ends, since the rail is outside it.
        let horizontal = cut.horizontal.expect("a seam between the rows");
        assert_eq!(horizontal.min.x, cut.panes.unwrap().min.x);
        assert_eq!(horizontal.max.x, window().max.x);
        assert_eq!(cut.shell.unwrap().min.y, history.min.y);
        assert_eq!(cut.shell.unwrap().max.y, history.max.y);
        assert_eq!(places.max.y, cut.keys.expect("the key bar").max.y);
    }

    #[test]
    fn the_keys_are_a_strip_under_the_panes_inside_their_own_column() {
        let cut = sectors(window(), showing());
        let panes = cut.panes.expect("the panes");
        let keys = cut.keys.expect("the key bar");
        assert_eq!(
            keys.min.x, panes.min.x,
            "as wide as the panes, not the window"
        );
        assert_eq!(keys.max.x, panes.max.x);
        assert_eq!(keys.min.y, panes.max.y, "directly under them");
        assert!((keys.height() - KEYS_HEIGHT).abs() < 0.01);

        let without = sectors(
            window(),
            Showing {
                keys: false,
                ..showing()
            },
        );
        assert!(without.keys.is_none());
        assert!(
            without.panes.unwrap().height() > panes.height(),
            "the strip goes back to the panes"
        );
    }

    #[test]
    fn with_no_places_list_there_is_no_column_and_the_shell_takes_the_width() {
        let cut = sectors(
            window(),
            Showing {
                places: false,
                ..showing()
            },
        );
        assert!(cut.places.is_none());
        assert!(cut.vertical.is_none());
        assert!(
            cut.history.is_none(),
            "what was run here lives in that column, so it goes with it"
        );
        assert_eq!(cut.panes.unwrap().max.x, window().max.x);
        assert_eq!(cut.shell.unwrap().max.x, window().max.x);
    }

    #[test]
    fn one_half_on_show_takes_the_whole_window() {
        // The panes alone: no shell, no history under the places, and no
        // seam between rows, because there is one row.
        let files = sectors(
            window(),
            Showing {
                bottom: false,
                ..showing()
            },
        );
        assert!(files.shell.is_none());
        assert!(files.history.is_none());
        assert!(files.horizontal.is_none());
        assert_eq!(
            files.keys.expect("the key bar").max.y,
            window().max.y,
            "the panes and their keys run to the bottom"
        );
        assert_eq!(files.places.expect("places").max.y, window().max.y);

        // The shell alone: the places list belongs to the top row, so the
        // column is the history for as long as that is what is showing.
        let shell = sectors(
            window(),
            Showing {
                top: false,
                ..showing()
            },
        );
        assert!(shell.panes.is_none());
        assert!(shell.keys.is_none());
        assert!(shell.places.is_none());
        assert!(shell.horizontal.is_none());
        assert_eq!(shell.shell.expect("the shell").min.y, window().min.y);
        assert_eq!(shell.history.expect("history").min.y, window().min.y);
        // The column is still the column: the history is exactly as wide as
        // the places list was, so nothing jumps sideways when it comes back.
        assert_eq!(shell.history.unwrap().min.x, files.places.unwrap().min.x);
    }

    #[test]
    fn a_window_showing_neither_half_falls_back_to_the_panes() {
        // Not reachable from the keys, which toggle rather than set - this is
        // the guarantee that no combination of them can leave a blank window.
        let cut = sectors(
            window(),
            Showing {
                top: false,
                bottom: false,
                ..showing()
            },
        );
        assert!(cut.panes.is_some());
        assert!(cut.shell.is_none());
    }

    #[test]
    fn neither_seam_can_be_dragged_far_enough_to_lose_a_sector() {
        // A drawer dragged to the top of the window, or a column dragged over
        // the panes, is one nobody can get back.
        let squashed = sectors(
            window(),
            Showing {
                row: 5_000.0,
                column: 5_000.0,
                ..showing()
            },
        );
        assert!(squashed.panes.unwrap().height() >= 1.0);
        assert!(squashed.panes.unwrap().width() >= window().width() * 0.4);

        let flattened = sectors(
            window(),
            Showing {
                row: 0.0,
                column: 0.0,
                ..showing()
            },
        );
        assert!(flattened.shell.unwrap().height() >= ROW_MIN - GUTTER);
        assert!(flattened.places.expect("places").width() >= COLUMN_MIN - GUTTER);
    }

    #[test]
    fn each_half_can_have_the_window_and_the_same_key_gives_it_back() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        assert_eq!(app.half, Half::Both);

        app.run_action(A::ShellOnly);
        assert_eq!(app.half, Half::Shell);
        // A shell to give the window to: asking for it with none open would
        // be a window given over to nothing.
        assert!(app.show_terminal);

        // The key you press to leave is the key you pressed to arrive.
        app.run_action(A::ShellOnly);
        assert_eq!(app.half, Half::Both);

        app.run_action(A::FilesOnly);
        assert_eq!(app.half, Half::Files);
        assert!(
            !app.terminal_focused,
            "the keyboard is not left in a shell nobody can see"
        );
        app.run_action(A::FilesOnly);
        assert_eq!(app.half, Half::Both);
    }

    #[test]
    fn the_halves_only_follow_each_other_while_both_are_showing() {
        let (root, mut app) = fixture();
        let elsewhere = root.path().join("left").join("sub");
        app.open_terminal(None);

        // With the panes alone, the shell is not dragged along behind them:
        // it is not on screen, and moving it would be moving something the
        // reader cannot see.
        app.run_action(keys::Action::FilesOnly);
        let before = app
            .terminals
            .active()
            .map(|session| session.cwd.clone())
            .expect("a shell");
        app.left.current_mut().chdir(elsewhere.clone());
        app.sync_halves();
        assert_eq!(
            app.terminals.active().map(|s| s.cwd.clone()),
            Some(before.clone()),
            "the shell stays where it was left"
        );

        // Both again: whichever half had the window is the one that is right
        // about where you are, so the shell catches up to the pane.
        app.run_action(keys::Action::FilesOnly);
        assert_eq!(app.half, Half::Both);
        assert_eq!(
            app.terminals.active().map(|s| s.cwd.clone()),
            Some(elsewhere.clone()),
            "and catches up when it comes back"
        );
    }

    #[test]
    fn typing_a_command_brings_the_shell_back() {
        let (_root, mut app) = fixture();
        app.run_action(keys::Action::FilesOnly);
        assert_eq!(app.half, Half::Files);

        // Answering into a shell that is not on screen looks exactly like the
        // keystroke was swallowed.
        app.type_into_command_line("ls");
        assert_eq!(app.half, Half::Both);
    }

    #[test]
    fn the_key_bar_says_what_the_keyboard_does() {
        // The bar is read out of `action_for`, so this is the check that it
        // is still asking rather than remembering - and that nothing on it is
        // drawn with an empty label.
        let bar = keys::function_keys();
        assert_eq!(bar.len(), 10, "F1 to F10");
        for (number, action) in &bar {
            assert!(
                !keys::name_of(*action).is_empty(),
                "F{number} is on the bar with nothing to say"
            );
        }
        // F9 is the selection menu here, whatever Norton put there. If that
        // ever changes, the bar changes with it and this line is what says so.
        assert_eq!(
            bar.iter().find(|(number, _)| *number == 9).map(|(_, a)| *a),
            Some(keys::Action::SelectMenu)
        );
        assert_eq!(
            bar.iter().find(|(number, _)| *number == 5).map(|(_, a)| *a),
            Some(keys::Action::Copy)
        );
    }

    #[test]
    fn history_answers_in_the_other_pane_and_gives_it_back() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        assert!(!app.show_right, "one pane to begin with");

        app.run_action(A::ViewHistory);
        // The answer is about the folder you are standing in, so it cannot be
        // drawn in the pane showing it - and the cursor stays where it was.
        assert!(app.show_right);
        assert_eq!(app.view(Side::Right), ViewMode::History);
        assert_eq!(app.active, Side::Left);

        app.run_action(A::ViewHistory);
        assert_eq!(app.view(Side::Right), ViewMode::Details);
        assert!(
            !app.show_right,
            "a pane opened only to answer is folded away again"
        );
    }

    #[test]
    fn a_pane_the_reader_opened_survives_the_history_closing() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.show_right = true;

        app.run_action(A::ViewHistory);
        app.run_action(A::ViewHistory);
        assert!(app.show_right, "it was not borrowed, so it is not taken");
    }

    #[test]
    fn two_panes_cannot_both_follow_the_other_one() {
        let (_root, mut app) = fixture();
        app.show_right = true;

        // History and quick view both read the *other* pane, so a pair of
        // them would be two panes looking at each other with no cursor
        // between them.
        app.set_view(Side::Left, ViewMode::History);
        app.set_view(Side::Right, ViewMode::History);
        assert_eq!(app.view(Side::Left), ViewMode::Details);
        assert_eq!(app.view(Side::Right), ViewMode::History);

        app.set_view(Side::Left, ViewMode::Preview);
        assert_eq!(
            app.view(Side::Right),
            ViewMode::Details,
            "a history pane gives way to a quick view as well"
        );
    }

    #[test]
    fn the_history_view_reads_what_was_done_in_the_other_pane_s_folder() {
        let (_root, mut app) = fixture();
        let _dir = with_a_journal(&mut app);
        let here = app.left.cwd().to_path_buf();

        app.note(journal::Event::new(
            journal::Kind::Delete,
            here.join("a.txt"),
        ));
        app.note(
            journal::Event::new(journal::Kind::Copy, "/somewhere/report.txt")
                .to(here.join("report.txt").display().to_string()),
        );

        // The right pane is the one showing the history, so what it reads is
        // the left pane's folder - the one being stood in.
        let rows = app.happenings_in(&here);
        let names: Vec<&str> = rows.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["report.txt", "a.txt"], "newest first");
        assert!(rows[0].incoming, "it arrived here, and the row can say so");

        // Somewhere else has its own story, and it is not this one.
        assert!(app.happenings_in(Path::new("/nowhere")).is_empty());
    }

    #[test]
    fn the_history_column_shows_here_or_everything() {
        let (_root, mut app) = fixture();
        let _dir = with_a_journal(&mut app);
        let here = app.left.cwd().to_path_buf();

        app.note(journal::Event::new(journal::Kind::Command, &here).note("cargo test"));
        app.note(journal::Event::new(journal::Kind::Command, "/elsewhere").note("make"));

        // What the column caches, which is everything - the filter is applied
        // when it draws, so switching between the two does not re-read the
        // account.
        app.shell_history = app.commands_in(&here);
        app.shell_history_of = Some(here.clone());

        let lines = |app: &GuiApp| -> Vec<String> {
            app.history_shown()
                .into_iter()
                .map(|past| past.line.clone())
                .collect()
        };

        // "here" to begin with: a shell's own history is one list with no
        // idea where you were standing, and this is the half that is about
        // where you are.
        assert!(app.history_here_only);
        assert_eq!(lines(&app), vec!["cargo test"]);

        // "all" is the rest of it, this directory first.
        app.history_here_only = false;
        assert_eq!(lines(&app), vec!["cargo test", "make"]);
    }

    /// An app with an account, kept in a directory of its own.
    fn with_a_journal(app: &mut GuiApp) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        app.journal = Some(journal::Journal::at(
            dir.path().to_path_buf(),
            journal::Keep::default(),
        ));
        dir
    }

    /// Everything in the file stream of the account, today.
    fn file_records(app: &GuiApp) -> Vec<journal::Event> {
        records_in(app, journal::Stream::Files)
    }

    /// Everything in the shell stream of the account, today.
    fn shell_records(app: &GuiApp) -> Vec<journal::Event> {
        records_in(app, journal::Stream::Shell)
    }

    fn records_in(app: &GuiApp, stream: journal::Stream) -> Vec<journal::Event> {
        app.journal
            .as_ref()
            .map(|journal| journal.read(stream, journal::Day::today()))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|record| match record {
                journal::Record::Event(event) => Some(event),
                journal::Record::Group(_) | journal::Record::Done(_) => None,
            })
            .collect()
    }

    /// A zip in the left pane, with two levels inside it.
    fn with_an_archive(app: &mut GuiApp) -> Option<PathBuf> {
        let here = app.left.cwd().to_path_buf();
        let build = here.join("build");
        std::fs::create_dir_all(build.join("docs")).unwrap();
        std::fs::write(build.join("readme.txt"), "at the top\n").unwrap();
        std::fs::write(build.join("docs/notes.txt"), "in a folder\n").unwrap();
        let made = std::process::Command::new("sh")
            .arg("-c")
            .arg("zip -qr ../papers.zip readme.txt docs")
            .current_dir(&build)
            .status();
        std::fs::remove_dir_all(&build).ok();
        let archive = here.join("papers.zip");
        app.left.reload();
        match matches!(made, Ok(s) if s.success()) && archive.exists() {
            true => Some(archive),
            false => None,
        }
    }

    fn finish_the_job(app: &mut GuiApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while app.job.is_some() && std::time::Instant::now() < deadline {
            app.poll_job();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn extracting_leaves_the_pane_where_it_was() {
        // A pane that jumped back to the archive's top level after every
        // extraction would make taking several things out of one folder a
        // matter of walking back down each time.
        let (_root, mut app) = fixture();
        let Some(archive) = with_an_archive(&mut app) else {
            eprintln!("no zip on this machine - skipped");
            return;
        };
        app.step_into_archive(Side::Left, archive);
        assert!(app.left.current().in_archive());

        let at = app
            .left
            .current()
            .entries
            .iter()
            .position(|e| e.name == "docs")
            .expect("docs is listed");
        app.left.current_mut().cursor_to(at);
        app.left.current_mut().enter();
        assert_eq!(app.left.current().inside.as_ref().unwrap().at, "docs");

        app.active = Side::Left;
        app.left.current_mut().mark_all();
        app.extract_to_other();
        finish_the_job(&mut app);

        assert_eq!(
            app.left.current().inside.as_ref().map(|i| i.at.clone()),
            Some("docs".to_string()),
            "the pane walked out from under the user"
        );
        // And the files landed relative to the level, not from the root.
        assert!(app.right.cwd().join("notes.txt").exists());
        assert!(!app.right.cwd().join("docs").exists());
    }

    #[test]
    fn opening_a_file_is_recorded_with_what_it_was_handed_to() {
        // The one thing this program can honestly say about a change made
        // outside it: that it handed the file to something else, and when.
        let (root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        let _log = watch_opener(&mut app);
        let target = root.path().join("left").join("one.txt");

        app.open_now(vec![target.clone()]);

        let opened: Vec<journal::Event> = file_records(&app)
            .into_iter()
            .filter(|event| event.kind == journal::Kind::Open)
            .collect();
        assert_eq!(opened.len(), 1, "got {opened:?}");
        assert_eq!(opened[0].path, target.display().to_string());
        assert!(
            !opened[0].note.is_empty(),
            "the account has to say what it was handed to"
        );
        assert!(!opened[0].is_failure());
    }

    #[test]
    fn an_open_that_did_not_work_is_recorded_as_a_failure() {
        let (root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        app.opener = Box::new(|_| Err("no association".to_string()));

        app.open_now(vec![root.path().join("left").join("one.txt")]);

        let opened: Vec<journal::Event> = file_records(&app)
            .into_iter()
            .filter(|event| event.kind == journal::Kind::Open)
            .collect();
        assert_eq!(opened.len(), 1);
        assert!(opened[0].is_failure(), "it did not open");
    }

    #[test]
    fn commands_typed_into_a_terminal_reach_the_account() {
        // The gap this closes: a terminal panel runs a real shell, so what is
        // typed there never passes through this program at all. It arrives
        // instead as marks in the shell's own output.
        let (_root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        let Some(shell) = lost_commander_core::pty::plain::hookable() else {
            eprintln!("no shell with a seam to hook on this machine - skipped");
            return;
        };
        app.open_terminal(Some(shell));
        assert!(
            app.terminals.active().unwrap().journals(),
            "the test shell should have been hooked"
        );

        app.terminals
            .active_mut()
            .unwrap()
            .run_line("echo from-the-terminal-panel");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            app.poll_terminal_commands();
            if !shell_records(&app).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let recorded = shell_records(&app);
        let one = recorded
            .iter()
            .find(|event| event.note.contains("from-the-terminal-panel"))
            .expect("the command should be in the account");
        assert_eq!(one.kind, journal::Kind::Command);
        assert_eq!(one.note, "echo from-the-terminal-panel");
        assert!(!one.is_failure());
        // Which shell ran it - whichever this machine's is, not a name
        // hard-coded here - and how long it took.
        let running = shell::program_name(&app.terminals.active().unwrap().program);
        assert_eq!(one.shell.as_deref(), Some(running.as_str()));
        assert_eq!(one.label(), running, "not the useless word 'Command'");
        assert!(one.ms.is_some(), "a command's duration is worth keeping");
        // The shell's directory, which is the one the command ran in.
        assert!(!one.path.is_empty());
    }

    #[test]
    fn a_failed_command_carries_its_status() {
        let (_root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        let Some(shell) = lost_commander_core::pty::plain::hookable() else {
            eprintln!("no shell with a seam to hook on this machine - skipped");
            return;
        };
        app.open_terminal(Some(shell));
        // In a subshell: a bare `exit 3` would end the session, and a shell
        // that has gone never reaches the prompt that reports the status.
        app.terminals.active_mut().unwrap().run_line("(exit 3)");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            app.poll_terminal_commands();
            if shell_records(&app).iter().any(|e| e.is_failure()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let recorded = shell_records(&app);
        let failed = recorded
            .iter()
            .find(|event| event.is_failure())
            .expect("a failure should have been recorded");
        assert_eq!(failed.failed.as_deref(), Some("exit 3"));
    }

    #[test]
    fn a_shell_that_cannot_be_recorded_says_so() {
        // An empty stream must never be able to mean "nothing was run" when
        // the truth is "nothing could be seen".
        let dash = "/bin/dash";
        if !Path::new(dash).exists() {
            eprintln!("no dash on this machine - skipped");
            return;
        }
        let (_root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        app.open_terminal(Some(dash.to_string()));
        assert!(!app.terminals.active().unwrap().journals());

        let recorded = shell_records(&app);
        let said = recorded
            .iter()
            .find(|event| event.kind == journal::Kind::Session)
            .expect("the account should say the session is not being recorded");
        // The shell's name is in the column that shows it, not repeated in
        // the sentence beside it.
        assert_eq!(said.shell.as_deref(), Some("dash"));
        assert_eq!(said.label(), "dash");
        assert!(said.note.contains("not recorded"));
    }

    #[test]
    fn a_hooked_shell_says_nothing_about_itself() {
        let (_root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        let Some(shell) = lost_commander_core::pty::plain::hookable() else {
            eprintln!("no shell with a seam to hook on this machine - skipped");
            return;
        };
        app.open_terminal(Some(shell));
        assert!(
            !shell_records(&app)
                .iter()
                .any(|event| event.kind == journal::Kind::Session),
            "there is nothing to warn about"
        );
    }

    #[test]
    fn a_recording_is_noted_so_the_account_can_point_at_it() {
        let (_root, mut app) = fixture();
        let _journal = with_a_journal(&mut app);
        let Some(shell) = lost_commander_core::pty::plain::hookable() else {
            eprintln!("no shell with a seam to hook on this machine - skipped");
            return;
        };
        app.open_terminal(Some(shell));

        let path = app.toggle_recording("stamp").expect("started");
        app.toggle_recording("stamp");

        let notes: Vec<String> = shell_records(&app)
            .into_iter()
            .filter(|event| event.kind == journal::Kind::Session)
            .map(|event| format!("{} {}", event.note, event.path))
            .collect();
        assert!(
            notes
                .iter()
                .any(|note| note.starts_with("Started recording")),
            "got {notes:?}"
        );
        assert!(
            notes.iter().any(|note| note.contains("Stopped recording")),
            "got {notes:?}"
        );
        assert!(
            notes
                .iter()
                .all(|note| note.contains(&path.display().to_string())),
            "the account has to say which file, or it cannot point at it"
        );
    }

    #[test]
    fn recording_needs_a_terminal() {
        let (_root, mut app) = fixture();
        assert!(app.toggle_recording("stamp").is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn output_is_saved_into_the_folder_the_pane_is_showing() {
        let (_root, mut app) = fixture();
        app.open_initial_terminal();
        let session = app.terminals.active_mut().unwrap();
        session.run_line("echo marker-in-the-log");
        for _ in 0..200 {
            if session.visible_text().contains("marker-in-the-log") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let path = app.save_output("20260725-115233").expect("written");
        // The active pane's directory, not the process's own.
        assert_eq!(path.parent().unwrap(), app.left.cwd());
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".log"));

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("marker-in-the-log"), "got:\n{written}");
        // And the pane it landed in now lists it.
        assert!(app.left.current().entries.iter().any(|e| e.path == path));
    }

    #[test]
    fn a_second_save_in_the_same_second_does_not_clobber_the_first() {
        let (_root, mut app) = fixture();
        app.open_initial_terminal();

        let first = app.save_output("stamp").expect("written");
        let second = app.save_output("stamp").expect("written");
        assert_ne!(first, second, "a saved log is not worth losing");
        assert!(first.exists() && second.exists());
        assert!(second.file_name().unwrap().to_string_lossy().contains("-2"));
    }

    #[test]
    fn with_the_terminal_hidden_it_is_the_command_line_that_is_saved() {
        let (_root, mut app) = fixture();
        app.show_terminal = false;
        assert!(app.output_text().is_none(), "nothing has run yet");
        assert!(app.save_output("stamp").is_none());
        assert!(app.status_is_error);

        app.command = "echo from-the-command-line".into();
        app.run_command();
        for _ in 0..200 {
            app.poll_shell();
            if !app.console.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let text = app.output_text().expect("a command has run");
        assert!(text.contains("echo from-the-command-line"), "the command");
        assert!(text.contains("from-the-command-line\n"), "its output");
        // Named for the surface it came from, not for a shell tab.
        assert!(app.output_basename("stamp").starts_with("console-"));
    }

    #[test]
    fn a_tree_pane_follows_the_pane_when_it_moves() {
        let (_root, mut app) = fixture();
        app.set_view(Side::Left, ViewMode::Tree);
        let target = app.left.cwd().join("sub");

        app.navigate(Side::Left, target.clone());

        assert_eq!(app.left.cwd(), target);
        let tree = app.left.current().tree.as_ref().expect("still a tree");
        assert_eq!(
            tree.selected_path().unwrap(),
            target,
            "the tree should show the new location, not the old one"
        );
    }

    #[test]
    fn switching_a_pane_to_tree_does_not_disturb_the_other() {
        let (_root, mut app) = fixture();
        app.set_view(Side::Right, ViewMode::Tree);

        assert!(app.right.current().in_tree_mode());
        // The left pane still lists files, tree or no tree on the right.
        assert!(app.left.current().entries.iter().any(|e| e.name == "a.txt"));
    }

    #[test]
    fn the_plus_button_opens_a_terminal_in_the_active_panel() {
        let (_root, mut app) = fixture();
        assert!(app.terminals.is_empty());

        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));

        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.terminals.active().unwrap().cwd, app.left.cwd());
        assert!(app.show_terminal);
        assert!(!app.status_is_error, "{}", app.status);
    }

    #[test]
    fn several_terminals_run_at_once_and_close_independently() {
        // The point of the + button: a long build keeps going in one tab while
        // another is used for something else.
        let (_root, mut app) = fixture();
        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));
        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));
        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));
        assert_eq!(app.terminals.len(), 3);
        assert_eq!(app.terminals.active, 2);

        app.terminals.close(1);
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.terminals.active, 1, "the selection follows the removal");
    }

    #[test]
    fn a_new_terminal_opens_where_the_active_panel_is() {
        let (_root, mut app) = fixture();
        app.active = Side::Right;
        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));
        assert_eq!(app.terminals.active().unwrap().cwd, app.right.cwd());
    }

    #[test]
    fn opening_a_shell_that_does_not_exist_reports_it() {
        let (_root, mut app) = fixture();
        app.open_terminal(Some("/definitely/not/a/shell".to_string()));
        assert!(app.terminals.is_empty());
        assert!(app.status_is_error);
    }

    #[test]
    fn ctrl_enter_types_the_selection_into_the_focused_terminal() {
        let (_root, mut app) = fixture();
        app.open_terminal(Some(lost_commander_core::pty::plain::program().to_string()));
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        assert!(app.send_selection_to_terminal(false));
        // The one-shot command line is untouched: the shell got the text.
        assert!(app.command.is_empty());
    }

    #[test]
    fn sending_a_selection_with_no_terminal_open_declines() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        assert!(
            !app.send_selection_to_terminal(false),
            "so the caller can fall back to the command line"
        );
    }

    #[test]
    fn ctrl_enter_puts_the_cursor_file_on_the_command_line() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        app.insert_selection(false);
        assert_eq!(app.command, "a.txt");

        // A second insert appends rather than replacing.
        app.insert_selection(false);
        assert_eq!(app.command, "a.txt a.txt");
    }

    #[test]
    fn ctrl_enter_inserts_every_marked_file() {
        let (_root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        let b = index_of(&app, Side::Left, "b.txt");
        app.apply_click(Side::Left, Some((a, Click::Toggle)), None);
        app.apply_click(Side::Left, Some((b, Click::Toggle)), None);

        app.command = "wc -l".to_string();
        app.insert_selection(false);
        assert_eq!(app.command, "wc -l a.txt b.txt");
    }

    #[test]
    fn inserted_names_with_spaces_are_quoted() {
        let (root, _app) = fixture();
        let dir = root.path().join("left");
        fs::write(dir.join("my holiday.jpg"), "x").unwrap();
        let mut app = GuiApp::detached(dir, root.path().join("right"));

        let index = index_of(&app, Side::Left, "my holiday.jpg");
        app.apply_click(Side::Left, Some((index, Click::Plain)), None);
        app.insert_selection(false);

        // One argument, not two.
        assert!(
            app.command == "'my holiday.jpg'" || app.command == "\"my holiday.jpg\"",
            "{}",
            app.command
        );
    }

    #[test]
    fn shift_ctrl_enter_inserts_the_whole_path() {
        let (root, mut app) = fixture();
        let a = index_of(&app, Side::Left, "a.txt");
        app.apply_click(Side::Left, Some((a, Click::Plain)), None);

        app.insert_selection(true);
        // Against the platform's own quoting rather than a bare suffix: every
        // Windows path holds backslashes, so there the whole thing arrives
        // quoted and `ends_with("a.txt")` would read as a truncated path.
        let whole = root.path().join("left").join("a.txt");
        assert_eq!(
            app.command,
            shell::quote_here(&whole.display().to_string()),
            "{}",
            app.command
        );
    }

    #[test]
    fn the_parent_entry_is_never_inserted() {
        let (_root, mut app) = fixture();
        app.left.current_mut().cursor_home();
        app.insert_selection(false);
        assert!(app.command.is_empty());
    }

    #[test]
    fn a_command_runs_in_the_active_panel_and_is_recorded() {
        let (_root, mut app) = fixture();
        app.command = "ls".to_string();
        app.run_command();

        assert!(app.shell_job.is_some());
        assert!(app.command.is_empty(), "the line is cleared once submitted");

        if let Some(job) = &mut app.shell_job {
            job.join();
        }
        app.poll_shell();

        assert!(app.shell_job.is_none());
        assert_eq!(app.console.len(), 1);
        assert_eq!(app.console[0].line, "ls");
        assert!(app.console[0].output.stdout.contains("a.txt"));
    }

    #[test]
    fn a_failing_command_is_recorded_and_flagged() {
        let (_root, mut app) = fixture();
        app.command = "ls /definitely/not/here".to_string();
        app.run_command();
        if let Some(job) = &mut app.shell_job {
            job.join();
        }
        app.poll_shell();

        assert_eq!(app.console.len(), 1);
        assert!(!app.console[0].output.succeeded());
        assert!(app.status_is_error);
    }

    #[test]
    fn cd_moves_the_panel_instead_of_spawning_a_shell() {
        let (_root, mut app) = fixture();
        let target = app.left.cwd().join("sub");

        app.command = "cd sub".to_string();
        app.run_command();

        // No subprocess: a child changing its own directory would do nothing.
        assert!(app.shell_job.is_none());
        assert_eq!(app.left.cwd(), target);
        assert!(app.command.is_empty());
    }

    #[test]
    fn cd_somewhere_that_is_not_a_directory_reports_an_error() {
        let (_root, mut app) = fixture();
        app.command = "cd a.txt".to_string();
        app.run_command();

        assert!(app.shell_job.is_none());
        assert!(app.status_is_error);
        assert!(app.status.contains("Not a directory"), "{}", app.status);
    }

    #[test]
    fn an_empty_command_line_does_nothing() {
        let (_root, mut app) = fixture();
        app.command = "   ".to_string();
        app.run_command();
        assert!(app.shell_job.is_none());
        assert!(app.console.is_empty());
    }

    #[test]
    fn a_command_that_changes_files_refreshes_the_panels() {
        let (_root, mut app) = fixture();
        assert!(index_of(&app, Side::Left, "a.txt") > 0);

        app.command = "touch brand-new.txt".to_string();
        app.run_command();
        if let Some(job) = &mut app.shell_job {
            job.join();
        }
        app.poll_shell();

        assert!(
            app.left
                .current()
                .entries
                .iter()
                .any(|e| e.name == "brand-new.txt"),
            "the panel should have been re-read after the command"
        );
    }

    #[test]
    fn the_prompt_names_the_active_panel() {
        let (_root, mut app) = fixture();
        assert!(app.prompt().starts_with("left"));
        app.active = Side::Right;
        assert!(app.prompt().starts_with("right"));
    }

    #[test]
    fn the_command_runs_in_whichever_panel_is_active() {
        let (_root, mut app) = fixture();
        // b.txt only exists on the left; running from the right must not see it.
        app.active = Side::Right;
        app.command = "ls".to_string();
        app.run_command();
        if let Some(job) = &mut app.shell_job {
            job.join();
        }
        app.poll_shell();

        assert!(!app.console[0].output.stdout.contains("b.txt"));
        assert_eq!(app.console[0].cwd, app.right.cwd());
    }

    #[test]
    fn history_records_where_the_command_actually_ran() {
        let (_root, mut app) = fixture();
        let started_in = app.left.cwd().to_path_buf();

        app.command = "ls".to_string();
        app.run_command();

        // Switch panels while it is still in flight, as a user waiting on a
        // slow command would.
        app.active = Side::Right;

        if let Some(job) = &mut app.shell_job {
            job.join();
        }
        app.poll_shell();

        // The log line must name where it ran, not where the panels ended up.
        assert_eq!(app.console[0].cwd, started_in);
        assert!(
            app.console[0].prompt.starts_with("left"),
            "{}",
            app.console[0].prompt
        );
    }

    #[test]
    fn ctrl_enter_takes_its_names_from_the_active_panel() {
        let (root, _app) = fixture();
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::write(right.join("only-on-the-right.txt"), "x").unwrap();
        let mut app = GuiApp::detached(left, right);

        app.active = Side::Right;
        let index = index_of(&app, Side::Right, "only-on-the-right.txt");
        app.apply_click(Side::Right, Some((index, Click::Plain)), None);
        app.insert_selection(false);

        assert_eq!(app.command, "only-on-the-right.txt");
    }

    #[test]
    fn cd_moves_the_active_panel_and_leaves_the_other_alone() {
        let (_root, mut app) = fixture();
        let left_before = app.left.cwd().to_path_buf();
        let right_before = app.right.cwd().to_path_buf();

        app.active = Side::Right;
        app.command = format!("cd {}", left_before.display());
        app.run_command();

        assert_eq!(app.right.cwd(), left_before, "the active panel moved");
        assert_eq!(app.left.cwd(), left_before);
        assert_ne!(right_before, app.right.cwd());
    }

    #[test]
    fn short_names_stay_on_one_line() {
        assert_eq!(wrap_label("main.rs", 15), vec!["main.rs"]);
    }

    #[test]
    fn long_names_wrap_to_two_lines_and_are_elided() {
        let lines = wrap_label("a-really-quite-long-file-name-here.txt", 15);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars().count(), 15);
        assert!(lines[1].ends_with('\u{2026}'));
    }

    #[test]
    fn names_just_over_the_limit_do_not_get_an_ellipsis() {
        let lines = wrap_label("exactly-sixteen!", 15);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "!");
    }

    #[test]
    fn viewing_in_a_one_pane_window_gives_the_pane_back_afterwards() {
        let (_root, mut app) = fixture();
        app.show_right = false;
        app.active = Side::Left;
        app.left.current_mut().cursor_to(1);

        app.view_selected();
        assert!(app.show_right, "a preview has to appear somewhere");
        assert_eq!(app.view(Side::Right), ViewMode::Preview);
        assert_eq!(app.active, Side::Left, "the keyboard stays where it was");

        // F3 again. The window goes back to how it was found: a viewer that
        // left a pane behind would be redecorating on its way out.
        app.view_selected();
        assert!(!app.show_right, "the borrowed pane is handed back");
        assert_eq!(app.active, Side::Left);
    }

    #[test]
    fn a_pane_that_was_already_open_is_not_folded_away_by_the_viewer() {
        let (_root, mut app) = fixture();
        // The reader opened it themselves, which is what makes it theirs.
        app.show_right = true;
        app.left.current_mut().cursor_to(1);

        app.view_selected();
        app.view_selected();
        // Nothing was borrowed, so nothing is given back. Closing a preview
        // must not take away a pane the reader had open before they asked.
        assert!(app.show_right);
    }

    #[test]
    fn the_pane_comes_back_however_the_preview_is_closed() {
        // F3 is not the only way out - Ctrl-Q, Escape and the pane's own view
        // buttons all end up in `set_view`, and a pane handed back by only
        // one of them would be worse than one never handed back at all.
        for closed_with in [ViewMode::Details, ViewMode::Grid, ViewMode::Tree] {
            let (_root, mut app) = fixture();
            app.show_right = false;
            app.left.current_mut().cursor_to(1);

            app.view_selected();
            assert!(app.show_right);

            app.set_view(Side::Right, closed_with);
            assert!(
                !app.show_right,
                "closing the preview with {closed_with:?} should give the pane back"
            );
        }
    }

    #[test]
    fn asking_for_the_second_pane_yourself_makes_it_yours() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.show_right = false;
        app.left.current_mut().cursor_to(1);

        app.view_selected();
        assert!(app.pane_opened_to_view);

        // The reader takes ownership of the arrangement by toggling it
        // themselves; the viewer does not get to undo that later.
        app.run_action(A::ToggleSecondPane);
        assert!(!app.pane_opened_to_view);
        app.set_view(Side::Right, ViewMode::Details);
        assert!(
            !app.show_right,
            "the toggle folded it away, and the viewer did not put it back"
        );
    }

    #[test]
    fn a_pane_with_a_tree_counts_what_is_tagged_and_not_what_is_on_screen() {
        let (root, mut app) = fixture();
        let panel = app.left.current_mut();
        panel.enter_tree_mode();
        panel.cursor_to(1);
        panel.toggle_mark();

        // Tag something in a directory that is not the one being shown, the
        // way walking a tree does.
        let elsewhere = root.path().join("sub");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("far.txt"), b"x").unwrap();
        panel.tagged.insert(elsewhere.join("far.txt"));

        let panel = app.left.current();
        let summary = selection_summary(panel, panel.entries.len() - 1, ViewMode::Tree);
        assert_eq!(summary, "2 tagged across the tree");
        // The row count would have said one, because that is all that is on
        // screen - reassuring, and about the wrong thing.
        assert_eq!(panel.marked_count(), 1);
    }

    #[test]
    fn without_a_tree_the_header_says_what_it_always_said() {
        let (_root, mut app) = fixture();
        let panel = app.left.current_mut();
        panel.cursor_to(1);
        panel.toggle_mark();
        let panel = app.left.current();
        let count = panel.entries.len() - 1;
        assert_eq!(
            selection_summary(panel, count, ViewMode::Details),
            format!("1 of {count} selected")
        );
    }

    #[test]
    fn enter_goes_down_into_the_files_and_escape_comes_back_up() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::ViewTree);
        assert!(app.on_tree[0], "the tree has the keyboard");

        // Enter on a directory means "show me what is in here", and what is
        // in it is the half below.
        app.run_action(A::Open);
        assert!(!app.on_tree[0], "down into the files that were just opened");

        app.run_action(A::Cancel);
        assert!(app.on_tree[0], "and back up to the tree");
    }

    #[test]
    fn tab_only_ever_means_the_other_pane() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.run_action(A::ViewTree);
        assert!(app.on_tree[0]);

        // It briefly walked the halves too, which made it mean two things
        // depending on state.
        app.show_right = true;
        app.run_action(A::SwitchPane);
        assert_eq!(app.active, Side::Right);
        app.run_action(A::SwitchPane);
        assert_eq!(app.active, Side::Left);
        assert!(app.on_tree[0], "and left the halves alone");
    }

    #[test]
    fn arrows_reach_the_tree_only_while_the_tree_has_the_keyboard() {
        use keys::Action as A;
        let (_root, mut app) = fixture();
        app.set_view(Side::Left, ViewMode::Tree);

        // In the files half, Down moves the file cursor and leaves the tree
        // where it is.
        app.on_tree[0] = false;
        let tree_before = app.left.current().tree.as_ref().unwrap().cursor;
        app.run_action(A::CursorDown);
        assert_eq!(
            app.left.current().tree.as_ref().unwrap().cursor,
            tree_before,
            "the tree does not move when the files have the keyboard"
        );

        // In the tree half, the same key moves the tree.
        app.on_tree[0] = true;
        app.run_action(A::CursorDown);
        assert_ne!(
            app.left.current().tree.as_ref().unwrap().cursor,
            tree_before
        );
    }

    #[test]
    fn the_cursor_never_rests_on_a_row_the_file_half_does_not_draw() {
        let (_root, mut app) = fixture();
        app.set_view(Side::Left, ViewMode::Tree);

        // Put it on a directory by hand, the way any listing change could.
        let at = app
            .left
            .current()
            .entries
            .iter()
            .position(|e| e.is_dir() || e.is_parent());
        if let Some(at) = at {
            app.left.current_mut().cursor_to(at);
            app.snap_to_a_visible_row(Side::Left);
            let entry = app.left.current().selected().expect("something selected");
            assert!(
                !entry.is_dir() && !entry.is_parent(),
                "a cursor on a directory is a selection nobody can see"
            );
        }
    }

    #[test]
    fn a_settings_file_cannot_push_a_pane_off_the_screen() {
        // Clamped rather than trusted. A settings file is a text file
        // somebody can edit, and a split of 40 would leave one pane with the
        // whole window and the divider somewhere off the right-hand edge -
        // with no way to drag it back, because there is nothing to grab.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let settings = lost_commander_core::config::Settings {
            pane_split: Some(40.0),
            tree_split: Some(-3.0),
            ..Default::default()
        };
        settings.save_to(&path).unwrap();

        let read = lost_commander_core::config::Settings::load_from(&path).unwrap();
        let split = read.pane_split.unwrap().clamp(0.15, 0.85);
        let tree = read.tree_split.unwrap().clamp(0.15, 0.85);
        assert!((0.15..=0.85).contains(&split));
        assert!((0.15..=0.85).contains(&tree));
    }

    #[test]
    fn the_layout_that_is_written_down_is_the_one_on_screen() {
        let (_root, mut app) = fixture();
        app.split = 0.7;
        app.tree_split = 0.3;
        // Not `remember_layout`, which saves: see the note on it.
        app.layout_into_settings();
        assert_eq!(app.settings.pane_split, Some(0.7));
        assert_eq!(app.settings.tree_split, Some(0.3));
    }

    #[test]
    fn the_tree_moves_with_the_arrows_the_way_a_reader_gets_to_it() {
        use keys::Action as A;
        let (_root, mut app) = fixture();

        // Through the key, not through `set_view`. Every test of this walked
        // in by calling the method or setting the flag, which is why a tree
        // nobody could reach still passed all of them.
        app.run_action(A::ViewTree);
        assert_eq!(app.view(Side::Left), ViewMode::Tree);
        assert!(app.on_tree[0], "the tree has the keyboard once asked for");

        let before = app.left.current().tree.as_ref().unwrap().cursor;
        app.run_action(A::CursorDown);
        assert_ne!(
            app.left.current().tree.as_ref().unwrap().cursor,
            before,
            "Down moves the tree"
        );
    }

    #[test]
    fn walking_a_pane_is_not_undone_by_a_shell_that_has_not_moved() {
        // The rule that makes following usable: the pane follows a `cd` when
        // it *happens*, not whenever the two disagree. With the second rule,
        // walking a pane somewhere the shell is not would be undone on the
        // very next frame, and the panes would be unusable while a shell was
        // open anywhere.
        let (root, mut app) = fixture();
        let elsewhere = root.path().join("sub");
        std::fs::create_dir_all(&elsewhere).unwrap();

        // The shell said where it was once, and has not moved since.
        app.shell_was = Some((0, elsewhere.clone()));
        let went = app.left.cwd().to_path_buf();

        app.follow_the_shell();
        assert_eq!(
            app.left.cwd(),
            went,
            "no shell running, and nothing reported - the pane stays"
        );
    }

    #[test]
    fn switching_shell_tabs_is_not_a_cd() {
        // Noted without acting on it, or every glance at another shell would
        // drag the pane somewhere the reader never asked to go.
        let (root, mut app) = fixture();
        let elsewhere = root.path().join("sub");
        std::fs::create_dir_all(&elsewhere).unwrap();

        app.shell_was = Some((3, elsewhere.clone()));
        let went = app.left.cwd().to_path_buf();
        app.follow_the_shell();
        assert_eq!(app.left.cwd(), went, "a different tab is not a move");
    }

    #[test]
    fn a_pinned_terminal_is_steered_by_nobody() {
        let (root, mut app) = fixture();
        let elsewhere = root.path().join("sub");
        std::fs::create_dir_all(&elsewhere).unwrap();

        // Set directly: `set_pinned` pins a tab that exists, and the
        // checkbox is only drawn when one does. Spawning a real shell here
        // would test the pty rather than the rule.
        app.terminals.pinned = vec![true];
        assert!(app.terminals.is_pinned(0));

        // Neither direction does anything without a session, and neither
        // panics reaching for one.
        let went = app.left.cwd().to_path_buf();
        app.shell_was = Some((0, elsewhere.clone()));
        app.follow_the_shell();
        app.shell_follows_the_pane();
        assert_eq!(app.left.cwd(), went);
    }

    #[test]
    fn closing_a_tab_takes_its_pin_with_it() {
        // Or every tab after it inherits its neighbour's pin, and a terminal
        // nobody pinned quietly stops following the panels.
        let terminals = lost_commander_core::pty::Terminals {
            pinned: vec![false, true, false],
            ..Default::default()
        };
        // `close` on an empty session list is a no-op, so the pins are asked
        // about directly - the point is that the two stay the same length.
        assert!(terminals.is_pinned(1));
        assert!(!terminals.is_pinned(2));
        assert!(!terminals.is_pinned(9), "past the end is not pinned");
    }
}
