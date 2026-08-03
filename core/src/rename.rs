// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Renaming a whole selection at once.
//!
//! The idea, and most of the notation, is Total Commander's multi-rename
//! tool: you write what the new name should be made of rather than typing
//! each one, and you see the whole list of old -> new before a single file
//! moves. `photo_0001.jpg`, `photo_0002.jpg` out of whatever the camera
//! called them is a two-second job that way and an afternoon by hand.
//!
//! Everything here is pure: [`plan`] turns a selection and a set of rules
//! into the list a dialog shows, and [`steps`] turns that list into the
//! filesystem moves, in an order where nothing lands on a name that has not
//! moved out of the way yet. Only [`apply`] touches the disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::entry::Entry;
use crate::mount::Platform;

/// What the name field starts as: the name it already has.
pub const KEEP_NAME: &str = "[N]";

/// What the extension field starts as.
pub const KEEP_EXTENSION: &str = "[E]";

/// The characters no name may contain, on any platform.
///
/// A rename stays in its directory, so a separator is never a name - it is a
/// request to move somewhere, which this tool does not do.
const ALWAYS_ILLEGAL: &[char] = &['/', '\0'];

/// The rest of what Windows refuses, which is a good deal more.
const WINDOWS_ILLEGAL: &[char] = &['\\', ':', '*', '?', '"', '<', '>', '|'];

/// Names Windows will not give a file whatever the extension.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// How the finished name is cased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    /// However the rules left it.
    #[default]
    Keep,
    Lower,
    Upper,
    /// Every Word Capitalised.
    Title,
    /// Only the first, like a sentence.
    First,
}

impl Case {
    pub const ALL: [Case; 5] = [
        Case::Keep,
        Case::Lower,
        Case::Upper,
        Case::Title,
        Case::First,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Case::Keep => "as written",
            Case::Lower => "lower case",
            Case::Upper => "UPPER CASE",
            Case::Title => "Title Case",
            Case::First => "First letter",
        }
    }

    /// The next option, wrapping, for a field you cycle with one key.
    pub fn next(&self) -> Case {
        let at = Case::ALL.iter().position(|c| c == self).unwrap_or(0);
        Case::ALL[(at + 1) % Case::ALL.len()]
    }

    pub fn prev(&self) -> Case {
        let at = Case::ALL.iter().position(|c| c == self).unwrap_or(0);
        Case::ALL[(at + Case::ALL.len() - 1) % Case::ALL.len()]
    }

    /// Apply to a finished name.
    ///
    /// Word boundaries are anything that is not a letter or a digit, so
    /// `my_holiday-photo.jpg` title-cases every part of it.
    pub fn apply(&self, text: &str) -> String {
        match self {
            Case::Keep => text.to_string(),
            Case::Lower => text.to_lowercase(),
            Case::Upper => text.to_uppercase(),
            Case::Title => {
                let mut out = String::with_capacity(text.len());
                let mut fresh = true;
                for c in text.chars() {
                    if fresh {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    fresh = !c.is_alphanumeric();
                }
                out
            }
            Case::First => {
                let mut out = String::with_capacity(text.len());
                let mut fresh = true;
                for c in text.chars() {
                    if fresh && c.is_alphabetic() {
                        out.extend(c.to_uppercase());
                        fresh = false;
                    } else {
                        out.extend(c.to_lowercase());
                    }
                }
                out
            }
        }
    }
}

/// What to make the new names out of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// The template for the part before the extension.
    pub name: String,
    /// The template for the extension. Empty leaves the file without one.
    pub extension: String,
    /// Text to look for in the assembled name, and what to put there instead.
    pub find: String,
    pub replace: String,
    /// Whether `find` has to match case, which also decides whether the
    /// counter's and the templates' output is compared exactly. Names are
    /// otherwise left alone.
    pub case_sensitive: bool,
    pub case: Case,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            name: KEEP_NAME.to_string(),
            extension: KEEP_EXTENSION.to_string(),
            find: String::new(),
            replace: String::new(),
            case_sensitive: false,
            case: Case::Keep,
        }
    }
}

impl Rules {
    /// Whether these rules would leave every name exactly as it is.
    ///
    /// The dialog opens in this state, and there is nothing to preview or to
    /// warn about until something has been typed.
    pub fn is_identity(&self) -> bool {
        *self == Rules::default()
    }
}

/// One file to be renamed.
///
/// Not an [`Entry`], because the templates need very little of one and a
/// test should not have to build a directory listing to check a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub name: String,
    /// Where the date placeholders come from.
    pub modified: Option<SystemTime>,
}

impl Source {
    pub fn new(path: impl Into<PathBuf>, name: impl Into<String>) -> Source {
        Source {
            path: path.into(),
            name: name.into(),
            modified: None,
        }
    }

    pub fn from_entry(entry: &Entry) -> Source {
        Source {
            path: entry.path.clone(),
            name: entry.name.clone(),
            modified: entry.modified,
        }
    }
}

/// A counter's start, its step, and how many digits it is padded to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counter {
    pub start: i64,
    pub step: i64,
    /// Zero means "as many digits as the number needs".
    pub width: usize,
}

impl Default for Counter {
    fn default() -> Self {
        Counter {
            start: 1,
            step: 1,
            width: 0,
        }
    }
}

impl Counter {
    /// What this counter reads at the nth file, zero-based.
    pub fn at(&self, index: usize) -> String {
        let value = self.start + self.step * index as i64;
        format!("{value:0width$}", width = self.width)
    }
}

/// Read the part of a `[C…]` placeholder that follows the C.
///
/// `` is 1, 1, unpadded; `10` starts at ten; `001` starts at one and pads to
/// three, the leading zero being both the request and the width; `1+2` steps
/// by two; `+2` does the same from one. Anything else is not a counter, and
/// the placeholder is left in the name as the literal text it is.
pub fn parse_counter(spec: &str) -> Option<Counter> {
    let (start_text, step_text) = match spec.split_once('+') {
        Some((s, t)) => (s, Some(t)),
        None => (spec, None),
    };

    let start = if start_text.is_empty() {
        1
    } else {
        start_text.parse::<i64>().ok()?
    };
    let step = match step_text {
        None => 1,
        Some(text) => text.parse::<i64>().ok()?,
    };
    // A leading zero is how you ask for padding, and the digits you write are
    // the width you want: [C001] counts 001, 002, ... 010.
    let width = if start_text.starts_with('0') && start_text.len() > 1 {
        start_text.len()
    } else {
        0
    };
    Some(Counter { start, step, width })
}

/// A file name split into the part templates call `[N]` and the part they
/// call `[E]`, with the dot belonging to neither.
///
/// The split is at the last dot, so `archive.tar.gz` is `archive.tar` plus
/// `gz`. A name that begins with a dot is all name: `.bashrc` has no
/// extension, whatever the dot suggests.
pub fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at + 1..]),
        _ => (name, ""),
    }
}

/// The piece of `text` a range asks for, counted in characters from one.
///
/// `` is all of it, `3` is the third character, `2-5` is the second to the
/// fifth, `2-` is the second onwards, and `2,3` is three characters starting
/// at the second. A range that runs past the end gives what there is, and one
/// that starts past it gives nothing - a rule applied to a mixed selection
/// should shorten the names it can, not fail on the ones it cannot.
pub fn slice(text: &str, spec: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if spec.is_empty() {
        return Some(text.to_string());
    }

    let take = |from: usize, upto: usize| -> String {
        if from >= chars.len() || upto <= from {
            String::new()
        } else {
            chars[from..upto.min(chars.len())].iter().collect()
        }
    };

    if let Some((first, count)) = spec.split_once(',') {
        let first: usize = first.parse().ok()?;
        let count: usize = count.parse().ok()?;
        let from = first.checked_sub(1)?;
        return Some(take(from, from.saturating_add(count)));
    }
    if let Some((first, last)) = spec.split_once('-') {
        let first: usize = first.parse().ok()?;
        let from = first.checked_sub(1)?;
        if last.is_empty() {
            return Some(take(from, chars.len()));
        }
        let last: usize = last.parse().ok()?;
        return Some(take(from, last));
    }
    let only: usize = spec.parse().ok()?;
    let from = only.checked_sub(1)?;
    Some(take(from, from + 1))
}

/// What one placeholder expands to, or `None` if it is not one.
fn placeholder(body: &str, source: &Source, index: usize) -> Option<String> {
    let (base, extension) = split_name(&source.name);
    let (letter, spec) = match body.chars().next() {
        Some(c) => (c, &body[c.len_utf8()..]),
        None => return None,
    };
    match letter {
        'N' => slice(base, spec),
        'E' => slice(extension, spec),
        'C' => parse_counter(spec).map(|counter| counter.at(index)),
        'Y' | 'M' | 'D' | 'h' | 'n' | 's' if spec.is_empty() => {
            // A file whose date could not be read gets no date in its name,
            // rather than the placeholder showing through as text.
            let Some(modified) = source.modified else {
                return Some(String::new());
            };
            let time: chrono::DateTime<chrono::Local> = modified.into();
            // Minutes are [n] rather than [m], because [M] is already the
            // month and a name that depends on which shift key you held is
            // not a name anyone can read back.
            let format = match letter {
                'Y' => "%Y",
                'M' => "%m",
                'D' => "%d",
                'h' => "%H",
                'n' => "%M",
                _ => "%S",
            };
            Some(time.format(format).to_string())
        }
        _ => None,
    }
}

/// Expand a template for one file.
///
/// Text outside brackets is kept as it is, and so is anything in brackets
/// that is not a placeholder - `[note]` in a name is a name, not an error.
pub fn expand(template: &str, source: &Source, index: usize) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) => {
                let body = &after[..close];
                match placeholder(body, source, index) {
                    Some(text) => out.push_str(&text),
                    None => {
                        out.push('[');
                        out.push_str(body);
                        out.push(']');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                // An unclosed bracket is a bracket.
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replace every occurrence of `find`, optionally ignoring case.
pub fn replace_all(text: &str, find: &str, replace: &str, case_sensitive: bool) -> String {
    if find.is_empty() {
        return text.to_string();
    }
    if case_sensitive {
        return text.replace(find, replace);
    }
    // Matching case-insensitively means matching on a lowered copy and then
    // cutting the original at those offsets, so the parts that are kept keep
    // the case they had.
    let hay = text.to_lowercase();
    let needle = find.to_lowercase();
    // Lowercasing can change a string's length (SS -> ss is the famous one),
    // and an offset into a copy of a different length is not an offset into
    // this one. Where that happens, take the plain path and match exactly.
    if hay.len() != text.len() {
        return text.replace(find, replace);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    while let Some(found) = hay[at..].find(&needle) {
        let from = at + found;
        out.push_str(&text[at..from]);
        out.push_str(replace);
        at = from + needle.len();
    }
    out.push_str(&text[at..]);
    out
}

/// The name the rules give the nth file of the selection.
pub fn new_name(rules: &Rules, source: &Source, index: usize) -> String {
    let base = expand(&rules.name, source, index);
    let extension = expand(&rules.extension, source, index);
    let joined = if extension.is_empty() {
        base
    } else {
        format!("{base}.{extension}")
    };
    let replaced = replace_all(&joined, &rules.find, &rules.replace, rules.case_sensitive);
    rules.case.apply(&replaced)
}

/// Why a new name cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trouble {
    /// The rules produced nothing at all.
    Empty,
    /// Not a name the filesystem will take.
    Illegal,
    /// Another file in the selection wants this same name.
    Duplicate,
    /// Something already there, which is not one of the files being renamed.
    Exists,
}

impl Trouble {
    pub fn message(&self) -> &'static str {
        match self {
            Trouble::Empty => "no name",
            Trouble::Illegal => "not a usable name",
            Trouble::Duplicate => "two files want this name",
            Trouble::Exists => "already exists",
        }
    }
}

/// Whether a platform will accept `name` as a file name.
pub fn is_legal(platform: Platform, name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.chars().any(|c| ALWAYS_ILLEGAL.contains(&c)) {
        return false;
    }
    if platform != Platform::Windows {
        return true;
    }
    if name
        .chars()
        .any(|c| WINDOWS_ILLEGAL.contains(&c) || (c as u32) < 0x20)
    {
        return false;
    }
    // A trailing dot or space is accepted and then quietly dropped, which
    // means the file is not the one you asked for.
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = split_name(name).0.to_uppercase();
    !WINDOWS_RESERVED.contains(&stem.as_str())
}

/// One line of the preview: where a file is, and where it would go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub from: PathBuf,
    pub to: PathBuf,
    /// The old name, for the left column.
    pub was: String,
    /// The new name, for the right one.
    pub name: String,
    pub trouble: Option<Trouble>,
}

impl Change {
    /// Whether this one would actually move anything.
    pub fn is_rename(&self) -> bool {
        self.trouble.is_none() && self.name != self.was
    }
}

/// Work out what the rules would do, without doing any of it.
///
/// `exists` is injected so the tests can describe a directory rather than
/// build one; the caller passes [`crate::preview::on_disk`].
pub fn plan(
    platform: Platform,
    sources: &[Source],
    rules: &Rules,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Change> {
    let mut changes: Vec<Change> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let name = new_name(rules, source, index);
            let to = match source.path.parent() {
                Some(parent) => parent.join(&name),
                None => PathBuf::from(&name),
            };
            let trouble = if name.is_empty() {
                Some(Trouble::Empty)
            } else if !is_legal(platform, &name) {
                Some(Trouble::Illegal)
            } else {
                None
            };
            Change {
                from: source.path.clone(),
                to,
                was: source.name.clone(),
                name,
                trouble,
            }
        })
        .collect();

    // Two files asking for one name is the mistake this tool makes easiest -
    // a template with no counter in it, over a selection - so it is worth
    // finding before anything moves rather than after half of it has.
    let mut wanted: HashMap<PathBuf, usize> = HashMap::new();
    for change in changes.iter().filter(|c| c.trouble.is_none()) {
        *wanted.entry(change.to.clone()).or_default() += 1;
    }
    let ours: Vec<PathBuf> = changes.iter().map(|c| c.from.clone()).collect();
    for change in changes.iter_mut() {
        if change.trouble.is_some() || change.name == change.was {
            continue;
        }
        if wanted.get(&change.to).copied().unwrap_or(0) > 1 {
            change.trouble = Some(Trouble::Duplicate);
        } else if !ours.contains(&change.to) && exists(&change.to) {
            // A name freed by another file in the selection is fair game;
            // one belonging to a file that is staying put is not.
            change.trouble = Some(Trouble::Exists);
        }
    }
    changes
}

/// How many of a plan would move, and how many are in trouble.
pub fn tally(changes: &[Change]) -> (usize, usize) {
    let moving = changes.iter().filter(|c| c.is_rename()).count();
    let troubled = changes.iter().filter(|c| c.trouble.is_some()).count();
    (moving, troubled)
}

/// One filesystem move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Which change in the plan this belongs to.
    pub change: usize,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Whether this move is only getting a name out of the way, and a later
    /// step puts the file where it belongs.
    pub staged: bool,
}

/// The moves a plan needs, in an order where none of them overwrites a file
/// that has not moved yet.
///
/// Renaming `a` to `b` while `b` becomes `c` only works one way round, and
/// swapping two names does not work in any order at all - which is what the
/// temporary is for. `temp` is given the path in the way and returns a free
/// name beside it; injected so the tests can say what it will be.
pub fn steps(changes: &[Change], temp: &dyn Fn(&Path) -> PathBuf) -> Vec<Step> {
    let moving: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].is_rename())
        .collect();
    // Who is sitting on each name. Targets are unique here - a plan with two
    // files wanting one name has them both marked as trouble and neither of
    // them moving - so every file is waited on by at most one other, and the
    // graph is chains and rings, nothing more tangled.
    let occupant: HashMap<&Path, usize> = moving
        .iter()
        .map(|&i| (changes[i].from.as_path(), i))
        .collect();

    let mut out = Vec::new();
    let mut scheduled: HashMap<usize, bool> = moving.iter().map(|&i| (i, false)).collect();
    for &start in &moving {
        if scheduled[&start] {
            continue;
        }
        // Walk from here to whoever has to move first, which is whoever is
        // standing on the name we want, and so on.
        let mut chain = Vec::new();
        let mut at = start;
        let mut ring = false;
        loop {
            chain.push(at);
            scheduled.insert(at, true);
            match occupant.get(changes[at].to.as_path()) {
                Some(&next) if !scheduled[&next] => at = next,
                Some(&next) if next == start => {
                    ring = true;
                    break;
                }
                _ => break,
            }
        }

        if ring {
            // Nothing in a ring can move first, so one of them steps aside.
            let first = chain[0];
            let aside = temp(&changes[first].from);
            out.push(Step {
                change: first,
                from: changes[first].from.clone(),
                to: aside.clone(),
                staged: true,
            });
            for &i in chain[1..].iter().rev() {
                out.push(Step {
                    change: i,
                    from: changes[i].from.clone(),
                    to: changes[i].to.clone(),
                    staged: false,
                });
            }
            out.push(Step {
                change: first,
                from: aside,
                to: changes[first].to.clone(),
                staged: false,
            });
        } else {
            for &i in chain.iter().rev() {
                out.push(Step {
                    change: i,
                    from: changes[i].from.clone(),
                    to: changes[i].to.clone(),
                    staged: false,
                });
            }
        }
    }
    out
}

/// A file the rename could not move, and what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub name: String,
    pub message: String,
}

/// What a run of [`apply`] came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub renamed: usize,
    pub failures: Vec<Failure>,
}

/// A free name beside `path`, for a file that has to step aside.
fn aside(path: &Path) -> PathBuf {
    let mut n = 0u32;
    loop {
        let candidate = path.with_file_name(format!(".lostc-rename-{n}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Carry out a plan.
///
/// A file that will not move does not stop the others - a selection is not a
/// transaction, and stopping halfway leaves a worse mess than carrying on and
/// saying which ones did not make it.
pub fn apply(changes: &[Change]) -> Applied {
    let mut applied = Applied::default();
    let mut stuck: Vec<usize> = Vec::new();
    for step in steps(changes, &aside) {
        if stuck.contains(&step.change) {
            continue;
        }
        match std::fs::rename(&step.from, &step.to) {
            Ok(()) => {
                if !step.staged {
                    applied.renamed += 1;
                }
            }
            Err(error) => {
                stuck.push(step.change);
                let change = &changes[step.change];
                let mut message = error.to_string();
                if step.from != change.from {
                    // Half-done, and the file is not where its name says.
                    // Say where it actually is, so it can be found.
                    message = format!("{message} (left as {})", step.from.display());
                }
                applied.failures.push(Failure {
                    name: change.was.clone(),
                    message,
                });
            }
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn source(name: &str) -> Source {
        Source::new(format!("/files/{name}"), name)
    }

    fn none(_: &Path) -> bool {
        false
    }

    fn rules(name: &str, extension: &str) -> Rules {
        Rules {
            name: name.to_string(),
            extension: extension.to_string(),
            ..Rules::default()
        }
    }

    #[test]
    fn the_default_rules_change_nothing() {
        let rules = Rules::default();
        assert!(rules.is_identity());
        for name in ["photo.JPG", "README", ".bashrc", "archive.tar.gz"] {
            assert_eq!(new_name(&rules, &source(name), 0), name);
        }
    }

    #[test]
    fn split_name_cuts_at_the_last_dot() {
        assert_eq!(split_name("photo.jpg"), ("photo", "jpg"));
        assert_eq!(split_name("archive.tar.gz"), ("archive.tar", "gz"));
        assert_eq!(split_name("README"), ("README", ""));
        assert_eq!(split_name(".bashrc"), (".bashrc", ""));
        assert_eq!(split_name("trailing."), ("trailing", ""));
    }

    #[test]
    fn slice_counts_characters_from_one() {
        assert_eq!(slice("holiday", "").unwrap(), "holiday");
        assert_eq!(slice("holiday", "1").unwrap(), "h");
        assert_eq!(slice("holiday", "1-4").unwrap(), "holi");
        assert_eq!(slice("holiday", "3-").unwrap(), "liday");
        assert_eq!(slice("holiday", "2,3").unwrap(), "oli");
        // Past the end gives what there is, and nothing beyond it.
        assert_eq!(slice("holiday", "5-99").unwrap(), "day");
        assert_eq!(slice("holiday", "20-30").unwrap(), "");
        // Characters, not bytes.
        assert_eq!(slice("créme", "1-3").unwrap(), "cré");
        // Not a range at all.
        assert!(slice("holiday", "x").is_none());
        assert!(slice("holiday", "0").is_none());
    }

    #[test]
    fn counters_start_step_and_pad() {
        let plain = parse_counter("").unwrap();
        assert_eq!(plain, Counter::default());
        assert_eq!(plain.at(0), "1");
        assert_eq!(plain.at(9), "10");

        let padded = parse_counter("001").unwrap();
        assert_eq!(padded.at(0), "001");
        assert_eq!(padded.at(9), "010");
        assert_eq!(padded.at(999), "1000");

        let from_ten = parse_counter("10").unwrap();
        assert_eq!(from_ten.at(0), "10");
        assert_eq!(from_ten.width, 0);

        let stepped = parse_counter("1+2").unwrap();
        assert_eq!(stepped.at(0), "1");
        assert_eq!(stepped.at(3), "7");

        let backwards = parse_counter("10+-1").unwrap();
        assert_eq!(backwards.at(2), "8");

        assert_eq!(parse_counter("+5").unwrap().at(1), "6");
        assert!(parse_counter("x").is_none());
    }

    #[test]
    fn templates_keep_text_that_is_not_a_placeholder() {
        let s = source("photo.jpg");
        assert_eq!(expand("holiday-[N]", &s, 0), "holiday-photo");
        assert_eq!(expand("[N] (copy)", &s, 0), "photo (copy)");
        // Brackets around something that means nothing here are just text.
        assert_eq!(expand("[note] [N]", &s, 0), "[note] photo");
        assert_eq!(expand("[N", &s, 0), "[N");
        assert_eq!(expand("", &s, 0), "");
    }

    #[test]
    fn a_camera_dump_becomes_a_numbered_set() {
        let files = ["DSC00417.JPG", "DSC00418.JPG", "IMG_2231.jpg"];
        let rules = rules("holiday_[C001]", "[E]");
        let named: Vec<String> = files
            .iter()
            .enumerate()
            .map(|(i, name)| new_name(&rules, &source(name), i))
            .collect();
        assert_eq!(
            named,
            [
                "holiday_001.JPG".to_string(),
                "holiday_002.JPG".to_string(),
                "holiday_003.jpg".to_string()
            ]
        );
    }

    #[test]
    fn dates_come_from_the_file_and_minutes_are_n() {
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut s = source("note.txt");
        s.modified = Some(when);

        let stamped = new_name(&rules("[Y]-[M]-[D] [N]", "[E]"), &s, 0);
        let time: chrono::DateTime<chrono::Local> = when.into();
        assert_eq!(
            stamped,
            format!("{} note.txt", time.format("%Y-%m-%d")),
            "the date placeholders read the file's own modification time"
        );

        // [M] is the month and [n] is the minute, so neither depends on which
        // shift key was held.
        assert_eq!(
            expand("[M]", &s, 0),
            time.format("%m").to_string(),
            "capital M is the month"
        );
        assert_eq!(
            expand("[n]", &s, 0),
            time.format("%M").to_string(),
            "small n is the minute"
        );

        // A file with no date left gets no date in its name, rather than a
        // made-up one.
        s.modified = None;
        assert_eq!(new_name(&rules("[Y][N]", "[E]"), &s, 0), "note.txt");
    }

    #[test]
    fn an_empty_extension_template_takes_the_dot_with_it() {
        assert_eq!(
            new_name(&rules("[N]", ""), &source("photo.jpg"), 0),
            "photo"
        );
        // And a file with no extension does not gain a trailing dot.
        assert_eq!(
            new_name(&Rules::default(), &source("README"), 0),
            "README",
            "[E] on a file without one leaves no dot behind"
        );
    }

    #[test]
    fn search_and_replace_can_ignore_case_without_losing_it() {
        assert_eq!(replace_all("a-b-a", "a", "z", true), "z-b-z");
        assert_eq!(
            replace_all("Photo Copy.jpg", " Copy", "", false),
            "Photo.jpg"
        );
        assert_eq!(
            replace_all("HOLIDAY holiday", "holiday", "trip", false),
            "trip trip"
        );
        // The parts that are kept keep the case they had.
        assert_eq!(
            replace_all("MyFileName", "file", "Folder", false),
            "MyFolderName"
        );
        assert_eq!(replace_all("nothing", "", "x", false), "nothing");
    }

    #[test]
    fn case_conversion_covers_the_usual_four() {
        assert_eq!(Case::Keep.apply("My Photo.JPG"), "My Photo.JPG");
        assert_eq!(Case::Lower.apply("My Photo.JPG"), "my photo.jpg");
        assert_eq!(Case::Upper.apply("My Photo.JPG"), "MY PHOTO.JPG");
        assert_eq!(
            Case::Title.apply("my_holiday-photo.jpg"),
            "My_Holiday-Photo.Jpg"
        );
        assert_eq!(Case::First.apply("MY HOLIDAY.JPG"), "My holiday.jpg");
        // The field cycles both ways and comes back to where it started.
        let mut case = Case::Keep;
        for _ in 0..Case::ALL.len() {
            case = case.next();
        }
        assert_eq!(case, Case::Keep);
        assert_eq!(Case::Keep.prev(), Case::First);
    }

    #[test]
    fn plan_puts_the_new_names_beside_the_old_ones() {
        let sources = vec![source("b.txt"), source("a.txt")];
        let changes = plan(Platform::Linux, &sources, &rules("[C]", "[E]"), &none);
        assert_eq!(changes[0].to, PathBuf::from("/files/1.txt"));
        assert_eq!(changes[1].to, PathBuf::from("/files/2.txt"));
        assert_eq!(changes[0].was, "b.txt");
        assert_eq!(changes[0].name, "1.txt");
        assert!(changes.iter().all(|c| c.trouble.is_none()));
        assert_eq!(tally(&changes), (2, 0));
    }

    #[test]
    fn a_file_whose_name_does_not_change_is_not_a_rename() {
        let changes = plan(
            Platform::Linux,
            &[source("photo.jpg")],
            &Rules::default(),
            &none,
        );
        assert_eq!(
            changes[0].trouble, None,
            "leaving a name alone is not an error"
        );
        assert!(!changes[0].is_rename());
        assert_eq!(tally(&changes), (0, 0));
        assert!(steps(&changes, &|p| p.to_path_buf()).is_empty());
    }

    #[test]
    fn two_files_wanting_one_name_are_both_flagged() {
        let sources = vec![source("a.txt"), source("b.txt")];
        let changes = plan(Platform::Linux, &sources, &rules("same", "[E]"), &none);
        assert_eq!(changes[0].trouble, Some(Trouble::Duplicate));
        assert_eq!(changes[1].trouble, Some(Trouble::Duplicate));
        assert_eq!(tally(&changes), (0, 2));
        assert!(
            steps(&changes, &|p| p.to_path_buf()).is_empty(),
            "nothing moves while the plan is in trouble"
        );
    }

    #[test]
    fn an_existing_file_is_in_the_way_unless_it_is_moving_too() {
        let taken: HashSet<PathBuf> = ["/files/taken.txt", "/files/b.txt"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let exists = |p: &Path| taken.contains(p);

        // Something else already has the name.
        let changes = plan(
            Platform::Linux,
            &[source("a.txt")],
            &rules("taken", "[E]"),
            &exists,
        );
        assert_eq!(changes[0].trouble, Some(Trouble::Exists));

        // But b.txt is in the selection and about to move, so its name is
        // free for a.txt to take.
        let sources = vec![source("a.txt"), source("b.txt")];
        let changes = plan(Platform::Linux, &sources, &rules("[C]", "[E]"), &exists);
        let renamed = plan(Platform::Linux, &sources, &rules("x[N]", "[E]"), &exists);
        assert!(changes.iter().all(|c| c.trouble.is_none()));
        assert!(renamed.iter().all(|c| c.trouble.is_none()));
    }

    #[test]
    fn empty_and_illegal_names_are_refused() {
        let changes = plan(Platform::Linux, &[source("a.txt")], &rules("", ""), &none);
        assert_eq!(changes[0].trouble, Some(Trouble::Empty));

        let changes = plan(
            Platform::Linux,
            &[source("a.txt")],
            &rules("dir/name", "[E]"),
            &none,
        );
        assert_eq!(
            changes[0].trouble,
            Some(Trouble::Illegal),
            "a rename stays in its directory"
        );
    }

    #[test]
    fn windows_refuses_a_good_deal_more_than_unix_does() {
        for name in ["what?.txt", "a:b", "trailing .", "CON.txt", "nul"] {
            assert!(
                is_legal(Platform::Linux, name),
                "{name} is a perfectly good name on Unix"
            );
            assert!(
                !is_legal(Platform::Windows, name),
                "{name} is not one Windows will take"
            );
        }
        for name in ["photo.jpg", "my file.txt", ".bashrc", "console.log"] {
            assert!(is_legal(Platform::Windows, name), "{name} is fine");
        }
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            assert!(!is_legal(platform, ""));
            assert!(!is_legal(platform, "."));
            assert!(!is_legal(platform, ".."));
            assert!(!is_legal(platform, "a/b"));
        }
    }

    /// Renames a plan would need, as `from -> to`, in order.
    fn moves(changes: &[Change]) -> Vec<String> {
        let temp = |p: &Path| p.with_file_name("TMP");
        steps(changes, &temp)
            .iter()
            .map(|s| {
                format!(
                    "{} -> {}",
                    s.from.file_name().unwrap().to_string_lossy(),
                    s.to.file_name().unwrap().to_string_lossy()
                )
            })
            .collect()
    }

    #[test]
    fn a_chain_of_renames_runs_from_the_far_end() {
        // a -> b while b -> c: b has to move out of a's way first.
        let changes = vec![
            Change {
                from: "/files/a".into(),
                to: "/files/b".into(),
                was: "a".into(),
                name: "b".into(),
                trouble: None,
            },
            Change {
                from: "/files/b".into(),
                to: "/files/c".into(),
                was: "b".into(),
                name: "c".into(),
                trouble: None,
            },
        ];
        assert_eq!(moves(&changes), ["b -> c", "a -> b"]);
    }

    #[test]
    fn swapping_two_names_goes_through_a_temporary() {
        let changes = vec![
            Change {
                from: "/files/a".into(),
                to: "/files/b".into(),
                was: "a".into(),
                name: "b".into(),
                trouble: None,
            },
            Change {
                from: "/files/b".into(),
                to: "/files/a".into(),
                was: "b".into(),
                name: "a".into(),
                trouble: None,
            },
        ];
        assert_eq!(moves(&changes), ["a -> TMP", "b -> a", "TMP -> b"]);
        let steps = steps(&changes, &|p| p.with_file_name("TMP"));
        assert!(
            steps[0].staged,
            "the first move is only getting out of the way"
        );
        assert!(!steps[2].staged);
    }

    #[test]
    fn a_ring_of_three_needs_exactly_one_temporary() {
        // a -> b -> c -> a, the shift-everything-along case.
        let changes = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let next = ["b", "c", "a"][i];
                Change {
                    from: format!("/files/{name}").into(),
                    to: format!("/files/{next}").into(),
                    was: name.to_string(),
                    name: next.to_string(),
                    trouble: None,
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            moves(&changes),
            ["a -> TMP", "c -> a", "b -> c", "TMP -> b"]
        );
    }

    #[test]
    fn unrelated_renames_keep_their_order() {
        let changes = ["one", "two", "three"]
            .iter()
            .map(|name| Change {
                from: format!("/files/{name}").into(),
                to: format!("/files/{name}.bak").into(),
                was: name.to_string(),
                name: format!("{name}.bak"),
                trouble: None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            moves(&changes),
            ["one -> one.bak", "two -> two.bak", "three -> three.bak"]
        );
    }

    #[test]
    fn every_step_lands_somewhere_free() {
        // Whatever the shape, walking the steps in order must never write
        // over a name that is still occupied.
        let shapes: Vec<Vec<(&str, &str)>> = vec![
            vec![("a", "b"), ("b", "c"), ("c", "d")],
            vec![("a", "b"), ("b", "a")],
            vec![("a", "b"), ("b", "c"), ("c", "a")],
            vec![("c", "a"), ("a", "b"), ("b", "c")],
            vec![("a", "b"), ("c", "d"), ("d", "e")],
        ];
        for shape in shapes {
            let changes: Vec<Change> = shape
                .iter()
                .map(|(from, to)| Change {
                    from: format!("/files/{from}").into(),
                    to: format!("/files/{to}").into(),
                    was: from.to_string(),
                    name: to.to_string(),
                    trouble: None,
                })
                .collect();
            let mut present: HashSet<PathBuf> = changes.iter().map(|c| c.from.clone()).collect();
            for step in steps(&changes, &|p| p.with_file_name("TMP")) {
                assert!(
                    present.remove(&step.from),
                    "{shape:?}: moved {} which is not there",
                    step.from.display()
                );
                assert!(
                    present.insert(step.to.clone()),
                    "{shape:?}: {} was written over",
                    step.to.display()
                );
            }
            let ended: HashSet<PathBuf> = changes.iter().map(|c| c.to.clone()).collect();
            assert_eq!(present, ended, "{shape:?}: did not end where it was going");
        }
    }

    #[test]
    fn apply_renames_and_reports_what_it_could_not() {
        let dir = std::env::temp_dir().join(format!(
            "lostc-rename-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.join(name), name).unwrap();
        }

        // A swap, which is the case that needs the temporary, plus one that
        // cannot work because the file is not there.
        let mut changes: Vec<Change> = [("a", "b"), ("b", "a")]
            .iter()
            .map(|(from, to)| Change {
                from: dir.join(from),
                to: dir.join(to),
                was: from.to_string(),
                name: to.to_string(),
                trouble: None,
            })
            .collect();
        changes.push(Change {
            from: dir.join("missing"),
            to: dir.join("elsewhere"),
            was: "missing".into(),
            name: "elsewhere".into(),
            trouble: None,
        });

        let applied = apply(&changes);
        assert_eq!(applied.renamed, 2);
        assert_eq!(applied.failures.len(), 1);
        assert_eq!(applied.failures[0].name, "missing");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "b");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "a");
        assert!(
            !dir.join(".lostc-rename-0").exists(),
            "the temporary is not left behind"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
