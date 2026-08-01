//! Reading a text file by line number without holding it in memory.
//!
//! A viewer that caps at a quarter of a megabyte is a viewer that cannot open
//! the log you actually wanted to look at. The way out is not a bigger cap: it
//! is to stop keeping the file at all, and instead keep a note of where the
//! lines *are*, so any window of them can be fetched on demand.
//!
//! The note is sparse. Recording every line's offset would cost 8 bytes a
//! line, 80 MB for a ten-million-line log, so only every [`STRIDE`]th offset
//! is kept and the reader walks forward from the nearest one. That is 40 000
//! anchors, about 320 KB, for the same file.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How many lines apart the recorded offsets are.
pub const STRIDE: usize = 256;

/// The most of a single line that is kept. A minified bundle or a base64 blob
/// can be one line of many megabytes, and nobody is reading that anyway.
pub const MAX_LINE: usize = 4096;

/// How far the scan will go before giving up. Generous enough that it will not
/// be met in practice, low enough that a runaway file cannot hang the worker.
pub const MAX_SCAN: u64 = 4 << 30;

/// Where a file's lines are, without the file itself.
#[derive(Debug, Clone)]
pub struct LineIndex {
    path: PathBuf,
    /// Byte offset of the start of line `i * STRIDE`.
    anchors: Vec<u64>,
    lines: usize,
    bytes: u64,
    /// Set when the scan stopped at [`MAX_SCAN`] rather than at the end.
    partial: bool,
}

impl LineIndex {
    /// Walk the file once, noting where the lines begin.
    ///
    /// This reads the whole file but keeps almost none of it, so the cost is
    /// the read - which is why it happens on a worker thread.
    pub fn build(path: &Path) -> io::Result<LineIndex> {
        let file = File::open(path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut reader = BufReader::with_capacity(64 * 1024, file);

        let mut anchors = vec![0u64];
        let mut lines = 0usize;
        let mut position = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        let mut partial = false;
        // A file whose last byte is not a newline still ends in a line.
        let mut trailing = false;

        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            for (index, &byte) in buffer[..read].iter().enumerate() {
                if byte == b'\n' {
                    lines += 1;
                    if lines % STRIDE == 0 {
                        anchors.push(position + index as u64 + 1);
                    }
                    trailing = false;
                } else {
                    trailing = true;
                }
            }
            position += read as u64;
            if position >= MAX_SCAN {
                partial = true;
                break;
            }
        }
        if trailing {
            lines += 1;
        }

        Ok(LineIndex {
            path: path.to_path_buf(),
            anchors,
            lines,
            bytes,
            partial,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many lines the file has.
    pub fn lines(&self) -> usize {
        self.lines
    }

    /// The file's size on disk.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// True when the file was too big to index all of.
    pub fn partial(&self) -> bool {
        self.partial
    }

    /// How much memory the index itself costs, for the panel to admit to.
    pub fn index_bytes(&self) -> usize {
        self.anchors.len() * std::mem::size_of::<u64>()
    }

    /// Read `count` lines starting at `start`, counting from zero.
    ///
    /// Seeks to the nearest anchor at or before `start` and walks forward, so
    /// the work is bounded by [`STRIDE`] however far into the file it is.
    pub fn read(&self, start: usize, count: usize) -> io::Result<Vec<String>> {
        if start >= self.lines || count == 0 {
            return Ok(Vec::new());
        }
        let anchor = (start / STRIDE).min(self.anchors.len().saturating_sub(1));
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.anchors[anchor]))?;
        let mut reader = BufReader::with_capacity(64 * 1024, file);

        // Walk from the anchor to the line actually wanted.
        let mut scratch = Vec::new();
        for _ in 0..(start - anchor * STRIDE) {
            scratch.clear();
            if reader.read_until(b'\n', &mut scratch)? == 0 {
                return Ok(Vec::new());
            }
        }

        let wanted = count.min(self.lines - start);
        let mut out = Vec::with_capacity(wanted);
        for _ in 0..wanted {
            scratch.clear();
            if reader.read_until(b'\n', &mut scratch)? == 0 {
                break;
            }
            out.push(render(&scratch));
        }
        Ok(out)
    }
}

/// Turn one raw line into something displayable.
fn render(raw: &[u8]) -> String {
    let mut end = raw.len();
    // Drop the line ending, both kinds: a file written on Windows would
    // otherwise show a stray glyph at the end of every line.
    if end > 0 && raw[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && raw[end - 1] == b'\r' {
        end -= 1;
    }

    let clipped = end > MAX_LINE;
    // Truncating a UTF-8 sequence mid-character is fine here: the lossy
    // conversion turns the fragment into a replacement character rather than
    // failing, which is the right outcome for a preview.
    let mut text = String::from_utf8_lossy(&raw[..end.min(MAX_LINE)]).replace('\t', "    ");
    if clipped {
        text.push_str(" ...");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn lines_are_counted_with_or_without_a_final_newline() {
        let dir = tempfile::tempdir().unwrap();

        let path = write(dir.path(), "a.txt", "one\ntwo\nthree\n");
        assert_eq!(LineIndex::build(&path).unwrap().lines(), 3);

        // No trailing newline: the last line still counts.
        let path = write(dir.path(), "b.txt", "one\ntwo\nthree");
        assert_eq!(LineIndex::build(&path).unwrap().lines(), 3);

        let path = write(dir.path(), "c.txt", "");
        assert_eq!(LineIndex::build(&path).unwrap().lines(), 0);

        // Blank lines are lines.
        let path = write(dir.path(), "d.txt", "\n\n\n");
        assert_eq!(LineIndex::build(&path).unwrap().lines(), 3);
    }

    #[test]
    fn any_window_of_lines_can_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..1000).map(|i| format!("line {i:04}\n")).collect();
        let path = write(dir.path(), "big.txt", &body);
        let index = LineIndex::build(&path).unwrap();
        assert_eq!(index.lines(), 1000);

        // The start.
        assert_eq!(
            index.read(0, 3).unwrap(),
            ["line 0000", "line 0001", "line 0002"]
        );
        // The middle, well past several anchors.
        assert_eq!(index.read(700, 2).unwrap(), ["line 0700", "line 0701"]);
        // Straddling an anchor boundary, where the walk-forward arithmetic
        // would show up if it were wrong.
        assert_eq!(
            index.read(STRIDE - 1, 3).unwrap(),
            [
                format!("line {:04}", STRIDE - 1),
                format!("line {STRIDE:04}"),
                format!("line {:04}", STRIDE + 1)
            ]
        );
        // The end, asking for more than is there.
        assert_eq!(index.read(998, 50).unwrap(), ["line 0998", "line 0999"]);
        // Past the end entirely.
        assert!(index.read(5000, 10).unwrap().is_empty());
        assert!(index.read(0, 0).unwrap().is_empty());
    }

    #[test]
    fn a_file_far_past_the_old_cap_is_still_readable_to_the_last_line() {
        // The whole point: 256 KiB used to be the end of the file as far as
        // the viewer was concerned.
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..60_000)
            .map(|i| format!("row {i:06} padding\n"))
            .collect();
        assert!(body.len() > 1_000_000, "want a file well past the old cap");
        let path = write(dir.path(), "huge.log", &body);

        let index = LineIndex::build(&path).unwrap();
        assert_eq!(index.lines(), 60_000);
        assert!(!index.partial());
        assert_eq!(index.read(59_999, 1).unwrap(), ["row 059999 padding"]);
        assert_eq!(index.read(30_000, 1).unwrap(), ["row 030000 padding"]);

        // And the index is a rounding error next to the file.
        assert!(
            index.index_bytes() < body.len() / 100,
            "index was {} bytes for a {}-byte file",
            index.index_bytes(),
            body.len()
        );
    }

    #[test]
    fn line_endings_and_tabs_are_tidied() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "crlf.txt", "one\r\ntwo\r\nthree");
        let index = LineIndex::build(&path).unwrap();
        assert_eq!(index.read(0, 3).unwrap(), ["one", "two", "three"]);

        let path = write(dir.path(), "tabs.txt", "a\tb\n");
        let index = LineIndex::build(&path).unwrap();
        assert_eq!(index.read(0, 1).unwrap(), ["a    b"]);
    }

    #[test]
    fn one_enormous_line_does_not_become_an_enormous_string() {
        // A minified bundle is one line of megabytes, and pulling it into a
        // label would be a memory spike for something nobody can read.
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "min.js",
            &format!("{}\nafter\n", "x".repeat(200_000)),
        );
        let index = LineIndex::build(&path).unwrap();

        let lines = index.read(0, 2).unwrap();
        assert!(lines[0].len() < MAX_LINE + 16);
        assert!(lines[0].ends_with(" ..."), "clipping should be visible");
        // And the line after it is still found.
        assert_eq!(lines[1], "after");
    }

    #[test]
    fn a_binary_does_not_panic_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [0xff, 0xfe, b'\n', 0x80, 0x81, b'\n']).unwrap();

        let index = LineIndex::build(&path).unwrap();
        assert_eq!(index.lines(), 2);
        // Lossy, not an error: invalid bytes become replacement characters.
        assert_eq!(index.read(0, 2).unwrap().len(), 2);
    }

    #[test]
    fn a_file_that_vanishes_between_index_and_read_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "gone.txt", "one\ntwo\n");
        let index = LineIndex::build(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert!(index.read(0, 1).is_err());
    }
}
