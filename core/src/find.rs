// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Finding files by name, and by what is inside them.
//!
//! Split the same way [`crate::progress`] is, and for the same reason: the
//! walk is a synchronous function over a [`Sink`], so the tests drive it
//! directly and deterministically, and [`Search`] is the thin wrapper that
//! puts it on a thread and shares its results with a UI.
//!
//! Results arrive as they are found rather than at the end. A search over a
//! large tree takes as long as it takes, and a list that fills while it runs
//! is one you can use before it finishes - which for "where did I put that"
//! is usually after the first few hits.

use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::panel::matches_glob_with;
use crate::preview::looks_like_text;

/// How many hits are kept before a search calls it enough.
///
/// A pattern of `*` over a home directory is not a search, it is a listing;
/// the cap is what stops it becoming a memory problem instead of a result.
pub const MAX_HITS: usize = 5_000;

/// The longest line offered as an excerpt.
const MAX_EXCERPT: usize = 200;

/// How much of a file is read to decide whether it is text.
const SNIFF: usize = 4_096;

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// A glob over the file name.
    ///
    /// Empty means every name, and empty is also what the box opens with -
    /// ready to be typed into, rather than holding a `*` that has to be
    /// cleared out of the way first.
    pub pattern: String,
    /// Text that must appear inside the file. Empty means do not look.
    pub contains: String,
    pub case_sensitive: bool,
    /// Whether to descend into and report dot-files.
    pub include_hidden: bool,
}

impl Query {
    /// Whether there is anything to search for.
    ///
    /// An empty pattern means "every name", which is only a search worth
    /// running when there is also text to look for inside.
    pub fn is_empty(&self) -> bool {
        let pattern = self.pattern.trim();
        (pattern.is_empty() || pattern == "*") && self.contains.is_empty()
    }

    /// The glob to match names against, with an empty box meaning everything.
    pub fn glob(&self) -> &str {
        let pattern = self.pattern.trim();
        if pattern.is_empty() {
            "*"
        } else {
            pattern
        }
    }

    /// Whether a bare name matches, before anything is opened.
    pub fn matches_name(&self, name: &str) -> bool {
        matches_glob_with(self.glob(), name, self.case_sensitive)
    }

    /// Whether this search has to open files to answer.
    pub fn reads_files(&self) -> bool {
        !self.contains.is_empty()
    }
}

/// One file the search matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// Where in the file the text was found, for a content search.
    pub line: Option<usize>,
    /// The matching line, trimmed and capped.
    pub excerpt: Option<String>,
}

/// Where a walk reports to, and asks whether to stop.
pub trait Sink {
    /// A file matched. Returning false stops the walk - the cap reached.
    fn hit(&mut self, hit: Hit) -> bool;
    /// Somewhere the walk has got to, for the "still looking" line.
    fn looking_at(&mut self, path: &Path);
    fn cancelled(&self) -> bool;
}

/// Whether `line` contains `needle`.
pub fn line_matches(line: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        line.contains(needle)
    } else {
        line.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// The first line of `reader` containing `needle`, one-based.
///
/// Reads a line at a time rather than in fixed blocks: a match that straddled
/// a block boundary would be missed, and the line number and the excerpt are
/// what make a content hit worth showing.
pub fn scan_text(
    reader: &mut dyn BufRead,
    needle: &str,
    case_sensitive: bool,
) -> io::Result<Option<(usize, String)>> {
    let mut buffer = Vec::new();
    let mut number = 0usize;
    loop {
        buffer.clear();
        // A file with no newlines at all is one line; reading it whole would
        // be the same read either way, and the cap below keeps the excerpt
        // from becoming the file.
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        number += 1;
        let line = String::from_utf8_lossy(&buffer);
        if line_matches(&line, needle, case_sensitive) {
            let trimmed = line.trim_end_matches(['\n', '\r']).trim();
            let excerpt: String = trimmed.chars().take(MAX_EXCERPT).collect();
            return Ok(Some((number, excerpt)));
        }
    }
}

/// Look inside one file. `None` when it does not match, or is not text.
pub fn search_file(path: &Path, query: &Query) -> io::Result<Option<(usize, String)>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    // Binaries are skipped rather than searched: a match inside one is noise
    // nine times out of ten, and printing an excerpt of it is worse.
    let head = reader.fill_buf()?;
    let head = &head[..head.len().min(SNIFF)];
    if !looks_like_text(head) {
        return Ok(None);
    }

    scan_text(&mut reader, &query.contains, query.case_sensitive)
}

/// Whether a name is hidden, in the sense the panels use.
fn is_hidden(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

/// Walk `root`, reporting everything that matches.
///
/// Symbolic links to directories are not followed. A link pointing at an
/// ancestor makes the walk infinite, and there is no bookkeeping cheap enough
/// to make following them safe that is also worth it here - the file is still
/// reachable by its real path, so nothing is lost but a duplicate.
pub fn walk(root: &Path, query: &Query, sink: &mut dyn Sink) {
    if sink.cancelled() {
        return;
    }
    sink.looking_at(root);

    let Ok(entries) = std::fs::read_dir(root) else {
        // An unreadable directory is not an error worth stopping for: a
        // search over a home directory meets several.
        return;
    };

    for entry in entries.flatten() {
        if sink.cancelled() {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !query.include_hidden && is_hidden(&name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let is_link = metadata.file_type().is_symlink();

        if metadata.is_dir() && !is_link {
            // A directory can match by name too - "where is that folder
            // called build" is the same question.
            if !query.reads_files() && query.matches_name(&name) && !hit(sink, &path, None) {
                return;
            }
            walk(&path, query, sink);
            continue;
        }

        if !query.matches_name(&name) {
            continue;
        }
        if !query.reads_files() {
            if !hit(sink, &path, None) {
                return;
            }
            continue;
        }
        // Name matched and there is text to look for, so now it is worth
        // opening. Doing it in this order is what keeps a content search over
        // a big tree bearable: `*.rs` with text opens the Rust files only.
        if let Ok(Some(found)) = search_file(&path, query) {
            if !hit(sink, &path, Some(found)) {
                return;
            }
        }
    }
}

fn hit(sink: &mut dyn Sink, path: &Path, found: Option<(usize, String)>) -> bool {
    let (line, excerpt) = match found {
        Some((line, excerpt)) => (Some(line), Some(excerpt)),
        None => (None, None),
    };
    sink.hit(Hit {
        path: path.to_path_buf(),
        line,
        excerpt,
    })
}

/// What a running search has turned up so far.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// Where it has got to, for the line that says it is still going.
    pub current: String,
    pub finished: bool,
    pub cancelled: bool,
    /// It stopped at [`MAX_HITS`] rather than because it ran out of tree.
    pub truncated: bool,
}

struct SharedSink {
    found: Arc<Mutex<Found>>,
    cancel: Arc<AtomicBool>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sink for SharedSink {
    fn hit(&mut self, hit: Hit) -> bool {
        let mut guard = lock(&self.found);
        guard.hits.push(hit);
        if guard.hits.len() >= MAX_HITS {
            guard.truncated = true;
            return false;
        }
        true
    }

    fn looking_at(&mut self, path: &Path) {
        lock(&self.found).current = path.display().to_string();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A search running on a worker thread.
pub struct Search {
    found: Arc<Mutex<Found>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub root: PathBuf,
    pub query: Query,
}

impl Search {
    pub fn spawn(root: PathBuf, query: Query) -> Search {
        let found: Arc<Mutex<Found>> = Arc::default();
        let cancel = Arc::new(AtomicBool::new(false));

        let worker_found = Arc::clone(&found);
        let worker_cancel = Arc::clone(&cancel);
        let worker_root = root.clone();
        let worker_query = query.clone();

        let handle = std::thread::spawn(move || {
            let mut sink = SharedSink {
                found: Arc::clone(&worker_found),
                cancel: Arc::clone(&worker_cancel),
            };
            walk(&worker_root, &worker_query, &mut sink);

            let mut guard = lock(&worker_found);
            guard.cancelled = worker_cancel.load(Ordering::Relaxed);
            guard.finished = true;
            guard.current.clear();
        });

        Search {
            found,
            cancel,
            handle: Some(handle),
            root,
            query,
        }
    }

    pub fn snapshot(&self) -> Found {
        lock(&self.found).clone()
    }

    /// Just the count, which is all a redraw needs most frames.
    pub fn count(&self) -> usize {
        lock(&self.found).hits.len()
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.found).finished
    }

    pub fn request_stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Search {
    /// A window closed mid-search must not leave a thread walking a disk.
    fn drop(&mut self) {
        self.request_stop();
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestSink {
        hits: Vec<Hit>,
        visited: Vec<PathBuf>,
        stop_after: Option<usize>,
    }

    impl Sink for TestSink {
        fn hit(&mut self, hit: Hit) -> bool {
            self.hits.push(hit);
            match self.stop_after {
                Some(limit) => self.hits.len() < limit,
                None => true,
            }
        }
        fn looking_at(&mut self, path: &Path) {
            self.visited.push(path.to_path_buf());
        }
        fn cancelled(&self) -> bool {
            false
        }
    }

    impl TestSink {
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = self
                .hits
                .iter()
                .map(|h| h.path.file_name().unwrap().to_string_lossy().to_string())
                .collect();
            names.sort();
            names
        }
    }

    /// A small tree with something to find at every depth.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("README.md"), "the readme\nwith a needle in it\n").unwrap();
        std::fs::write(root.join("notes.txt"), "nothing here\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { needle(); }\n").unwrap();
        std::fs::write(root.join("src/deep/util.rs"), "// no match\n").unwrap();
        std::fs::write(root.join(".hidden.txt"), "a needle, hidden\n").unwrap();
        std::fs::write(root.join(".git/config"), "needle\n").unwrap();
        dir
    }

    fn run(root: &Path, query: Query) -> TestSink {
        let mut sink = TestSink::default();
        walk(root, &query, &mut sink);
        sink
    }

    #[test]
    fn a_name_pattern_finds_files_at_every_depth() {
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "*.rs".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["main.rs", "util.rs"]);
        // No content was asked for, so nothing carries a line.
        assert!(sink.hits.iter().all(|h| h.line.is_none()));
    }

    #[test]
    fn the_pattern_is_forgiving_about_case_unless_told_otherwise() {
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "readme*".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["README.md"]);

        let sink = run(
            dir.path(),
            Query {
                pattern: "readme*".into(),
                case_sensitive: true,
                ..Query::default()
            },
        );
        assert!(sink.names().is_empty(), "{:?}", sink.names());
    }

    #[test]
    fn hidden_files_are_left_out_until_they_are_asked_for() {
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "*.txt".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["notes.txt"]);

        let sink = run(
            dir.path(),
            Query {
                pattern: "*.txt".into(),
                include_hidden: true,
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec![".hidden.txt", "notes.txt"]);
    }

    #[test]
    fn a_hidden_directory_is_not_walked_either() {
        // .git is the case that matters: searching a repository without this
        // returns ten times as much as it should.
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "config".into(),
                ..Query::default()
            },
        );
        assert!(sink.names().is_empty(), "{:?}", sink.names());
        assert!(
            !sink.visited.iter().any(|p| p.ends_with(".git")),
            "it walked into .git"
        );
    }

    #[test]
    fn searching_the_contents_reports_where_it_matched() {
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "*".into(),
                contains: "needle".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["README.md", "main.rs"]);

        let readme = sink
            .hits
            .iter()
            .find(|h| h.path.ends_with("README.md"))
            .unwrap();
        assert_eq!(readme.line, Some(2));
        assert_eq!(readme.excerpt.as_deref(), Some("with a needle in it"));
    }

    #[test]
    fn the_name_narrows_what_is_opened() {
        // This is what keeps a content search over a big tree bearable: the
        // name is checked first, and only what passes gets read.
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "*.rs".into(),
                contains: "needle".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["main.rs"]);
    }

    #[test]
    fn a_directory_matches_by_name_but_never_by_content() {
        let dir = tree();
        let sink = run(
            dir.path(),
            Query {
                pattern: "src".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["src"]);

        // With text to find, a directory cannot be the answer.
        let sink = run(
            dir.path(),
            Query {
                pattern: "src".into(),
                contains: "needle".into(),
                ..Query::default()
            },
        );
        assert!(sink.names().is_empty(), "{:?}", sink.names());
    }

    #[test]
    fn a_binary_is_skipped_rather_than_searched() {
        let dir = tempfile::tempdir().unwrap();
        // The needle really is in there, as bytes.
        std::fs::write(dir.path().join("prog.bin"), b"\x7fELF\0\0needle\0\0").unwrap();
        std::fs::write(dir.path().join("prog.txt"), "needle\n").unwrap();

        let sink = run(
            dir.path(),
            Query {
                pattern: "*".into(),
                contains: "needle".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["prog.txt"]);
    }

    #[test]
    fn a_match_is_found_however_long_the_line_before_it() {
        // Reading in fixed blocks would drop a match straddling a boundary.
        let dir = tempfile::tempdir().unwrap();
        let mut text = "x".repeat(200_000);
        text.push('\n');
        text.push_str("needle\n");
        std::fs::write(dir.path().join("long.txt"), &text).unwrap();

        let sink = run(
            dir.path(),
            Query {
                pattern: "*".into(),
                contains: "needle".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.hits.len(), 1);
        assert_eq!(sink.hits[0].line, Some(2));
    }

    #[test]
    fn an_excerpt_is_capped_rather_than_being_the_whole_line() {
        let mut line = "needle".to_string();
        line.push_str(&"y".repeat(10_000));
        let found = scan_text(&mut line.as_bytes(), "needle", false)
            .unwrap()
            .unwrap();
        assert_eq!(found.0, 1);
        assert_eq!(found.1.chars().count(), MAX_EXCERPT);
    }

    #[test]
    fn the_text_search_can_be_told_to_mind_the_case() {
        assert!(line_matches("The Needle", "needle", false));
        assert!(!line_matches("The Needle", "needle", true));
        assert!(line_matches("The Needle", "Needle", true));
    }

    #[test]
    fn scanning_reports_the_first_line_that_matches() {
        let text = "one\ntwo\nneedle here\nneedle again\n";
        let found = scan_text(&mut text.as_bytes(), "needle", false)
            .unwrap()
            .unwrap();
        assert_eq!(found, (3, "needle here".to_string()));

        assert!(scan_text(&mut text.as_bytes(), "haystack", false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_walk_stops_when_the_sink_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..20 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), "x").unwrap();
        }
        let mut sink = TestSink {
            stop_after: Some(5),
            ..TestSink::default()
        };
        walk(dir.path(), &Query::default(), &mut sink);
        assert_eq!(sink.hits.len(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_does_not_hang_the_walk() {
        // A link pointing at an ancestor is the classic way to make a
        // recursive walk never return.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/found.txt"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/loop")).unwrap();

        let sink = run(
            dir.path(),
            Query {
                pattern: "found.txt".into(),
                ..Query::default()
            },
        );
        assert_eq!(sink.names(), vec!["found.txt"], "walked the link");
    }

    #[test]
    fn an_unreadable_directory_does_not_stop_the_search() {
        let dir = tree();
        // A path that is not a directory at all stands in for one that
        // cannot be read: either way the walk carries on.
        let mut sink = TestSink::default();
        walk(&dir.path().join("README.md"), &Query::default(), &mut sink);
        assert!(sink.hits.is_empty());
    }

    #[test]
    fn an_empty_query_is_not_worth_running() {
        assert!(Query::default().is_empty());
        assert!(Query {
            pattern: "  ".into(),
            ..Query::default()
        }
        .is_empty());
        // A bare `*` is only a search when there is text to go with it.
        assert!(!Query {
            pattern: "*".into(),
            contains: "needle".into(),
            ..Query::default()
        }
        .is_empty());
        assert!(!Query {
            pattern: "*.rs".into(),
            ..Query::default()
        }
        .is_empty());
    }

    #[test]
    fn an_empty_pattern_means_every_name() {
        let query = Query {
            pattern: String::new(),
            contains: "needle".into(),
            ..Query::default()
        };
        assert_eq!(query.glob(), "*");
        assert!(query.matches_name("anything.at.all"));
    }

    #[test]
    fn a_search_runs_on_a_thread_and_reports_when_it_is_done() {
        let dir = tree();
        let mut search = Search::spawn(
            dir.path().to_path_buf(),
            Query {
                pattern: "*.rs".into(),
                ..Query::default()
            },
        );
        search.join();

        let found = search.snapshot();
        assert!(found.finished);
        assert!(!found.cancelled);
        assert!(!found.truncated);
        assert_eq!(found.hits.len(), 2);
        assert!(found.current.is_empty(), "still says it is looking");
    }

    #[test]
    fn stopping_a_search_says_it_was_stopped() {
        let dir = tree();
        let mut search = Search::spawn(dir.path().to_path_buf(), Query::default());
        search.request_stop();
        search.join();
        assert!(search.snapshot().finished);
    }
}
