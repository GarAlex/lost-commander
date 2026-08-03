// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The keyboard map: every key the graphical view answers to.
//!
//! Kept as a pure function from key to intent, with no application state in
//! sight, so the whole map can be tested without a window. The dispatch that
//! carries an [`Action`] out lives in [`super::GuiApp::run_action`].
//!
//! The bindings are the Commander ones. F5 copies, F6 moves, F8 deletes, Tab
//! swaps panes, Insert marks, and the grey `+` `-` `*` do patterns and
//! inversion, because that is what forty years of muscle memory expects. The
//! additions - the view switch, the shell panel - take keys that were never
//! spoken for.
//!
//! # How typing and shortcuts stay out of each other's way
//!
//! The original's answer was that they never overlapped in the first place.
//! Everything printable went to the command line at the bottom of the screen,
//! and every panel command was a key that types nothing: a function key, an
//! arrow, Tab, Insert, or something with Ctrl or Alt on it. There was no
//! ambiguity to resolve.
//!
//! The exceptions were `+` `-` `*`, and they worked because on a PC keyboard
//! those exist twice. The panel used the **grey** ones on the numeric keypad,
//! which the hardware reports as different keys from the `+` and `-` you type
//! with. That distinction is not available here - egui folds the numpad into
//! the same `Key` values as the main row - so this uses the rule the original
//! already applied to `Enter`:
//!
//! **A single-character panel command only applies while the command line is
//! empty.** Empty line and `*` inverts the marks; a line with `find ` on it
//! and `*` is a `*` in a shell glob. Same for `+`, `-`, and `Space`, and for
//! `Enter` - which opened the file under the cursor on an empty line and ran
//! the command on a full one, exactly as it does here.

use eframe::egui::{Key, Modifiers};

/// Something the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // ---- moving about
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    Home,
    End,
    Open,
    Parent,
    Root,
    SwitchPane,
    SwapPanes,
    Reload,

    // ---- marking
    Mark,
    MarkAll,
    ClearMarks,
    InvertMarks,
    SelectByPattern,
    DeselectByPattern,
    SelectMenu,

    // ---- the F-keys
    Help,
    Rename,
    View,
    Edit,
    Copy,
    Move,
    MkDir,
    Delete,
    Quit,

    // ---- the graphical view's own
    ViewDetails,
    ViewGrid,
    ViewTree,
    QuickView,
    ToggleHidden,
    ToggleSidebar,
    ToggleSecondPane,
    Bookmark,
    ToggleShellPanel,
    FocusTerminal,
    LeaveTerminal,
    FocusCommandLine,
    Cancel,

    // ---- the command line, without leaving the panes
    /// Hand a Tab to the shell, for its own completion.
    CompleteCommand,
    /// Walk the shell's history.
    HistoryBack,
    HistoryForward,

    /// The colour form.
    Theme,
    /// Choose which application opens the file, rather than the default one.
    OpenWith,
    /// Edit a file you do not own, without running the editor as root.
    EditAsAdmin,
    /// A shell with administrator privileges, where the panel is.
    RootShell,
    /// Delete for good, without going through the trash.
    DeleteForever,
    /// Find files by name, and by what is inside them.
    Find,
    /// See and change what a file is: permissions, ownership, dates.
    Properties,
    /// New names for a whole selection at once.
    MultiRename,

    // ---- tabs
    /// Another tab in this pane, on the directory it is showing.
    NewTab,
    CloseTab,
    /// Keep the tab on show and close the rest.
    CloseOtherTabs,
    NextTab,
    PreviousTab,
    /// Send this tab, whole, to the other pane.
    MoveTabAcross,

    // ---- telling two directories apart
    /// Mark what differs between the two panes, without a dialog.
    CompareFolders,
    /// The recursive one: what differs, and which way it would go.
    Synchronize,
    /// Two files, line by line.
    CompareFiles,
    /// Files under this pane that are the same file twice.
    Duplicates,
    /// The account of what was done to the files.
    Journal,
    /// Hand the file to $EDITOR in a shell tab, rather than editing it here.
    EditExternally,
    /// Open the picture under the cursor for turning, cropping and resizing.
    EditImage,
}

/// What a key press means, or `None` to let it fall through.
///
/// `shift` is deliberately ignored for most keys: a shifted `+` is still `+`
/// on the keyboards where it needs shift at all.
pub fn action_for(key: Key, modifiers: Modifiers) -> Option<Action> {
    let ctrl = modifiers.ctrl || modifiers.command;
    let alt = modifiers.alt;
    let shift = modifiers.shift;

    // The one key that must work while the shell has the keyboard, so there
    // is always a way back out to the panes without reaching for the mouse.
    if key == Key::Escape && shift {
        return Some(Action::LeaveTerminal);
    }

    // Ctrl-Tab walks the tabs, as it does in every program that has them.
    // Before the Shift-Tab arm below, or Ctrl-Shift-Tab would be read as a
    // request to complete a command.
    if ctrl && key == Key::Tab {
        return Some(if shift {
            Action::PreviousTab
        } else {
            Action::NextTab
        });
    }

    // Completion and history, with a modifier, so plain Tab and the plain
    // arrows stay with the panes. This is the split every Commander settled
    // on - mc puts completion on Esc-Tab and history on Alt-P/Alt-N for the
    // same reason: you compose a command *while* browsing, and the workflow
    // that makes the layout worth having is to start typing, walk to a file,
    // and Ctrl-Enter its name into the line. Taking the arrows would cost
    // exactly that.
    if key == Key::Tab && shift {
        return Some(Action::CompleteCommand);
    }
    if ctrl && key == Key::ArrowUp {
        return Some(Action::HistoryBack);
    }
    if ctrl && key == Key::ArrowDown {
        return Some(Action::HistoryForward);
    }

    // Shift-Enter is Enter with a question attached: not "open this" but
    // "open this with what?". Every desktop puts the chooser one modifier
    // away from the default action, and this is the modifier left.
    if shift && key == Key::Enter {
        return Some(Action::OpenWith);
    }
    // F2 renames one file; F2 with a shift behind it renames the selection.
    // The same key as the terminal view, which cannot have Ctrl-M.
    if shift && key == Key::F2 {
        return Some(Action::MultiRename);
    }
    // F3 shows one file; F3 with a shift behind it shows what two of them
    // differ by. The terminal view cannot have this one - the escape sequence
    // for Shift-F3 is `CSI 1;2R`, which is also the cursor-position report,
    // so it never arrives as a key - and has it on Alt-D instead, which works
    // in both.
    if shift && key == Key::F3 {
        return Some(Action::CompareFiles);
    }
    // F6 sends a file to the other pane; F6 with a shift behind it sends the
    // whole tab, which is the same idea one level up.
    if shift && key == Key::F6 {
        return Some(Action::MoveTabAcross);
    }
    // F4 edits; F4 with a shift behind it edits what you do not own.
    if shift && key == Key::F4 {
        return Some(Action::EditAsAdmin);
    }
    // Shift makes a delete permanent, which is the convention everywhere -
    // Explorer has used Shift-Delete for it since the recycle bin existed.
    if shift && matches!(key, Key::F8 | Key::Delete) {
        return Some(Action::DeleteForever);
    }
    // Alt-F7 is the Commander binding for find; Ctrl-F is what anyone who
    // has used anything else since will reach for. Both, since neither costs
    // the other anything.
    if alt && key == Key::F7 {
        return Some(Action::Find);
    }
    // Alt-Enter is Properties everywhere it exists, which is everywhere.
    if alt && key == Key::Enter {
        return Some(Action::Properties);
    }

    if ctrl {
        return Some(match key {
            Key::A => Action::MarkAll,
            Key::R => Action::Reload,
            Key::U => Action::SwapPanes,
            Key::H => Action::ToggleHidden,
            Key::D => Action::Bookmark,
            Key::T => Action::NewTab,
            Key::W => Action::CloseTab,
            Key::Q => Action::QuickView,
            Key::O => Action::ToggleShellPanel,
            Key::Backtick => Action::FocusTerminal,
            Key::Backslash => Action::Root,
            Key::K => Action::Theme,
            Key::E => Action::RootShell,
            Key::F => Action::Find,
            // Ctrl-M is Total Commander's multi-rename tool, and F2 with the
            // selection behind it is what anyone reaching for it expects.
            Key::M => Action::MultiRename,
            // J for journal. The account of what was done.
            Key::J => Action::Journal,
            Key::Num1 => Action::ViewDetails,
            Key::Num2 => Action::ViewGrid,
            Key::Num3 => Action::ViewTree,
            Key::Num4 => Action::QuickView,
            Key::PageUp => Action::PreviousTab,
            Key::PageDown => Action::NextTab,
            _ => return None,
        });
    }

    if alt {
        return match key {
            // Alt-. hides and shows dotfiles, as it does in half the file
            // managers written since.
            Key::Period => Some(Action::ToggleHidden),
            // The tree had Ctrl-T until tabs wanted it, which is the one key
            // every program with tabs uses. Ctrl-3 still opens the tree too.
            Key::T => Some(Action::ViewTree),
            Key::W => Some(Action::CloseOtherTabs),
            // The two halves of the same question, one key apart: C marks
            // what differs here, S opens the recursive one that can act on it.
            Key::C => Some(Action::CompareFolders),
            Key::D => Some(Action::CompareFiles),
            Key::S => Some(Action::Synchronize),
            // U because C, D and S are all spoken for by the three above and
            // "duplicate" has no letter left.
            Key::U => Some(Action::Duplicates),
            // I for image. Ctrl-E would be the obvious one and belongs to the
            // root shell, which is not a binding worth moving for this.
            Key::I => Some(Action::EditImage),
            // F4 has its own editor now; this is the route to your own.
            Key::E => Some(Action::EditExternally),
            _ => None,
        };
    }

    Some(match key {
        Key::Tab => Action::SwitchPane,
        Key::ArrowUp => Action::CursorUp,
        Key::ArrowDown => Action::CursorDown,
        Key::PageUp => Action::PageUp,
        Key::PageDown => Action::PageDown,
        Key::Home => Action::Home,
        Key::End => Action::End,
        Key::Enter => Action::Open,
        Key::ArrowRight => Action::Open,
        Key::Backspace | Key::ArrowLeft => Action::Parent,
        // Insert only. Space is a printable character, so it comes through
        // as text like `*` does - taking it here as well would type it twice.
        Key::Insert => Action::Mark,
        Key::Escape => Action::Cancel,

        Key::F1 => Action::Help,
        Key::F2 => Action::Rename,
        Key::F3 => Action::View,
        Key::F4 => Action::Edit,
        Key::F5 => Action::Copy,
        Key::F6 => Action::Move,
        Key::F7 => Action::MkDir,
        Key::F8 | Key::Delete => Action::Delete,
        Key::F9 => Action::SelectMenu,
        Key::F10 => Action::Quit,
        Key::F11 => Action::ToggleSidebar,
        Key::F12 => Action::ToggleSecondPane,
        _ => return None,
    })
}

/// What a printable character means when the panes have the keyboard.
///
/// Everything goes to the command line, which is what the original did and
/// the reason typing never fought with the panel's own keys. The three grey
/// characters are the exception, and only while the line is empty - see the
/// note at the top of this module.
pub fn action_for_text(text: &str, command_line_empty: bool) -> Option<Action> {
    if !command_line_empty {
        return None;
    }
    match text {
        "*" => Some(Action::InvertMarks),
        "+" => Some(Action::SelectByPattern),
        "-" => Some(Action::DeselectByPattern),
        " " => Some(Action::Mark),
        _ => None,
    }
}

/// Keys that mean one thing to the panes and another to the command line.
///
/// `Enter` runs a command rather than opening a file, `Backspace` rubs out a
/// character rather than going to the parent directory, `Space` types a space
/// rather than marking, and `Escape` clears the line. All of them only do the
/// panel's job while the line is empty - which is the original's rule for
/// `Enter`, applied to the rest for the same reason.
pub fn defers_to_command_line(action: Action) -> bool {
    matches!(action, Action::Open | Action::Parent | Action::Cancel)
}

/// Whether a character egui reports is one somebody meant to type.
///
/// Text events carry no modifiers of their own, so this reads the frame's. A
/// character produced while Alt or Ctrl is held is the by-product of a
/// shortcut, not typing: X11 sends `c` alongside the key event for Alt-C, and
/// without this every Alt binding leaves its letter on the command line behind
/// it.
pub fn is_typed_text(modifiers: Modifiers) -> bool {
    !(modifiers.alt || modifiers.ctrl || modifiers.command)
}

/// Whether egui will have walked keyboard focus for the key that meant this.
///
/// Every binding on `Tab`, whatever is held down with it: egui traverses on a
/// Tab press regardless of the modifiers, and it does the traversing *after*
/// the panes have had the key. A binding that forgets to say so leaves focus
/// parked on whichever button happened to be first, and from the next frame on
/// every key is read as typing into it - which is the keyboard dead until
/// something is clicked. Ctrl-Tab reintroduced exactly that bug the day it was
/// added, which is why the rule is a function with a test rather than a
/// condition written out at the one place that needs it.
pub fn traverses_focus(action: Action) -> bool {
    matches!(
        action,
        Action::SwitchPane | Action::CompleteCommand | Action::NextTab | Action::PreviousTab
    )
}

/// Whether an action is one the tree view answers itself.
///
/// A tree navigates differently: left and right are collapse and expand
/// rather than parent and open.
/// Whether an action would change a file, rather than look at one.
///
/// Used where a pane is somewhere that cannot be written to. Listed by what
/// they do rather than by key, so a rebinding does not quietly make one of
/// them reachable again.
pub fn changes_files(action: Action) -> bool {
    matches!(
        action,
        Action::Move
            | Action::Delete
            | Action::DeleteForever
            | Action::Rename
            | Action::MultiRename
            | Action::MkDir
            | Action::EditAsAdmin
            | Action::EditImage
            | Action::EditExternally
            | Action::Edit
    )
}

pub fn is_navigation(action: Action) -> bool {
    matches!(
        action,
        Action::CursorUp
            | Action::CursorDown
            | Action::PageUp
            | Action::PageDown
            | Action::Home
            | Action::End
            | Action::Open
            | Action::Parent
    )
}

/// The list shown by F1, in the order it reads best.
pub const HELP: &[(&str, &str)] = &[
    ("F1", "this help"),
    ("F2", "rename"),
    ("Shift-F2, Ctrl-M", "rename the whole selection at once"),
    ("Ctrl-T", "another tab, here"),
    ("Ctrl-W", "close this tab"),
    ("Alt-W", "close the other tabs"),
    ("Ctrl-Tab, Ctrl-PgUp/PgDn", "walk the tabs"),
    ("Shift-F6", "send this tab to the other pane"),
    ("Shift-F3, Alt-D", "compare two files, line by line"),
    ("Alt-C", "mark what differs between the panes"),
    ("Alt-U", "find files that are the same file twice"),
    ("Alt-S", "synchronize the two directories"),
    ("Alt-I", "turn, crop or resize the picture"),
    ("Ctrl-J", "what was done - the account"),
    ("Alt-E", "edit with $EDITOR in a shell tab"),
    ("Alt-T", "directory tree"),
    ("F3", "view - show it in the other pane; F3 again stops"),
    ("F4 on a binary", "edit its bytes"),
    ("F4", "edit the text here, with its encoding"),
    ("F5", "copy to the other pane"),
    ("F6", "move to the other pane"),
    ("F7", "make directory"),
    ("F8, Del", "move to the trash"),
    ("Shift-F8, Shift-Del", "delete for good"),
    ("Alt-F7, Ctrl-F", "find files"),
    ("Alt-Enter", "properties and permissions"),
    ("F9", "select menu"),
    ("F10", "quit"),
    ("", ""),
    ("Tab", "other pane"),
    ("Enter, Right", "open"),
    ("Backspace, Left", "parent directory"),
    ("Ctrl-\\", "filesystem root"),
    ("Ctrl-PageUp", "parent directory"),
    ("Ctrl-U", "swap the panes"),
    ("Ctrl-R", "reload both"),
    ("", ""),
    ("Insert, Space", "mark, and step down"),
    ("(anything else)", "goes to the command line"),
    ("*", "invert the marks"),
    ("+ / -", "select / deselect by pattern"),
    ("Ctrl-A", "mark everything"),
    ("", ""),
    ("Ctrl-1..4", "list, grid, tree, quick view"),
    ("Ctrl-T", "tree"),
    ("Ctrl-Q", "quick view"),
    ("Ctrl-H, Alt-.", "show hidden files"),
    ("Ctrl-D", "bookmark this directory"),
    ("F11 / F12", "sidebar / second pane"),
    ("Shift-Enter", "open with a chosen application"),
    ("Shift-F4", "edit a file you do not own"),
    ("Ctrl-E", "a shell as administrator, here"),
    ("Ctrl-K", "colours"),
    ("", ""),
    ("Ctrl-O", "show or hide the shell panel"),
    ("Ctrl-`", "type in the shell"),
    ("Shift-Esc", "leave the shell, back to the panes"),
    ("Ctrl-Enter", "put the selected names on the prompt"),
    ("Shift-Tab", "let the shell complete what you have typed"),
    ("Ctrl-Up / Ctrl-Down", "the shell's history"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(key: Key) -> Option<Action> {
        action_for(key, Modifiers::NONE)
    }
    fn with_ctrl(key: Key) -> Option<Action> {
        action_for(key, Modifiers::CTRL)
    }

    #[test]
    fn the_function_keys_are_the_ones_they_have_always_been() {
        assert_eq!(plain(Key::F1), Some(Action::Help));
        assert_eq!(plain(Key::F2), Some(Action::Rename));
        assert_eq!(plain(Key::F3), Some(Action::View));
        assert_eq!(plain(Key::F4), Some(Action::Edit));
        assert_eq!(plain(Key::F5), Some(Action::Copy));
        assert_eq!(plain(Key::F6), Some(Action::Move));
        assert_eq!(plain(Key::F7), Some(Action::MkDir));
        assert_eq!(plain(Key::F8), Some(Action::Delete));
        assert_eq!(plain(Key::F10), Some(Action::Quit));
        // Delete does what F8 does, for anyone who never learned the F-keys.
        assert_eq!(plain(Key::Delete), Some(Action::Delete));
    }

    #[test]
    fn the_grey_keys_mark_the_way_they_used_to() {
        assert_eq!(plain(Key::Insert), Some(Action::Mark));
        // Space marks through the text path, not as a key: taking it in both
        // places typed every space twice, which is what it did at first.
        assert_eq!(plain(Key::Space), None);
        assert_eq!(action_for_text(" ", true), Some(Action::Mark));
        assert_eq!(action_for_text(" ", false), None, "a space in a command");
        assert_eq!(action_for_text("*", true), Some(Action::InvertMarks));
        assert_eq!(action_for_text("+", true), Some(Action::SelectByPattern));
        assert_eq!(action_for_text("-", true), Some(Action::DeselectByPattern));
        // A letter is never a panel command: it is the start of a command.
        assert_eq!(action_for_text("a", true), None);
        // `+` and `-` arrive as characters, not as named keys: matching the
        // key as well would fire twice, and would only work on the layout
        // where they sit where egui expects.
        assert_eq!(plain(Key::Plus), None);
        assert_eq!(plain(Key::Minus), None);
        assert_eq!(action_for_text("+", true), Some(Action::SelectByPattern));
        assert_eq!(action_for_text("-", true), Some(Action::DeselectByPattern));
        assert_eq!(with_ctrl(Key::A), Some(Action::MarkAll));
    }

    #[test]
    fn moving_about_needs_no_pointer() {
        assert_eq!(plain(Key::Tab), Some(Action::SwitchPane));
        assert_eq!(plain(Key::ArrowUp), Some(Action::CursorUp));
        assert_eq!(plain(Key::ArrowDown), Some(Action::CursorDown));
        assert_eq!(plain(Key::PageUp), Some(Action::PageUp));
        assert_eq!(plain(Key::Home), Some(Action::Home));
        assert_eq!(plain(Key::End), Some(Action::End));
        assert_eq!(plain(Key::Enter), Some(Action::Open));
        assert_eq!(plain(Key::ArrowRight), Some(Action::Open));
        assert_eq!(plain(Key::Backspace), Some(Action::Parent));
        assert_eq!(plain(Key::ArrowLeft), Some(Action::Parent));
        // Ctrl-PageUp went to the tabs, which is what it does in every
        // program that has them. Backspace and Left are still the way up,
        // and so is the breadcrumb trail.
        assert_eq!(with_ctrl(Key::PageUp), Some(Action::PreviousTab));
        assert_eq!(with_ctrl(Key::PageDown), Some(Action::NextTab));
        assert_eq!(with_ctrl(Key::Backslash), Some(Action::Root));
        assert_eq!(with_ctrl(Key::U), Some(Action::SwapPanes));
    }

    #[test]
    fn every_binding_on_tab_says_that_focus_moved() {
        // Whatever egui traverses on has to be undone, so the set of actions
        // that admit it must be exactly the set that Tab can produce.
        let mut from_tab = Vec::new();
        for modifiers in [
            Modifiers::NONE,
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::CTRL | Modifiers::SHIFT,
            Modifiers::COMMAND,
        ] {
            if let Some(action) = action_for(Key::Tab, modifiers) {
                from_tab.push(action);
            }
        }
        assert!(!from_tab.is_empty());
        for action in &from_tab {
            assert!(
                traverses_focus(*action),
                "{action:?} comes from a Tab press and would leave focus behind"
            );
        }
    }

    #[test]
    fn a_letter_produced_by_a_shortcut_is_not_typing() {
        assert!(is_typed_text(Modifiers::NONE));
        assert!(is_typed_text(Modifiers::SHIFT), "a capital is still typing");
        assert!(!is_typed_text(Modifiers::ALT));
        assert!(!is_typed_text(Modifiers::CTRL));
        assert!(!is_typed_text(Modifiers::COMMAND));

        // Every Alt-letter binding depends on this, so it is worth saying
        // that they are all bindings.
        for key in [Key::C, Key::S, Key::T, Key::W] {
            assert!(
                action_for(key, Modifiers::ALT).is_some(),
                "{key:?} with Alt is a binding, and its letter must not be typed"
            );
        }
    }

    #[test]
    fn a_command_being_typed_takes_back_the_keys_it_needs() {
        // This is how the original kept the two apart, and the rule it used
        // for Enter all along: with something on the command line, the keys
        // that are also characters belong to the line.
        assert_eq!(action_for_text("*", false), None);
        assert_eq!(action_for_text("+", false), None);
        assert_eq!(action_for_text("-", false), None);
        // `find . -name *.rs` has to be typeable.
        assert_eq!(action_for_text("*", true), Some(Action::InvertMarks));

        // Enter, Backspace and Escape change meaning; Space is a character
        // and goes through the text path instead.
        assert!(defers_to_command_line(Action::Open));
        assert!(defers_to_command_line(Action::Parent));
        assert!(defers_to_command_line(Action::Cancel));
        assert_eq!(action_for_text(" ", true), Some(Action::Mark));
        assert_eq!(action_for_text(" ", false), None);
        // The rest never do: F5 copies whether or not you were mid-command.
        assert!(!defers_to_command_line(Action::Copy));
        assert!(!defers_to_command_line(Action::CursorDown));
        assert!(!defers_to_command_line(Action::SwitchPane));
        assert!(!defers_to_command_line(Action::Delete));
    }

    #[test]
    fn completion_and_history_take_a_modifier_so_the_panes_keep_the_plain_keys() {
        // Plain Tab and the plain arrows must stay with the panes: the
        // workflow the layout exists for is to start typing, walk to a file,
        // and put its name on the line.
        assert_eq!(plain(Key::Tab), Some(Action::SwitchPane));
        assert_eq!(plain(Key::ArrowUp), Some(Action::CursorUp));
        assert_eq!(plain(Key::ArrowDown), Some(Action::CursorDown));

        assert_eq!(
            action_for(Key::Tab, Modifiers::SHIFT),
            Some(Action::CompleteCommand)
        );
        assert_eq!(with_ctrl(Key::ArrowUp), Some(Action::HistoryBack));
        assert_eq!(with_ctrl(Key::ArrowDown), Some(Action::HistoryForward));
    }

    #[test]
    fn shift_escape_is_the_way_out_of_the_shell() {
        // It has to be a key no shell program wants, or leaving the terminal
        // would fight with whatever is running in it.
        assert_eq!(
            action_for(Key::Escape, Modifiers::SHIFT),
            Some(Action::LeaveTerminal)
        );
        // Plain Escape stays available to cancel a dialog.
        assert_eq!(plain(Key::Escape), Some(Action::Cancel));
    }

    #[test]
    fn ctrl_takes_precedence_and_unknown_keys_fall_through() {
        // Ctrl-D bookmarks rather than typing a `d` into the pane.
        assert_eq!(with_ctrl(Key::D), Some(Action::Bookmark));
        // A letter on its own is not ours: it belongs to whatever type-ahead
        // or text field wants it.
        assert_eq!(plain(Key::D), None);
        assert_eq!(plain(Key::Z), None);
        assert_eq!(with_ctrl(Key::Z), None);
    }

    #[test]
    fn every_action_is_reachable_from_the_keyboard() {
        // The point of the exercise: nothing may need a mouse. Walk every
        // key/modifier pair and check the map covers the whole enum.
        use Action::*;
        let every = [
            CursorUp,
            CursorDown,
            PageUp,
            PageDown,
            Home,
            End,
            Open,
            Parent,
            Root,
            SwitchPane,
            SwapPanes,
            Reload,
            Mark,
            MarkAll,
            ClearMarks,
            InvertMarks,
            SelectByPattern,
            DeselectByPattern,
            SelectMenu,
            Help,
            Rename,
            View,
            Edit,
            Copy,
            Move,
            MkDir,
            Delete,
            Quit,
            ViewDetails,
            ViewGrid,
            ViewTree,
            QuickView,
            ToggleHidden,
            ToggleSidebar,
            ToggleSecondPane,
            Bookmark,
            ToggleShellPanel,
            FocusTerminal,
            LeaveTerminal,
            FocusCommandLine,
            Cancel,
            CompleteCommand,
            HistoryBack,
            HistoryForward,
            Theme,
            OpenWith,
            EditAsAdmin,
            RootShell,
            DeleteForever,
            Find,
            Properties,
        ];

        let modifiers = [
            Modifiers::NONE,
            Modifiers::CTRL,
            Modifiers::SHIFT,
            Modifiers::ALT,
        ];
        let mut reachable = Vec::new();
        reachable.extend(
            ["*", "+", "-"]
                .iter()
                .filter_map(|t| action_for_text(t, true)),
        );
        for key in Key::ALL {
            for m in modifiers {
                if let Some(action) = action_for(*key, m) {
                    reachable.push(action);
                }
            }
        }

        // ClearMarks and FocusCommandLine are reached through the select menu
        // and the prompt itself rather than by a key of their own.
        let expected_gaps = [ClearMarks, FocusCommandLine];
        let _ = (CompleteCommand, HistoryBack, HistoryForward);
        for action in every {
            if expected_gaps.contains(&action) {
                continue;
            }
            assert!(
                reachable.contains(&action),
                "{action:?} cannot be reached from the keyboard"
            );
        }
    }
}
