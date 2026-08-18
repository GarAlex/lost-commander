// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The directory tree panel.
//!
//! The tree is held as a **flattened list of visible nodes**, which makes
//! cursor movement, scrolling and rendering trivial - they are all just index
//! arithmetic. Children are read only when a node is expanded, so opening the
//! tree on a large filesystem costs one `read_dir` rather than a full walk.

use std::fs;
use std::path::{Path, PathBuf};

use crate::panel::is_hidden;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub path: PathBuf,
    pub label: String,
    pub depth: usize,
    pub expanded: bool,
    /// Set once we have looked and found nothing to open, so the marker can
    /// stop offering to expand it.
    pub leaf: bool,
    /// False for the files a tree carrying them holds.
    ///
    /// A file is always a leaf and expanding one does nothing, so this is the
    /// only thing that tells it apart - but it is what an operation needs
    /// before it can know whether it is copying one thing or a subtree.
    pub is_dir: bool,
    /// Learned from the same stat that answered `is_dir`, and carried so a
    /// front-end drawing the row does not pay a second one to find out.
    pub is_symlink: bool,
}

#[derive(Debug, Clone)]
pub struct Tree {
    /// Visible nodes, in display order.
    pub nodes: Vec<Node>,
    pub cursor: usize,
    pub show_hidden: bool,
    /// Whether files hang under the directories, or only directories show.
    ///
    /// Off is the classic directory tree, which is a way of getting somewhere.
    /// On makes the tree a way of *working*: the rows are the rows a listing
    /// has, so marking them and copying them mean the same thing, only spread
    /// over more than one directory at a time.
    pub show_files: bool,
}

/// The topmost directory: `/` on Unix, `C:\` on Windows.
pub fn filesystem_root(path: &Path) -> PathBuf {
    path.ancestors()
        .last()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf())
}

fn label_for(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        // The root has no file name; show the path itself.
        .unwrap_or_else(|| path.display().to_string())
}

// `has_children` is gone, and with it the automount guard it carried. The
// eager peek read every visible folder's children to paint its arrow, and
// the guard existed because that peek, walking into /home - an autofs
// trigger on every Mac - blocked the whole tree behind automountd. Nothing
// reads an unopened folder any more, for arrows or anything else, so a
// trigger is only touched when its row is deliberately opened - which is a
// mount request, which is what opening it means.

/// What hangs under `path`: its subdirectories, and its files when asked for.
///
/// Directories first and then files, each sorted case-insensitively - the same
/// order a listing uses, so a directory looks the same whichever of the two
/// ways you look at it. Unreadable entries are skipped rather than failing the
/// whole expansion.
fn children(path: &Path, show_hidden: bool, show_files: bool) -> Vec<(PathBuf, bool, bool)> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };

    let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(metadata) = entry.path().symlink_metadata() else {
            continue;
        };
        if !show_hidden && is_hidden(&name, &metadata) {
            continue;
        }
        // Follow links so a symlinked directory can still be walked.
        let is_symlink = metadata.file_type().is_symlink();
        let is_dir = if is_symlink {
            fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false)
        } else {
            metadata.is_dir()
        };
        if is_dir {
            dirs.push((entry.path(), is_symlink));
        } else if show_files {
            files.push((entry.path(), is_symlink));
        }
    }

    dirs.sort_by_key(|(p, _)| label_for(p).to_lowercase());
    files.sort_by_key(|(p, _)| label_for(p).to_lowercase());
    dirs.into_iter()
        .map(|(p, link)| (p, true, link))
        .chain(files.into_iter().map(|(p, link)| (p, false, link)))
        .collect()
}

impl Tree {
    /// The classic directory tree: somewhere to go, not something to act on.
    pub fn rooted_at(root: &Path, show_hidden: bool) -> Tree {
        Tree::rooted_at_showing(root, show_hidden, false)
    }

    /// The general one, which may carry the files as well.
    ///
    /// `show_files` is fixed at construction rather than flipped later,
    /// because opening a node reads its children once: a tree already walked
    /// would keep the directories it opened before the flag changed and gain
    /// files only in whatever was opened after it.
    pub fn rooted_at_showing(root: &Path, show_hidden: bool, show_files: bool) -> Tree {
        Tree {
            nodes: vec![Node {
                path: root.to_path_buf(),
                label: label_for(root),
                depth: 0,
                expanded: false,
                leaf: false,
                is_dir: true,
                is_symlink: false,
            }],
            cursor: 0,
            show_hidden,
            show_files,
        }
    }

    /// A tree covering the whole filesystem, opened up to `target`.
    pub fn revealing(target: &Path, show_hidden: bool) -> Tree {
        Tree::revealing_showing(target, show_hidden, false)
    }

    pub fn revealing_showing(target: &Path, show_hidden: bool, show_files: bool) -> Tree {
        let mut tree = Tree::rooted_at_showing(&filesystem_root(target), show_hidden, show_files);
        tree.reveal(target);
        tree
    }

    pub fn selected(&self) -> Option<&Node> {
        self.nodes.get(self.cursor)
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected().map(|n| n.path.clone())
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.nodes.iter().position(|n| n.path == path)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.nodes.is_empty() {
            return;
        }
        let last = self.nodes.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.nodes.len().saturating_sub(1);
    }

    /// Read what hangs under a node and splice it in below.
    pub fn expand(&mut self, index: usize) {
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        // A file has nothing under it. Checked rather than left to come back
        // empty, because coming back empty would set `leaf` after the fact and
        // make the twisty appear for as long as it took to open.
        if node.expanded || !node.is_dir {
            return;
        }

        let depth = node.depth;
        let children = children(&node.path, self.show_hidden, self.show_files);

        let node = &mut self.nodes[index];
        node.expanded = true;
        node.leaf = children.is_empty();

        let new_nodes: Vec<Node> = children
            .into_iter()
            .map(|(path, is_dir, is_symlink)| Node {
                label: label_for(&path),
                // Every directory is offered as openable until it is opened.
                //
                // It used to be answered here, eagerly, by reading each
                // child's own children just to paint the arrow - a readdir
                // and a run of stats per visible row, which made revealing a
                // path through a directory of twenty-four thousand entries
                // cost half a second per call, and every fold repaid it.
                // Finder's answer is the honest cheap one: every folder gets
                // a triangle, and an empty one corrects itself when opened -
                // which the code below already did, in the line that sets
                // `leaf` from what expand actually found.
                leaf: !is_dir,
                path,
                depth: depth + 1,
                expanded: false,
                is_dir,
                is_symlink,
            })
            .collect();

        for (offset, child) in new_nodes.into_iter().enumerate() {
            self.nodes.insert(index + 1 + offset, child);
        }
    }

    /// Drop everything nested under a node.
    pub fn collapse(&mut self, index: usize) {
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        if !node.expanded {
            return;
        }
        let depth = node.depth;
        self.nodes[index].expanded = false;

        let mut end = index + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        self.nodes.drain(index + 1..end);

        // Keep the cursor on something that still exists.
        if self.cursor > index && self.cursor >= self.nodes.len() {
            self.cursor = self.nodes.len().saturating_sub(1);
        }
        if self.cursor > index && self.cursor > self.nodes.len().saturating_sub(1) {
            self.cursor = index;
        }
    }

    pub fn toggle(&mut self, index: usize) {
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        if node.expanded {
            self.collapse(index);
        } else {
            self.expand(index);
        }
    }

    /// The visible node one level up from `index`.
    pub fn parent_of(&self, index: usize) -> Option<usize> {
        let depth = self.nodes.get(index)?.depth;
        if depth == 0 {
            return None;
        }
        self.nodes[..index]
            .iter()
            .rposition(|n| n.depth == depth - 1)
    }

    /// Insert `path` among `parent`'s children, keeping them sorted.
    ///
    /// Used by [`reveal`](Self::reveal) for components that the normal listing
    /// filters out - most often a hidden ancestor such as `~/.config`.
    fn insert_child(&mut self, parent: usize, path: &Path) -> Option<usize> {
        let depth = self.nodes.get(parent)?.depth + 1;
        let label = label_for(path);
        let key = label.to_lowercase();

        // Walk the parent's direct children to find the sorted position.
        let mut at = parent + 1;
        while at < self.nodes.len() && self.nodes[at].depth >= depth {
            if self.nodes[at].depth == depth && self.nodes[at].label.to_lowercase() > key {
                break;
            }
            at += 1;
        }

        self.nodes.insert(
            at,
            Node {
                path: path.to_path_buf(),
                label,
                depth,
                expanded: false,
                leaf: false,
                // reveal only ever walks in ancestors of the target, and every
                // one of those is a directory.
                is_dir: true,
                is_symlink: false,
            },
        );
        self.nodes[parent].leaf = false;
        Some(at)
    }

    /// Expand every level from the root down to `target` and park the cursor
    /// there. Used when opening the tree so it starts where the user is.
    ///
    /// Components that the listing would hide are spliced in regardless: if the
    /// user is standing in `~/.config/foo`, the tree has to be able to show it.
    pub fn reveal(&mut self, target: &Path) {
        let Some(root) = self.nodes.first().map(|n| n.path.clone()) else {
            return;
        };
        // Only walk the part of the chain at or below our root; a tree rooted
        // in a subdirectory knows nothing about the levels above it.
        let mut chain: Vec<PathBuf> = target
            .ancestors()
            .filter(|p| p.starts_with(&root))
            .map(|p| p.to_path_buf())
            .collect();
        chain.reverse(); // root first

        for step in &chain {
            if let Some(index) = self.index_of(step) {
                self.expand(index);
                continue;
            }

            // Not listed: splice it under its parent if it really exists.
            let Some(parent) = step.parent().and_then(|p| self.index_of(p)) else {
                break;
            };
            if !step.is_dir() {
                break;
            }
            match self.insert_child(parent, step) {
                Some(index) => self.expand(index),
                None => break,
            }
        }

        if let Some(index) = self.index_of(target) {
            self.cursor = index;
        }
    }

    /// Re-read the tree, keeping the expanded set and the cursor where they
    /// were if those directories still exist.
    pub fn refresh(&mut self) {
        let expanded: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|n| n.expanded)
            .map(|n| n.path.clone())
            .collect();
        let selected = self.selected_path();
        let root = self.nodes.first().map(|n| n.path.clone());

        let Some(root) = root else { return };
        *self = Tree::rooted_at(&root, self.show_hidden);

        for path in expanded {
            if let Some(index) = self.index_of(&path) {
                self.expand(index);
            }
        }
        if let Some(path) = selected {
            if let Some(index) = self.index_of(&path) {
                self.cursor = index;
            }
        }
    }

    /// `+` for "can open", `-` for "open", a space for a known-empty branch.
    ///
    /// ASCII on purpose: the Windows console is not reliably UTF-8.
    pub fn marker(&self, index: usize) -> char {
        match self.nodes.get(index) {
            Some(node) if node.expanded && node.leaf => ' ',
            Some(node) if node.expanded => '-',
            Some(_) => '+',
            None => ' ',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tmp/
    ///   alpha/
    ///     nested/
    ///   beta/
    ///   .hidden/
    ///   file.txt
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("alpha/nested")).unwrap();
        fs::create_dir(root.join("beta")).unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join("file.txt"), "x").unwrap();
        dir
    }

    fn labels(tree: &Tree) -> Vec<String> {
        tree.nodes.iter().map(|n| n.label.clone()).collect()
    }

    #[test]
    fn a_new_tree_has_just_its_root() {
        let dir = fixture();
        let tree = Tree::rooted_at(dir.path(), false);
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.nodes[0].depth, 0);
        assert!(!tree.nodes[0].expanded);
    }

    #[test]
    fn expanding_lists_only_directories_sorted() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);

        // file.txt is not a directory, .hidden is filtered.
        assert_eq!(
            labels(&tree)[1..],
            ["alpha".to_string(), "beta".to_string()]
        );
        assert!(tree.nodes[0].expanded);
        assert_eq!(tree.nodes[1].depth, 1);
    }

    #[test]
    fn hidden_directories_appear_only_when_asked() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), true);
        tree.expand(0);
        assert!(labels(&tree).contains(&".hidden".to_string()));
    }

    #[test]
    fn expanding_is_lazy() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        // "nested" is under alpha, which has not been opened yet.
        assert!(!labels(&tree).contains(&"nested".to_string()));

        let alpha = tree.index_of(&dir.path().join("alpha")).unwrap();
        tree.expand(alpha);
        assert!(labels(&tree).contains(&"nested".to_string()));
    }

    #[test]
    fn collapsing_removes_the_whole_subtree() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        let alpha = tree.index_of(&dir.path().join("alpha")).unwrap();
        tree.expand(alpha);
        assert_eq!(tree.nodes.len(), 4); // root, alpha, nested, beta

        tree.collapse(alpha);
        assert_eq!(tree.nodes.len(), 3);
        assert!(!labels(&tree).contains(&"nested".to_string()));
        assert!(!tree.nodes[alpha].expanded);
    }

    #[test]
    fn collapsing_the_root_leaves_one_node() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        tree.collapse(0);
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.cursor, 0);
    }

    #[test]
    fn toggle_flips_between_expanded_and_collapsed() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.toggle(0);
        assert!(tree.nodes[0].expanded);
        tree.toggle(0);
        assert!(!tree.nodes[0].expanded);
    }

    #[test]
    fn reveal_opens_every_level_down_to_the_target() {
        let dir = fixture();
        let target = dir.path().join("alpha/nested");
        let mut tree = Tree::rooted_at(&filesystem_root(&target), false);
        tree.reveal(&target);

        // The cursor lands on the target, and its ancestors are open.
        assert_eq!(tree.selected_path().unwrap(), target);
        let alpha = tree.index_of(&dir.path().join("alpha")).unwrap();
        assert!(tree.nodes[alpha].expanded);
    }

    #[test]
    fn revealing_constructor_starts_at_the_filesystem_root() {
        let dir = fixture();
        let target = dir.path().join("beta");
        let tree = Tree::revealing(&target, false);

        assert_eq!(tree.nodes[0].path, filesystem_root(&target));
        assert_eq!(tree.selected_path().unwrap(), target);
    }

    #[test]
    fn reveal_reaches_through_a_hidden_ancestor() {
        // Directories like ~/.config are filtered from the listing, but the
        // tree still has to be able to open down into them.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".config/app/data");
        fs::create_dir_all(&target).unwrap();

        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.reveal(&target);

        assert_eq!(tree.selected_path().unwrap(), target);
        let hidden = tree.index_of(&dir.path().join(".config")).unwrap();
        assert!(tree.nodes[hidden].expanded);
        // The parent is no longer considered a leaf now that it has a child.
        assert!(!tree.nodes[0].leaf);
    }

    #[test]
    fn a_spliced_child_lands_in_sorted_position() {
        let dir = fixture();
        // ".hidden" sorts before "alpha" and "beta" once revealed.
        let target = dir.path().join(".hidden");
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        tree.reveal(&target);

        let visible: Vec<String> = tree
            .nodes
            .iter()
            .filter(|n| n.depth == 1)
            .map(|n| n.label.clone())
            .collect();
        assert_eq!(visible, [".hidden", "alpha", "beta"]);
    }

    #[test]
    fn reveal_stops_cleanly_at_a_path_that_does_not_exist() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.reveal(&dir.path().join("nope/deeper"));
        // No panic, no bogus nodes.
        assert!(tree.index_of(&dir.path().join("nope")).is_none());
    }

    #[test]
    fn parent_of_walks_up_one_level() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        let alpha = tree.index_of(&dir.path().join("alpha")).unwrap();
        tree.expand(alpha);
        let nested = tree.index_of(&dir.path().join("alpha/nested")).unwrap();

        assert_eq!(tree.parent_of(nested), Some(alpha));
        assert_eq!(tree.parent_of(alpha), Some(0));
        assert_eq!(tree.parent_of(0), None);
    }

    #[test]
    fn cursor_movement_is_clamped() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);

        tree.move_cursor(-10);
        assert_eq!(tree.cursor, 0);
        tree.move_cursor(100);
        assert_eq!(tree.cursor, tree.nodes.len() - 1);
        tree.cursor_home();
        assert_eq!(tree.cursor, 0);
        tree.cursor_end();
        assert_eq!(tree.cursor, tree.nodes.len() - 1);
    }

    #[test]
    fn markers_reflect_expansion_state() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        assert_eq!(tree.marker(0), '+');

        tree.expand(0);
        assert_eq!(tree.marker(0), '-');

        // beta has no subdirectories, so once opened it is a known leaf.
        let beta = tree.index_of(&dir.path().join("beta")).unwrap();
        tree.expand(beta);
        assert_eq!(tree.marker(beta), ' ');
    }

    #[test]
    fn refresh_keeps_expansion_and_cursor() {
        let dir = fixture();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        let alpha = tree.index_of(&dir.path().join("alpha")).unwrap();
        tree.expand(alpha);
        tree.cursor = tree.index_of(&dir.path().join("alpha/nested")).unwrap();

        // A directory added behind our back shows up after a refresh.
        fs::create_dir(dir.path().join("gamma")).unwrap();
        tree.refresh();

        assert!(labels(&tree).contains(&"gamma".to_string()));
        assert!(labels(&tree).contains(&"nested".to_string()));
        assert_eq!(
            tree.selected_path().unwrap(),
            dir.path().join("alpha/nested")
        );
    }

    /// A directory with a subdirectory, two files and a hidden one.
    fn mixed() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("beta.txt"), "b").unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
        std::fs::write(dir.path().join(".secret"), "s").unwrap();
        dir
    }

    #[test]
    fn the_plain_tree_is_still_directories_only() {
        let dir = mixed();
        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);

        let under: Vec<String> = tree.nodes[1..].iter().map(|n| n.label.clone()).collect();
        assert_eq!(under, vec!["sub"], "the files should not be there");
    }

    #[test]
    fn a_tree_showing_files_puts_them_after_the_directories() {
        let dir = mixed();
        let mut tree = Tree::rooted_at_showing(dir.path(), false, true);
        tree.expand(0);

        // Directories first and then files, each sorted case-insensitively -
        // the same order a listing uses, so a directory looks the same
        // whichever of the two ways you look at it.
        let under: Vec<String> = tree.nodes[1..].iter().map(|n| n.label.clone()).collect();
        assert_eq!(under, vec!["sub", "alpha.txt", "beta.txt"]);
    }

    #[test]
    fn a_file_in_the_tree_says_it_is_not_a_directory() {
        let dir = mixed();
        let mut tree = Tree::rooted_at_showing(dir.path(), false, true);
        tree.expand(0);

        let file = tree.nodes.iter().find(|n| n.label == "beta.txt").unwrap();
        assert!(!file.is_dir);
        // Known without looking, so no twisty is ever offered beside it.
        assert!(file.leaf);

        let sub = tree.nodes.iter().find(|n| n.label == "sub").unwrap();
        assert!(sub.is_dir);
    }

    #[test]
    fn expanding_a_file_does_nothing_at_all() {
        let dir = mixed();
        let mut tree = Tree::rooted_at_showing(dir.path(), false, true);
        tree.expand(0);
        let before = tree.nodes.len();

        let at = tree
            .nodes
            .iter()
            .position(|n| n.label == "beta.txt")
            .unwrap();
        tree.expand(at);

        assert_eq!(tree.nodes.len(), before);
        // And it is not left marked open, which would draw a twisty pointing
        // down beside a file with nothing under it.
        assert!(!tree.nodes[at].expanded);
    }

    #[test]
    fn hidden_files_follow_the_same_rule_the_listing_uses() {
        let dir = mixed();
        let mut tree = Tree::rooted_at_showing(dir.path(), false, true);
        tree.expand(0);
        assert!(!labels(&tree).iter().any(|l| l == ".secret"));

        let mut shown = Tree::rooted_at_showing(dir.path(), true, true);
        shown.expand(0);
        assert!(labels(&shown).iter().any(|l| l == ".secret"));
    }

    #[test]
    fn every_folder_offers_to_open_until_it_is_opened() {
        // The reversal of a trade, made on purpose and measured first. The
        // old contract answered "is there anything under this?" eagerly, by
        // reading every visible folder's children just to paint its arrow -
        // a readdir and a run of stats per row, which priced revealing a
        // path through twenty-four thousand siblings at half a second per
        // call, repaid on every fold. Finder's answer costs nothing and
        // lies only briefly: every folder gets an arrow, and opening an
        // empty one takes the arrow away. That correction is asserted here,
        // because it is now the whole of the contract.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        std::fs::create_dir(dir.path().join("deep")).unwrap();
        std::fs::create_dir(dir.path().join("deep/inner")).unwrap();

        let mut tree = Tree::rooted_at(dir.path(), false);
        tree.expand(0);
        let find = |tree: &Tree, name: &str| {
            tree.nodes
                .iter()
                .position(|n| n.label == name)
                .unwrap_or_else(|| panic!("{name} should be there"))
        };

        // Before opening: both offer, and neither was read to say so.
        assert!(!tree.nodes[find(&tree, "empty")].leaf);
        assert!(!tree.nodes[find(&tree, "deep")].leaf);

        // Opened, each tells the truth it found.
        let at = find(&tree, "empty");
        tree.expand(at);
        assert!(
            tree.nodes[at].leaf,
            "opening an empty folder takes its arrow away"
        );
        let at = find(&tree, "deep");
        tree.expand(at);
        assert!(!tree.nodes[at].leaf, "deep really had something under it");
        assert!(tree.nodes.iter().any(|n| n.label == "inner"));
    }

    #[test]
    fn a_folder_of_only_files_opens_when_the_tree_carries_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("papers")).unwrap();
        std::fs::write(dir.path().join("papers/one.txt"), "x").unwrap();

        let mut tree = Tree::rooted_at_showing(dir.path(), false, true);
        tree.expand(0);
        let papers = tree.nodes.iter().find(|n| n.label == "papers").unwrap();
        assert!(!papers.leaf, "it has a file, and this tree shows files");
    }

    #[test]
    fn an_unreadable_directory_expands_to_nothing_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let mut tree = Tree::rooted_at(&missing, false);
        tree.expand(0);
        assert_eq!(tree.nodes.len(), 1);
        assert!(tree.nodes[0].leaf);
    }
}
