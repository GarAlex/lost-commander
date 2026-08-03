// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Comparing two files line by line, and picking which two.
//!
//! [`compare`] says *that* two files differ; this says how. The alignment is
//! Myers' algorithm, which is what `git diff` and every other line differ
//! uses: it finds the shortest edit script, and its cost is proportional to
//! the size of the difference rather than the size of the files - so two
//! versions of the same file, which is what anyone actually compares, are
//! aligned almost instantly however long they are.
//!
//! [`choose`] is the other half, and it is the part with an opinion: two files
//! marked in one pane, or the row under each pane's cursor. Both gestures mean
//! the same thing and neither needs a file picker.

use std::io;
use std::path::{Path, PathBuf};

use crate::entry::EntryKind;
use crate::panel::Panel;

/// The largest file this will read into memory to align.
///
/// Both files are held as lines at once, and a diff of something larger than
/// this is not a thing anyone reads - it is a thing a program should be doing.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// How much of a file is read to decide whether it is text.
const SNIFF: usize = 4_096;

/// The most edits the alignment will look for before giving up.
///
/// Myers' cost grows with the size of the *difference*, so this only bites on
/// two files that have almost nothing in common - where an alignment would be
/// meaningless anyway, and showing them side by side unaligned is both honest
/// and what the eye wants.
pub const MAX_EDITS: usize = 100_000;

/// One line of the two-column view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The same on both sides.
    Same {
        left: usize,
        right: usize,
        text: String,
    },
    /// On the left and not on the right.
    OnlyLeft { left: usize, text: String },
    /// On the right and not on the left.
    OnlyRight { right: usize, text: String },
}

impl Row {
    pub fn is_same(&self) -> bool {
        matches!(self, Row::Same { .. })
    }

    /// What the left column shows: the text and its line number.
    pub fn left(&self) -> Option<(usize, &str)> {
        match self {
            Row::Same { left, text, .. } | Row::OnlyLeft { left, text } => {
                Some((*left, text.as_str()))
            }
            Row::OnlyRight { .. } => None,
        }
    }

    pub fn right(&self) -> Option<(usize, &str)> {
        match self {
            Row::Same { right, text, .. } | Row::OnlyRight { right, text } => {
                Some((*right, text.as_str()))
            }
            Row::OnlyLeft { .. } => None,
        }
    }
}

/// Two files lined up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    /// How many rows are not the same on both sides.
    pub changes: usize,
    /// The alignment gave up and the two are shown side by side as they are.
    pub unaligned: bool,
}

impl Diff {
    pub fn is_identical(&self) -> bool {
        self.changes == 0 && !self.unaligned
    }

    /// The next row after `from` that is not the same on both sides.
    ///
    /// Wraps, because a reader walking the differences of a file with one near
    /// the top and one near the bottom should not have to scroll back by hand.
    pub fn next_change(&self, from: usize) -> Option<usize> {
        let n = self.rows.len();
        (1..=n)
            .map(|step| (from + step) % n.max(1))
            .find(|&at| self.rows.get(at).map(|r| !r.is_same()).unwrap_or(false))
    }

    pub fn previous_change(&self, from: usize) -> Option<usize> {
        let n = self.rows.len();
        (1..=n)
            .map(|step| (from + n - (step % n.max(1))) % n.max(1))
            .find(|&at| self.rows.get(at).map(|r| !r.is_same()).unwrap_or(false))
    }
}

/// One step of an edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Keep,
    Delete,
    Insert,
}

/// Myers' shortest edit script, or `None` when it is longer than `max_edits`.
///
/// The greedy version with the trace kept for the walk back. `v[k]` is the
/// furthest point reached on diagonal `k`, and the answer is the first `d`
/// whose search touches the far corner - `d` being the number of insertions
/// and deletions, which is why similar files are cheap however long they are.
fn script(left: &[&str], right: &[&str], max_edits: usize) -> Option<Vec<Edit>> {
    let (n, m) = (left.len(), right.len());
    let max = (n + m).min(max_edits);
    // A slot either side of the widest diagonal, because the search reads
    // `k - 1` and `k + 1` - including when there is nothing to compare at all
    // and the widest diagonal is the only one.
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let at = (k + offset) as usize;
            // Go down when the diagonal below reached further, which is what
            // makes the script prefer deletions before insertions and so keeps
            // the two files' hunks in the order they are read.
            let down = k == -d || (k != d && v[at - 1] < v[at + 1]);
            let mut x = if down { v[at + 1] } else { v[at - 1] + 1 };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && left[x as usize] == right[y as usize] {
                x += 1;
                y += 1;
            }
            v[at] = x;
            if x as usize >= n && y as usize >= m {
                return Some(walk_back(&trace, n, m, offset));
            }
            k += 2;
        }
    }
    None
}

/// Turn the trace into the script, read backwards from the far corner.
fn walk_back(trace: &[Vec<isize>], n: usize, m: usize, offset: isize) -> Vec<Edit> {
    let mut out = Vec::new();
    let (mut x, mut y) = (n as isize, m as isize);

    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let at = (k + offset) as usize;
        let down = k == -d || (k != d && v[at - 1] < v[at + 1]);
        let previous_k = if down { k + 1 } else { k - 1 };
        let previous_x = v[(previous_k + offset) as usize];
        let previous_y = previous_x - previous_k;

        while x > previous_x && y > previous_y {
            out.push(Edit::Keep);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            out.push(if down { Edit::Insert } else { Edit::Delete });
            x = previous_x;
            y = previous_y;
        }
    }
    out.reverse();
    out
}

/// Line up two files.
pub fn align(left: &[String], right: &[String]) -> Diff {
    let left_refs: Vec<&str> = left.iter().map(String::as_str).collect();
    let right_refs: Vec<&str> = right.iter().map(String::as_str).collect();

    let Some(edits) = script(&left_refs, &right_refs, MAX_EDITS) else {
        return side_by_side(left, right);
    };

    let mut rows = Vec::new();
    let (mut l, mut r) = (0usize, 0usize);
    for edit in edits {
        match edit {
            Edit::Keep => {
                rows.push(Row::Same {
                    left: l + 1,
                    right: r + 1,
                    text: left[l].clone(),
                });
                l += 1;
                r += 1;
            }
            Edit::Delete => {
                rows.push(Row::OnlyLeft {
                    left: l + 1,
                    text: left[l].clone(),
                });
                l += 1;
            }
            Edit::Insert => {
                rows.push(Row::OnlyRight {
                    right: r + 1,
                    text: right[r].clone(),
                });
                r += 1;
            }
        }
    }
    let changes = rows.iter().filter(|row| !row.is_same()).count();
    Diff {
        rows,
        changes,
        unaligned: false,
    }
}

/// Two files with nothing in common, shown as they are.
fn side_by_side(left: &[String], right: &[String]) -> Diff {
    let mut rows = Vec::new();
    for at in 0..left.len().max(right.len()) {
        match (left.get(at), right.get(at)) {
            (Some(l), Some(_)) => rows.push(Row::OnlyLeft {
                left: at + 1,
                text: l.clone(),
            }),
            (Some(l), None) => rows.push(Row::OnlyLeft {
                left: at + 1,
                text: l.clone(),
            }),
            (None, Some(r)) => rows.push(Row::OnlyRight {
                right: at + 1,
                text: r.clone(),
            }),
            (None, None) => {}
        }
        if let (Some(_), Some(r)) = (left.get(at), right.get(at)) {
            rows.push(Row::OnlyRight {
                right: at + 1,
                text: r.clone(),
            });
        }
    }
    let changes = rows.len();
    Diff {
        rows,
        changes,
        unaligned: true,
    }
}

/// Why two files could not be compared line by line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not text - a comparison of it is a yes or a no, not a diff.
    Binary {
        /// Where the two first differ, and `None` when they never do. An
        /// offset is somewhere you can go and look; "they differ" is not.
        at: Option<u64>,
    },
    TooBig {
        bytes: u64,
    },
    Unreadable(String),
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Binary { at: None } => {
                "These are not text files, but their contents are identical.".to_string()
            }
            Refusal::Binary { at: Some(offset) } => format!(
                "These are not text files. They first differ at byte {offset} ({offset:#x}) - \
                 F3 on either shows it.",
            ),
            Refusal::TooBig { bytes } => format!(
                "Too big to compare line by line ({}, and the limit is {}).",
                crate::entry::human_size(*bytes),
                crate::entry::human_size(MAX_BYTES)
            ),
            Refusal::Unreadable(e) => e.clone(),
        }
    }
}

/// Split into lines, keeping neither the newline nor a phantom last line.
///
/// A file that ends with a newline has as many lines as it has newlines, not
/// one more: the trailing empty string is an artefact of splitting, and shown
/// in a diff it is an off-by-one that never goes away.
pub fn lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// Read a file for comparison, or say why not.
pub fn read(path: &Path) -> Result<Vec<String>, Refusal> {
    let meta = path
        .metadata()
        .map_err(|e| Refusal::Unreadable(format!("{}: {e}", path.display())))?;
    if meta.len() > MAX_BYTES {
        return Err(Refusal::TooBig { bytes: meta.len() });
    }
    let bytes =
        std::fs::read(path).map_err(|e| Refusal::Unreadable(format!("{}: {e}", path.display())))?;
    if !crate::preview::looks_like_text(&bytes[..bytes.len().min(SNIFF)]) {
        // Left for the caller to answer with both files in hand.
        return Err(Refusal::Binary { at: Some(0) });
    }
    Ok(lines(&String::from_utf8_lossy(&bytes)))
}

/// Compare two files, reading both.
pub fn compare_files(left: &Path, right: &Path) -> Result<Diff, Refusal> {
    match (read(left), read(right)) {
        (Ok(left_lines), Ok(right_lines)) => Ok(align(&left_lines, &right_lines)),
        // Either one being binary makes this a yes-or-no question, and the
        // answer is worth reading the whole of both files for.
        (Err(Refusal::Binary { .. }), _) | (_, Err(Refusal::Binary { .. })) => {
            let at = crate::hex::first_difference(left, right).unwrap_or(Some(0));
            Err(Refusal::Binary { at })
        }
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

/// Two files to compare, and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub left: PathBuf,
    pub right: PathBuf,
    /// True when both came from one pane rather than one from each.
    pub from_one_pane: bool,
}

/// Which two files a "compare files" is about.
///
/// Two ways of saying it, and they are the two ways anyone would: **mark two
/// files in one pane**, or **put one under each pane's cursor**. Marks win
/// where there are exactly two of them, because marking two files is a
/// deliberate act and a cursor is only ever wherever it was left.
///
/// The panes are given in the order they are on screen, and a pair taken one
/// from each comes back in that order too - the left pane's file on the left.
/// Anything else reads as though the two files had swapped places. Which pane
/// has the keyboard only decides whose marks are looked at first, so marking a
/// pair and then tabbing across still does what you meant.
pub fn choose(
    left_pane: &Panel,
    right_pane: &Panel,
    active_is_left: bool,
) -> Result<Chosen, String> {
    choose_from(&Side::of(left_pane), &Side::of(right_pane), active_is_left)
}

/// What one pane offers a comparison: what is marked in it, and what its
/// cursor is on.
///
/// Enough to decide with, and no more - which is what lets a front-end that
/// has no `Panel` ask the same question and get the same answer.
#[derive(Debug, Clone, Default)]
pub struct Side {
    /// The marked entries that are files, in the order they are listed.
    pub marked: Vec<PathBuf>,
    /// The row under the cursor, when it is a file.
    pub cursor: Option<PathBuf>,
}

impl Side {
    pub fn of(panel: &Panel) -> Side {
        Side {
            marked: marked_files(panel),
            cursor: panel
                .selected()
                .filter(|e| e.kind == EntryKind::File)
                .map(|e| e.path.clone()),
        }
    }
}

/// [`choose`], from what the two panes offer rather than from the panes.
///
/// Split out so that a front-end which keeps its own idea of a pane can reach
/// the same decision. The rule is worth having once: which of two files goes
/// on the left is not obvious, and getting it from "whichever pane has the
/// keyboard" makes the columns swap places depending on where you last clicked.
pub fn choose_from(
    left_side: &Side,
    right_side: &Side,
    active_is_left: bool,
) -> Result<Chosen, String> {
    let (active, other) = if active_is_left {
        (left_side, right_side)
    } else {
        (right_side, left_side)
    };
    let mine = active.marked.clone();
    let theirs = other.marked.clone();

    let pair = if mine.len() == 2 {
        Some((mine[0].clone(), mine[1].clone(), true))
    } else if mine.is_empty() && theirs.len() == 2 {
        Some((theirs[0].clone(), theirs[1].clone(), true))
    } else if mine.len() > 2 || (mine.is_empty() && theirs.len() > 2) {
        // Marks that clearly mean "these" but are not a pair. Falling back to
        // the cursors here would compare two files nobody pointed at.
        return Err(format!(
            "Mark exactly two files to compare - there are {}",
            mine.len().max(theirs.len())
        ));
    } else {
        None
    };

    if let Some((left, right, from_one_pane)) = pair {
        return Ok(Chosen {
            left,
            right,
            from_one_pane,
        });
    }

    // One from each pane, which is the gesture when there is no pair - and in
    // the order the panes are on screen, whichever of them has the keyboard.
    let left = one_file(left_side).ok_or_else(|| {
        "Put a file under each pane's cursor, or mark two in one pane".to_string()
    })?;
    let right = one_file(right_side).ok_or_else(|| {
        "Put a file under each pane's cursor, or mark two in one pane".to_string()
    })?;
    if left == right {
        return Err("Both are the same file".to_string());
    }
    Ok(Chosen {
        left,
        right,
        from_one_pane: false,
    })
}

/// The marked entries that are files, in the order they are listed.
fn marked_files(panel: &Panel) -> Vec<PathBuf> {
    panel
        .entries
        .iter()
        .filter(|e| e.marked && e.kind == EntryKind::File)
        .map(|e| e.path.clone())
        .collect()
}

/// The one file this pane means: its single mark, or the row under the cursor.
///
/// The same rule the rest of the program uses for a single-file operation -
/// a mark beats the cursor - so one file marked here and a cursor over there
/// is a pair. More than one mark is not "the one", and says so.
fn one_file(side: &Side) -> Option<PathBuf> {
    match side.marked.len() {
        1 => Some(side.marked[0].clone()),
        0 => side.cursor.clone(),
        _ => None,
    }
}

/// A line's number as the gutter shows it, or blank where there is no line.
pub fn gutter(number: Option<usize>, width: usize) -> String {
    match number {
        Some(n) => format!("{n:>width$}"),
        None => " ".repeat(width),
    }
}

/// How wide the line-number gutter has to be for this diff.
pub fn gutter_width(diff: &Diff) -> usize {
    let widest = diff
        .rows
        .iter()
        .map(|row| match row {
            Row::Same { left, right, .. } => (*left).max(*right),
            Row::OnlyLeft { left, .. } => *left,
            Row::OnlyRight { right, .. } => *right,
        })
        .max()
        .unwrap_or(1);
    widest.to_string().len().max(2)
}

/// Read both files and hand back a diff, or the reason there is not one.
pub fn open(chosen: &Chosen) -> Result<Diff, Refusal> {
    compare_files(&chosen.left, &chosen.right)
}

/// The io error a caller can turn into a [`Refusal`].
#[allow(dead_code)]
fn unreadable(e: io::Error) -> Refusal {
    Refusal::Unreadable(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    /// A diff as `-left`, `+right`, ` same`, which is how one reads.
    fn shown(diff: &Diff) -> Vec<String> {
        diff.rows
            .iter()
            .map(|row| match row {
                Row::Same { text, .. } => format!(" {text}"),
                Row::OnlyLeft { text, .. } => format!("-{text}"),
                Row::OnlyRight { text, .. } => format!("+{text}"),
            })
            .collect()
    }

    #[test]
    fn two_files_the_same_have_no_differences() {
        let same = text(&["one", "two", "three"]);
        let diff = align(&same, &same);
        assert!(diff.is_identical());
        assert_eq!(diff.changes, 0);
        assert_eq!(diff.rows.len(), 3);
        assert!(diff.rows.iter().all(Row::is_same));
    }

    #[test]
    fn a_changed_line_is_a_deletion_and_an_insertion() {
        let left = text(&["one", "two", "three"]);
        let right = text(&["one", "TWO", "three"]);
        let diff = align(&left, &right);
        assert_eq!(shown(&diff), [" one", "-two", "+TWO", " three"]);
        assert_eq!(diff.changes, 2);
    }

    #[test]
    fn lines_added_and_removed_keep_the_rest_lined_up() {
        let left = text(&["a", "b", "c", "d"]);
        let right = text(&["a", "c", "d", "e"]);
        let diff = align(&left, &right);
        assert_eq!(shown(&diff), [" a", "-b", " c", " d", "+e"]);
        assert_eq!(diff.changes, 2);
    }

    #[test]
    fn the_line_numbers_are_each_files_own() {
        let left = text(&["a", "b", "c"]);
        let right = text(&["a", "c"]);
        let diff = align(&left, &right);
        // b is line 2 on the left and nothing on the right; c is line 3 on
        // the left and line 2 on the right.
        assert_eq!(
            diff.rows[1],
            Row::OnlyLeft {
                left: 2,
                text: "b".into()
            }
        );
        assert_eq!(
            diff.rows[2],
            Row::Same {
                left: 3,
                right: 2,
                text: "c".into()
            }
        );
        assert_eq!(diff.rows[2].left(), Some((3, "c")));
        assert_eq!(diff.rows[2].right(), Some((2, "c")));
        assert_eq!(diff.rows[1].right(), None);
    }

    #[test]
    fn an_empty_file_against_a_full_one_is_all_insertions() {
        let diff = align(&[], &text(&["a", "b"]));
        assert_eq!(shown(&diff), ["+a", "+b"]);

        let diff = align(&text(&["a", "b"]), &[]);
        assert_eq!(shown(&diff), ["-a", "-b"]);

        let diff = align(&[], &[]);
        assert!(diff.is_identical());
        assert!(diff.rows.is_empty());
    }

    #[test]
    fn a_block_moved_is_a_removal_and_an_addition() {
        // Myers finds the shortest script, not the smartest one: a block that
        // moved reads as gone from one place and arrived in another, which is
        // what every line differ says about it.
        let left = text(&["header", "one", "two", "footer"]);
        let right = text(&["header", "two", "one", "footer"]);
        let diff = align(&left, &right);
        assert_eq!(diff.changes, 2);
        assert_eq!(shown(&diff), [" header", "-one", " two", "+one", " footer"]);
    }

    #[test]
    fn walking_the_differences_wraps_round() {
        let left = text(&["a", "b", "c", "d", "e"]);
        let right = text(&["a", "B", "c", "d", "E"]);
        let diff = align(&left, &right);
        let firsts: Vec<usize> = diff
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_same())
            .map(|(i, _)| i)
            .collect();
        assert!(firsts.len() >= 2);

        let first = diff.next_change(0).unwrap();
        assert_eq!(first, firsts[0]);
        let last = *firsts.last().unwrap();
        assert_eq!(
            diff.next_change(last),
            Some(firsts[0]),
            "past the last difference comes back to the first"
        );
        assert_eq!(diff.previous_change(firsts[0]), Some(last));
    }

    #[test]
    fn two_files_with_nothing_in_common_are_shown_as_they_are() {
        // The alignment is capped, and past it there is no alignment worth
        // drawing - so both files are shown, and the diff says as much rather
        // than pretending.
        let left = text(&["a", "b", "c"]);
        let right = text(&["x", "y", "z"]);
        let capped = {
            let l: Vec<&str> = left.iter().map(String::as_str).collect();
            let r: Vec<&str> = right.iter().map(String::as_str).collect();
            script(&l, &r, 2)
        };
        assert!(capped.is_none(), "six edits is more than the cap of two");

        let diff = side_by_side(&left, &right);
        assert!(diff.unaligned);
        assert!(!diff.is_identical());
        assert_eq!(diff.rows.len(), 6);
    }

    #[test]
    fn splitting_into_lines_invents_none() {
        assert_eq!(lines("one\ntwo\n"), ["one", "two"]);
        assert_eq!(lines("one\ntwo"), ["one", "two"]);
        assert_eq!(lines(""), Vec::<String>::new());
        assert_eq!(lines("\n"), [""]);
        // Windows line endings are line endings.
        assert_eq!(lines("one\r\ntwo\r\n"), ["one", "two"]);
    }

    #[test]
    fn the_gutter_is_as_wide_as_the_longest_line_number() {
        let short = align(&text(&["a"]), &text(&["a"]));
        assert_eq!(gutter_width(&short), 2, "never narrower than two");

        let long: Vec<String> = (1..=1200).map(|n| n.to_string()).collect();
        let diff = align(&long, &long);
        assert_eq!(gutter_width(&diff), 4);
        assert_eq!(gutter(Some(7), 4), "   7");
        assert_eq!(gutter(None, 4), "    ");
    }

    /// A pair of panels over a directory with a few files and a directory.
    fn panels() -> (tempfile::TempDir, Panel, Panel) {
        let root = tempfile::tempdir().unwrap();
        let (left, right) = (root.path().join("left"), root.path().join("right"));
        std::fs::create_dir_all(left.join("sub")).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(left.join(name), format!("in the left {name}\n")).unwrap();
        }
        std::fs::write(right.join("a.txt"), "in the right a.txt\n").unwrap();
        (root, Panel::new(left), Panel::new(right))
    }

    fn mark(panel: &mut Panel, names: &[&str]) {
        for entry in panel.entries.iter_mut() {
            entry.marked = names.contains(&entry.name.as_str());
        }
    }

    fn put_cursor(panel: &mut Panel, name: &str) {
        let at = panel
            .entries
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        panel.cursor_to(at);
    }

    #[test]
    fn two_marked_in_one_pane_are_the_two() {
        let (_root, mut left, right) = panels();
        mark(&mut left, &["a.txt", "c.txt"]);
        // The cursor is somewhere else entirely, and does not matter.
        put_cursor(&mut left, "b.txt");

        let chosen = choose(&left, &right, true).unwrap();
        assert_eq!(chosen.left.file_name().unwrap(), "a.txt");
        assert_eq!(chosen.right.file_name().unwrap(), "c.txt");
        assert!(chosen.from_one_pane);
    }

    #[test]
    fn a_pair_from_the_two_panes_comes_back_in_pane_order() {
        // Whichever pane has the keyboard, the left pane's file is the left
        // column - anything else reads as though the files had swapped places.
        let (_root, mut left, mut right) = panels();
        put_cursor(&mut left, "b.txt");
        put_cursor(&mut right, "a.txt");

        for active_is_left in [true, false] {
            let chosen = choose(&left, &right, active_is_left).unwrap();
            assert_eq!(
                chosen.left.parent(),
                Some(left.cwd.as_path()),
                "active_is_left = {active_is_left}"
            );
            assert_eq!(chosen.left.file_name().unwrap(), "b.txt");
            assert_eq!(chosen.right.file_name().unwrap(), "a.txt");
        }
    }

    #[test]
    fn one_under_each_cursor_when_nothing_is_marked() {
        let (_root, mut left, mut right) = panels();
        put_cursor(&mut left, "b.txt");
        put_cursor(&mut right, "a.txt");

        let chosen = choose(&left, &right, true).unwrap();
        assert_eq!(chosen.left.file_name().unwrap(), "b.txt");
        assert_eq!(chosen.right.file_name().unwrap(), "a.txt");
        assert!(!chosen.from_one_pane);
    }

    #[test]
    fn marks_in_the_other_pane_count_when_this_one_has_none() {
        // Marking a pair and then tabbing across still does what you meant.
        let (_root, left, mut right) = panels();
        std::fs::write(right.cwd.join("d.txt"), "d\n").unwrap();
        let mut right = {
            right.reload();
            right
        };
        mark(&mut right, &["a.txt", "d.txt"]);

        let chosen = choose(&left, &right, true).unwrap();
        assert!(chosen.from_one_pane);
        assert_eq!(chosen.left.file_name().unwrap(), "a.txt");
        assert_eq!(chosen.right.file_name().unwrap(), "d.txt");
    }

    #[test]
    fn the_active_panes_marks_win_over_the_others() {
        let (_root, mut left, mut right) = panels();
        std::fs::write(right.cwd.join("d.txt"), "d\n").unwrap();
        right.reload();
        mark(&mut left, &["a.txt", "b.txt"]);
        mark(&mut right, &["a.txt", "d.txt"]);

        // With the left pane active it is the left pane's pair...
        let chosen = choose(&left, &right, true).unwrap();
        assert_eq!(chosen.left.parent(), Some(left.cwd.as_path()));
        assert_eq!(chosen.right.file_name().unwrap(), "b.txt");

        // ...and with the right pane active it is the right pane's.
        let chosen = choose(&left, &right, false).unwrap();
        assert_eq!(chosen.left.parent(), Some(right.cwd.as_path()));
        assert_eq!(chosen.right.file_name().unwrap(), "d.txt");
    }

    #[test]
    fn marks_that_are_not_a_pair_are_a_message_rather_than_a_guess() {
        let (_root, mut left, mut right) = panels();
        put_cursor(&mut right, "a.txt");
        mark(&mut left, &["a.txt", "b.txt", "c.txt"]);
        let error = choose(&left, &right, true).unwrap_err();
        assert!(error.contains("exactly two"), "{error}");
    }

    #[test]
    fn one_mark_here_and_a_cursor_there_is_a_pair() {
        // A mark beats the cursor for a single file, as it does for every
        // other operation in the program.
        let (_root, mut left, mut right) = panels();
        mark(&mut left, &["c.txt"]);
        put_cursor(&mut left, "a.txt");
        put_cursor(&mut right, "a.txt");

        let chosen = choose(&left, &right, true).unwrap();
        assert_eq!(chosen.left.file_name().unwrap(), "c.txt");
        assert_eq!(chosen.right.file_name().unwrap(), "a.txt");
        assert!(!chosen.from_one_pane);
    }

    #[test]
    fn a_directory_is_never_one_of_the_two() {
        let (_root, mut left, mut right) = panels();
        put_cursor(&mut left, "sub");
        put_cursor(&mut right, "a.txt");
        let error = choose(&left, &right, true).unwrap_err();
        assert!(error.contains("cursor"), "{error}");

        // And the same for the parent row.
        left.cursor_home();
        assert!(choose(&left, &right, true).is_err());

        // A marked directory is not a file either, so marking one beside a
        // file leaves one file rather than a pair - and one file plus the
        // other pane's cursor is a perfectly good pair.
        mark(&mut left, &["sub", "a.txt"]);
        let chosen = choose(&left, &right, true).unwrap();
        assert_eq!(chosen.left.file_name().unwrap(), "a.txt");
    }

    #[test]
    fn a_file_is_not_compared_with_itself() {
        let (_root, mut left, mut right) = panels();
        right.chdir(left.cwd.clone());
        put_cursor(&mut left, "a.txt");
        put_cursor(&mut right, "a.txt");
        let error = choose(&left, &right, true).unwrap_err();
        assert!(error.contains("same file"), "{error}");
    }

    #[test]
    fn reading_refuses_what_it_cannot_show() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("a.bin");
        std::fs::write(&binary, [0u8, 1, 2, 3, 0, 5]).unwrap();
        assert!(matches!(read(&binary), Err(Refusal::Binary { .. })));

        let missing = dir.path().join("nope");
        assert!(matches!(read(&missing), Err(Refusal::Unreadable(_))));

        let text = dir.path().join("a.txt");
        std::fs::write(&text, "one\ntwo\n").unwrap();
        assert_eq!(read(&text).unwrap(), ["one", "two"]);
    }

    #[test]
    fn two_files_nobody_can_read_get_a_yes_or_a_no() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a.bin"),
            dir.path().join("b.bin"),
            dir.path().join("c.bin"),
        );
        std::fs::write(&a, [0u8, 1, 2, 3]).unwrap();
        std::fs::write(&b, [0u8, 1, 2, 3]).unwrap();
        std::fs::write(&c, [0u8, 9, 9, 9]).unwrap();

        assert_eq!(compare_files(&a, &b), Err(Refusal::Binary { at: None }));
        assert_eq!(
            compare_files(&a, &c),
            Err(Refusal::Binary { at: Some(1) }),
            "they agree on the first byte and differ on the second"
        );
        assert!(Refusal::Binary { at: None }.message().contains("identical"));
        let said = Refusal::Binary { at: Some(70_000) }.message();
        assert!(said.contains("70000") && said.contains("0x11170"), "{said}");
    }

    #[test]
    fn comparing_two_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        std::fs::write(&b, "one\nTWO\nthree\n").unwrap();

        let diff = compare_files(&a, &b).unwrap();
        assert_eq!(shown(&diff), [" one", "-two", "+TWO", " three"]);
    }

    #[test]
    fn a_long_file_with_one_change_is_cheap_to_align() {
        // The point of Myers: cost follows the size of the difference, not
        // the size of the files. Ten thousand lines and one edit.
        let left: Vec<String> = (0..10_000).map(|n| format!("line {n}")).collect();
        let mut right = left.clone();
        right[5_000] = "changed".to_string();

        let diff = align(&left, &right);
        assert_eq!(diff.changes, 2, "one line out, one line in");
        assert!(!diff.unaligned);
    }
}
