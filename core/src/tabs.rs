// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Several directories open in one pane at a time.
//!
//! A pane is no longer one directory but a row of them, of which one is on
//! show. Each keeps its own cursor, marks, sort order and hidden-file setting,
//! so walking six levels down to check something and coming back finds the tab
//! you were working in exactly as you left it.
//!
//! The rule that shapes the whole type is that a pane always shows something:
//! there is no such thing as an empty [`Tabs`], which is why [`Tabs::close`]
//! and [`Tabs::take`] can refuse. Everything else is a list and an index, and
//! all of it is testable without a screen.

use std::path::{Path, PathBuf};

use crate::panel::Panel;

/// The tabs of one pane, and which of them is on show.
pub struct Tabs {
    panels: Vec<Panel>,
    active: usize,
}

impl Tabs {
    /// A pane showing one directory, which is where every pane starts.
    pub fn new(panel: Panel) -> Tabs {
        Tabs {
            panels: vec![panel],
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.panels.len()
    }

    /// Never true - see the note at the top of this module.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// The panel on show.
    pub fn current(&self) -> &Panel {
        &self.panels[self.active]
    }

    pub fn current_mut(&mut self) -> &mut Panel {
        &mut self.panels[self.active]
    }

    /// Every tab, in the order they are drawn.
    pub fn all(&self) -> &[Panel] {
        &self.panels
    }

    pub fn get(&self, index: usize) -> Option<&Panel> {
        self.panels.get(index)
    }

    /// Where this pane is: the directory the tab on show is looking at.
    pub fn cwd(&self) -> &Path {
        &self.current().cwd
    }

    /// Re-read what is on show.
    ///
    /// Only the visible tab: the others are re-read when they are switched to,
    /// which costs nothing until then and cannot be stale by the time it is
    /// seen. See [`Tabs::activate`].
    pub fn reload(&mut self) {
        self.current_mut().reload();
    }

    /// Open another tab on the same directory, and go to it.
    ///
    /// Beside the current one rather than at the end, so a tab opened to
    /// follow a thought sits next to the thought.
    pub fn open(&mut self, panel: Panel) {
        let at = self.active + 1;
        self.panels.insert(at, panel);
        self.active = at;
    }

    /// A copy of what is on show, which is what a new tab starts as.
    pub fn duplicate(&self) -> Panel {
        let mut panel = Panel::new(self.current().cwd.clone());
        panel.sort_by = self.current().sort_by;
        panel.show_hidden = self.current().show_hidden;
        panel.reload();
        panel
    }

    /// Close the tab on show. False when it is the only one there is.
    ///
    /// What comes forward is the tab to the right, or the one to the left when
    /// the last tab was closed - which is where every browser leaves you.
    pub fn close(&mut self) -> bool {
        self.close_at(self.active)
    }

    /// Close the nth tab, which need not be the one on show.
    ///
    /// Closing one *behind* the current tab must not change what is on show,
    /// which is why the index is adjusted rather than clamped: the front-end
    /// closes background workspaces from a list, and a close that silently
    /// switched windows would be worse than the leak it was fixing.
    pub fn close_at(&mut self, index: usize) -> bool {
        if self.panels.len() < 2 || index >= self.panels.len() {
            return false;
        }
        self.panels.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        self.active = self.active.min(self.panels.len() - 1);
        true
    }

    /// Close every other tab, and say how many went.
    pub fn close_others(&mut self) -> usize {
        let closing = self.panels.len() - 1;
        if closing > 0 {
            self.panels.swap(0, self.active);
            self.panels.truncate(1);
            self.active = 0;
        }
        closing
    }

    /// Show the nth tab, re-reading it on the way in.
    ///
    /// Out of range does nothing: an index comes from a click or a saved
    /// position, and neither is worth a panic.
    pub fn activate(&mut self, index: usize) {
        if index >= self.panels.len() {
            return;
        }
        self.active = index;
        // A tab that was hidden while something else changed the disk would
        // otherwise come back showing what was there when it was last looked
        // at. Cheaper than watching every tab, and just as correct, because
        // nobody can see a tab that is not on show.
        self.panels[index].reload();
    }

    /// The next tab along, coming back round to the first.
    pub fn next(&mut self) {
        let at = (self.active + 1) % self.panels.len();
        self.activate(at);
    }

    pub fn prev(&mut self) {
        let at = (self.active + self.panels.len() - 1) % self.panels.len();
        self.activate(at);
    }

    /// Move the tab at `from` so it sits at `to`, keeping the same tab on
    /// show.
    ///
    /// Reordering is about where a tab *sits*, never about which one is
    /// showing - dragging the third workspace to the front should not
    /// switch windows on the way.
    pub fn shift(&mut self, from: usize, to: usize) -> bool {
        if from >= self.panels.len() || to >= self.panels.len() || from == to {
            return false;
        }
        let showing = self.panels[self.active].id;
        let panel = self.panels.remove(from);
        self.panels.insert(to, panel);
        self.active = self
            .panels
            .iter()
            .position(|panel| panel.id == showing)
            .unwrap_or(0);
        true
    }

    /// Lift the tab on show out of this pane, to be handed to the other one.
    ///
    /// `None` when it is the only tab, because a pane always shows something.
    pub fn take(&mut self) -> Option<Panel> {
        if self.panels.len() < 2 {
            return None;
        }
        let panel = self.panels.remove(self.active);
        self.active = self.active.min(self.panels.len() - 1);
        Some(panel)
    }

    /// Receive a tab from the other pane, and show it.
    pub fn accept(&mut self, panel: Panel) {
        self.open(panel);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn shifting_a_tab_moves_where_it_sits_and_never_what_is_showing() {
        let mut tabs = Tabs::new(Panel::new(PathBuf::from("/a")));
        tabs.open(Panel::new(PathBuf::from("/b")));
        tabs.open(Panel::new(PathBuf::from("/c")));
        let showing = tabs.current().id;

        // Drag the first to the end: the shown tab keeps showing, from its
        // new index.
        assert!(tabs.shift(0, 2));
        assert_eq!(tabs.current().id, showing);
        assert_eq!(tabs.all()[2].cwd, PathBuf::from("/a"));

        // Out of range or nowhere to go refuses rather than panics.
        assert!(!tabs.shift(0, 9));
        assert!(!tabs.shift(1, 1));
    }

    #[test]
    fn closing_a_tab_behind_the_current_one_does_not_change_what_is_on_show() {
        let mut tabs = Tabs::new(Panel::new(PathBuf::from("/a")));
        tabs.open(Panel::new(PathBuf::from("/b")));
        tabs.open(Panel::new(PathBuf::from("/c")));
        let showing = tabs.current().id;
        assert_eq!(tabs.active(), 2);

        // Close the first, which sits behind the current one.
        assert!(tabs.close_at(0));
        assert_eq!(tabs.current().id, showing, "still the same tab on show");
        assert_eq!(tabs.active(), 1, "at its new index");

        // Out of range refuses rather than panics, and the last tab refuses
        // because a pane always shows something.
        assert!(!tabs.close_at(9));
        assert!(tabs.close_at(1));
        assert!(!tabs.close_at(0));
    }

    #[test]
    fn a_tab_keeps_its_identity_through_everything_that_moves_it() {
        // The pairing between a directory and its shell hangs off this. An
        // index would do for as long as nobody opened, closed or moved a tab,
        // which is to say not at all.
        let mut left = Tabs::new(Panel::new(PathBuf::from(".")));
        let first = left.current().id;
        left.open(Panel::new(PathBuf::from(".")));
        let second = left.current().id;
        assert_ne!(first, second, "two tabs, two identities");

        // Opened beside it, so the first is now the tab *before* this one -
        // a different index, the same tab.
        left.prev();
        assert_eq!(left.current().id, first);

        // Handed to the other pane, where it is a different index again in a
        // different list. This is the case the pairing most needs to survive:
        // sending a tab across should not leave its shell behind.
        left.next();
        let moved = left.take().expect("two tabs, so one can go");
        assert_eq!(moved.id, second);
        let mut right = Tabs::new(Panel::new(PathBuf::from(".")));
        right.accept(moved);
        assert_eq!(right.current().id, second);

        // And closing one does not hand its identity to its neighbour.
        right.close();
        assert_ne!(right.current().id, second);
    }
}

/// What a tab is labelled: the name of the directory it is showing.
///
/// The whole path would not fit and would be mostly prefix anyway; the last
/// component is what tells two tabs apart. A filesystem root has no last
/// component, so it keeps the path it has.
pub fn title(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => path.display().to_string(),
    }
}

/// The titles of a pane's tabs, made distinguishable where they collide.
///
/// Two tabs called `src` say nothing about which is which, so when a title is
/// not unique it takes its parent with it: `lost-commander/src`. Titles that
/// are already unique are left short - and so are two tabs on the *same*
/// directory, because no amount of path tells those apart and the long form
/// would only be noise.
pub fn titles(paths: &[PathBuf]) -> Vec<String> {
    let plain: Vec<String> = paths.iter().map(|p| title(p)).collect();
    plain
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let shared = plain.iter().enumerate().any(|(other, text)| {
                other != index && text == name && paths[other] != paths[index]
            });
            if !shared {
                return name.clone();
            }
            match paths[index].parent().and_then(|p| p.file_name()) {
                Some(parent) => format!("{}/{name}", parent.to_string_lossy()),
                None => name.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory tree with a few named places to open tabs on.
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for name in ["one", "two", "three"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        std::fs::create_dir_all(root.path().join("one/src")).unwrap();
        std::fs::create_dir_all(root.path().join("two/src")).unwrap();
        root
    }

    fn tabs_over(root: &Path, names: &[&str]) -> Tabs {
        let mut tabs = Tabs::new(Panel::new(root.join(names[0])));
        for name in &names[1..] {
            tabs.open(Panel::new(root.join(name)));
        }
        tabs
    }

    fn shown(tabs: &Tabs) -> Vec<String> {
        tabs.all().iter().map(|p| title(&p.cwd)).collect()
    }

    #[test]
    fn a_pane_starts_with_one_tab_and_never_has_none() {
        let root = fixture();
        let mut tabs = Tabs::new(Panel::new(root.path().join("one")));
        assert_eq!(tabs.len(), 1);
        assert!(!tabs.is_empty());

        assert!(!tabs.close(), "the last tab does not close");
        assert!(tabs.take().is_none(), "nor does it move to the other pane");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.close_others(), 0);
    }

    #[test]
    fn a_new_tab_opens_beside_the_one_it_came_from() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two"]);
        // Two tabs: one, two - and the second is on show.
        assert_eq!(tabs.active(), 1);

        tabs.activate(0);
        tabs.open(Panel::new(root.path().join("three")));
        assert_eq!(
            shown(&tabs),
            ["one", "three", "two"],
            "beside the tab it was opened from, not at the end"
        );
        assert_eq!(tabs.active(), 1);
        assert_eq!(title(tabs.cwd()), "three");
    }

    #[test]
    fn a_duplicate_starts_where_the_tab_it_came_from_is() {
        let root = fixture();
        let mut tabs = Tabs::new(Panel::new(root.path().join("one")));
        tabs.current_mut().show_hidden = true;
        tabs.current_mut().sort_by = crate::panel::SortBy::Size;

        let copy = tabs.duplicate();
        assert_eq!(copy.cwd, root.path().join("one"));
        assert!(copy.show_hidden, "and with the same settings");
        assert_eq!(copy.sort_by, crate::panel::SortBy::Size);
    }

    #[test]
    fn closing_brings_the_next_tab_forward_and_then_the_previous_one() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two", "three"]);

        tabs.activate(0);
        assert!(tabs.close());
        assert_eq!(shown(&tabs), ["two", "three"]);
        assert_eq!(tabs.active(), 0, "the one that slid into its place");

        // Closing the last tab of the row falls back to the one before it.
        tabs.activate(1);
        assert!(tabs.close());
        assert_eq!(shown(&tabs), ["two"]);
        assert_eq!(tabs.active(), 0);
    }

    #[test]
    fn close_others_keeps_the_one_on_show() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two", "three"]);
        tabs.activate(1);

        assert_eq!(tabs.close_others(), 2);
        assert_eq!(
            shown(&tabs),
            ["two"],
            "the one that was on show is the one left"
        );
        assert_eq!(tabs.active(), 0);
    }

    #[test]
    fn next_and_previous_come_back_round() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two", "three"]);
        tabs.activate(0);

        tabs.next();
        assert_eq!(tabs.active(), 1);
        tabs.next();
        tabs.next();
        assert_eq!(tabs.active(), 0, "round to the first");
        tabs.prev();
        assert_eq!(tabs.active(), 2, "and round to the last");
    }

    #[test]
    fn activate_ignores_an_index_that_is_not_there() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two"]);
        tabs.activate(0);
        tabs.activate(99);
        assert_eq!(tabs.active(), 0, "a stale index is not a panic");
    }

    #[test]
    fn a_tab_moves_between_panes_whole() {
        let root = fixture();
        let mut left = tabs_over(root.path(), &["one", "two"]);
        let mut right = Tabs::new(Panel::new(root.path().join("three")));

        // Mark something, so it is clear the tab moved rather than a path.
        left.activate(1);
        left.current_mut().cursor_to(0);
        left.current_mut().toggle_mark();
        let marked = left.current().marked_count();

        let moved = left.take().expect("two tabs, so one can go");
        right.accept(moved);

        assert_eq!(shown(&left), ["one"]);
        assert_eq!(shown(&right), ["three", "two"]);
        assert_eq!(right.active(), 1, "and the pane it arrived at shows it");
        assert_eq!(
            right.current().marked_count(),
            marked,
            "the tab arrived as it was, marks and all"
        );
    }

    #[test]
    fn a_tab_that_comes_back_into_view_is_read_again() {
        let root = fixture();
        let mut tabs = tabs_over(root.path(), &["one", "two"]);
        tabs.activate(0);
        let listed = tabs.current().entries.len();

        // Something else adds a file while this tab is not the one on show.
        tabs.activate(1);
        std::fs::write(root.path().join("one/added.txt"), "new").unwrap();
        tabs.activate(0);

        assert_eq!(
            tabs.current().entries.len(),
            listed + 1,
            "switching to a tab re-reads it, so it cannot come back stale"
        );
    }

    #[test]
    fn tabs_with_the_same_name_take_their_parent_with_them() {
        let paths: Vec<PathBuf> = [
            "/home/user/one/src",
            "/home/user/two/src",
            "/home/user/notes",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(titles(&paths), ["one/src", "two/src", "notes"]);

        // Nothing to disambiguate, nothing added.
        let plain: Vec<PathBuf> = ["/a/one", "/b/two"].iter().map(PathBuf::from).collect();
        assert_eq!(titles(&plain), ["one", "two"]);

        // Two tabs on the same directory - which Ctrl-T makes in one press -
        // cannot be told apart by any amount of path, so they are not given
        // a longer one that says nothing.
        let twice: Vec<PathBuf> = ["/home/user/src", "/home/user/src"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(titles(&twice), ["src", "src"]);
    }

    #[test]
    fn the_root_directory_keeps_the_name_it_has() {
        assert_eq!(title(Path::new("/")), "/");
        assert_eq!(title(Path::new("/home/user")), "user");
    }
}
