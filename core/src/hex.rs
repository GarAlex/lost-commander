//! Showing a file as bytes, for the files that are not text.
//!
//! Every viewer in this program has so far assumed its file could be read.
//! A compiled program, a photo, an archive - handed to a text viewer those
//! come out as a screenful of replacement characters, which is worse than
//! nothing because it looks like a bug rather than like a binary.
//!
//! The layout is `hexdump -C`'s, because that is the one every reader already
//! knows: an offset, sixteen bytes in two groups of eight, and the same
//! sixteen bytes again as characters with the unprintable ones as dots. The
//! two groups exist so the eye can count to eight without counting.
//!
//! Nothing here holds a file. A dump's rows are at fixed offsets - row `n`
//! starts at byte `n * 16` - so the window on screen is the only part that is
//! ever read, and a four-gigabyte file costs exactly as much as a small one.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes to a row. Sixteen, as every hex dump since the 1970s.
pub const PER_ROW: usize = 16;

/// Where the two groups of eight are split.
const GROUP: usize = 8;

/// How much is read to decide whether a file is text.
const SNIFF: usize = 8_192;

/// One row of the dump: where it starts, and what is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

impl Row {
    /// The hex column, padded so a short last row keeps the text column
    /// where the rows above put it.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(PER_ROW * 3 + 1);
        for at in 0..PER_ROW {
            match self.bytes.get(at) {
                Some(byte) => out.push_str(&format!("{byte:02x}")),
                None => out.push_str("  "),
            }
            out.push(' ');
            if at + 1 == GROUP {
                out.push(' ');
            }
        }
        out.pop();
        out
    }

    /// The hex pair for one position, blank past the end of a short row.
    pub fn pair(&self, at: usize) -> String {
        match self.bytes.get(at) {
            Some(byte) => format!("{byte:02x}"),
            None => "  ".to_string(),
        }
    }

    /// The same bytes as characters, with the unreadable ones as dots.
    pub fn text(&self) -> String {
        self.bytes.iter().map(|b| printable(*b)).collect()
    }
}

/// A byte as the text column shows it.
///
/// Only ASCII, and only the printable half of it. Anything else is a dot -
/// including the bytes that make up a UTF-8 character, because a dump is
/// about bytes and half a character drawn in a fixed grid is a lie about
/// where the next byte starts.
pub fn printable(byte: u8) -> char {
    match byte {
        0x20..=0x7e => byte as char,
        _ => '.',
    }
}

/// How many hex digits the offsets of a file this size need.
///
/// Eight is the conventional width and covers four gigabytes; a larger file
/// gets what it needs rather than a column that silently stops lining up.
pub fn offset_width(size: u64) -> usize {
    let needed = format!("{:x}", size.max(1)).len();
    needed.max(8)
}

/// One row, laid out as `hexdump -C` lays it out.
pub fn line(row: &Row, offset_width: usize) -> String {
    format!(
        "{:0width$x}  {}  |{}|",
        row.offset,
        row.hex(),
        row.text(),
        width = offset_width
    )
}

/// Cut a block of bytes into rows, the first starting at `offset`.
pub fn rows_of(bytes: &[u8], offset: u64) -> Vec<Row> {
    bytes
        .chunks(PER_ROW)
        .enumerate()
        .map(|(index, chunk)| Row {
            offset: offset + (index * PER_ROW) as u64,
            bytes: chunk.to_vec(),
        })
        .collect()
}

/// Which byte positions of two rows differ.
///
/// A position one row has and the other does not counts as a difference: one
/// file ending where the other carries on is exactly what someone comparing
/// two binaries wants pointed out.
pub fn differing(left: Option<&Row>, right: Option<&Row>) -> Vec<bool> {
    (0..PER_ROW)
        .map(|at| {
            let l = left.and_then(|row| row.bytes.get(at));
            let r = right.and_then(|row| row.bytes.get(at));
            l != r
        })
        .collect()
}

/// A file being read as bytes.
///
/// Holds a path and a size, not a file: reading a window opens, seeks, reads
/// and closes. That costs one system call more than keeping the handle and
/// saves having to thread a mutable borrow through every draw - and a view
/// only reads when something moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    pub path: PathBuf,
    pub size: u64,
}

impl Dump {
    pub fn open(path: &Path) -> io::Result<Dump> {
        let size = path.metadata()?.len();
        Ok(Dump {
            path: path.to_path_buf(),
            size,
        })
    }

    /// How many rows the whole file comes to.
    pub fn rows(&self) -> u64 {
        self.size.div_ceil(PER_ROW as u64)
    }

    pub fn offset_width(&self) -> usize {
        offset_width(self.size)
    }

    /// `count` rows starting at row `from`, or fewer at the end of the file.
    pub fn read(&self, from: u64, count: usize) -> io::Result<Vec<Row>> {
        if count == 0 || from >= self.rows() {
            return Ok(Vec::new());
        }
        let offset = from * PER_ROW as u64;
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; count * PER_ROW];
        let mut filled = 0;
        while filled < bytes.len() {
            match file.read(&mut bytes[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        bytes.truncate(filled);
        Ok(rows_of(&bytes, offset))
    }
}

/// Whether a file should be read as bytes rather than as text.
///
/// The head only: a file is what its first few thousand bytes say it is, and
/// reading a gigabyte to find out is not an answer anyone waits for.
pub fn is_binary(path: &Path) -> io::Result<bool> {
    let file = std::fs::File::open(path)?;
    let mut head = Vec::new();
    file.take(SNIFF as u64).read_to_end(&mut head)?;
    Ok(!crate::preview::looks_like_text(&head))
}

/// Where two files first differ, or `None` when they do not.
///
/// The one thing worth saying about two binaries beyond yes or no: an offset
/// is something you can go and look at, and "they differ" is not.
pub fn first_difference(left: &Path, right: &Path) -> io::Result<Option<u64>> {
    let mut a = std::fs::File::open(left)?;
    let mut b = std::fs::File::open(right)?;
    let mut left_buf = vec![0u8; 64 * 1024];
    let mut right_buf = vec![0u8; 64 * 1024];
    let mut at = 0u64;
    loop {
        let read = fill(&mut a, &mut left_buf)?;
        let other = fill(&mut b, &mut right_buf)?;
        let common = read.min(other);
        for i in 0..common {
            if left_buf[i] != right_buf[i] {
                return Ok(Some(at + i as u64));
            }
        }
        if read != other {
            // One ran out first: the difference is where the shorter ended.
            return Ok(Some(at + common as u64));
        }
        if read == 0 {
            return Ok(None);
        }
        at += read as u64;
    }
}

fn fill(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

// ---- editing -------------------------------------------------------------
//
// A hex editor **overwrites**. It does not insert and it does not delete,
// because inserting one byte moves every byte after it - which turns a
// targeted fix to a header into a rewrite of the whole file, and is a
// different and far more dangerous operation than the one anyone opens a hex
// editor to perform. The file's length never changes here.

/// Which column the cursor is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// The hex column: two keystrokes to a byte.
    #[default]
    Hex,
    /// The character column: one keystroke to a byte, for patching strings.
    Text,
}

impl Pane {
    pub fn other(self) -> Pane {
        match self {
            Pane::Hex => Pane::Text,
            Pane::Text => Pane::Hex,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Pane::Hex => "hex",
            Pane::Text => "text",
        }
    }
}

/// Where the cursor is, down to which half of the byte.
///
/// The half matters: a byte is two hex digits, and an editor that replaced the
/// whole byte on the first keystroke would make `4f` unreachable except by
/// typing `04` and then `4f`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub at: u64,
    /// True when the next digit typed is the low half of the byte.
    pub low: bool,
    pub pane: Pane,
}

impl Cursor {
    /// Move by whole bytes, clamped to the file.
    pub fn to(&mut self, at: u64, size: u64) {
        self.at = at.min(size.saturating_sub(1));
        self.low = false;
    }

    pub fn step(&mut self, by: i64, size: u64) {
        let at = self.at as i64 + by;
        self.to(at.clamp(0, size.saturating_sub(1) as i64) as u64, size);
    }

    pub fn row(&self) -> u64 {
        self.at / PER_ROW as u64
    }

    pub fn column(&self) -> usize {
        (self.at % PER_ROW as u64) as usize
    }
}

/// A hex digit's value, or nothing if it is not one.
/// Where a typed offset means, or `None` if it means nowhere.
///
/// Hex by default, because every offset the view shows is hex and retyping
/// one you are looking at is the common case. `0x` is accepted for people
/// with the habit, and `+` or a leading `0n` asks for decimal - without some
/// way to say so, `100` could only ever mean 256 and somebody wanting byte
/// one hundred has no way to ask.
///
/// Underscores and spaces are allowed as groupings, so an offset copied out
/// of a dump or a spec still parses.
///
/// Past the end of the file lands on the last byte rather than refusing: the
/// reader asked to go as far as possible in that direction, and an error
/// message where the cursor should be is not an answer.
pub fn parse_offset(text: &str, size: u64) -> Option<u64> {
    let text = text.trim();
    let (digits, radix) = if let Some(rest) = text.strip_prefix("0x").or(text.strip_prefix("0X")) {
        (rest, 16)
    } else if let Some(rest) = text.strip_prefix("0n").or(text.strip_prefix("0N")) {
        (rest, 10)
    } else if let Some(rest) = text.strip_prefix('+') {
        (rest, 10)
    } else {
        (text, 16)
    };

    let cleaned: String = digits
        .chars()
        .filter(|c| *c != '_' && *c != ' ' && *c != ',')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let at = u64::from_str_radix(&cleaned, radix).ok()?;
    // An empty file has no byte to land on.
    Some(at.min(size.saturating_sub(1)))
}

pub fn hex_digit(character: char) -> Option<u8> {
    character.to_digit(16).map(|value| value as u8)
}

/// The byte that results from typing `digit` over `current`.
pub fn with_nibble(current: u8, digit: u8, low: bool) -> u8 {
    if low {
        (current & 0xF0) | (digit & 0x0F)
    } else {
        (current & 0x0F) | ((digit & 0x0F) << 4)
    }
}

/// Bytes changed but not yet written.
///
/// Sparse, because a change is almost always a handful of bytes in a file that
/// may be gigabytes - and holding the file to edit four bytes of it would
/// defeat the whole point of a dump that reads only what is on screen.
///
/// Each entry keeps what *was* there as well as what is there now, which buys
/// two things: undo is exact, and a byte typed back to its original value
/// stops counting as a change instead of being a change that happens to look
/// the same.
#[derive(Debug, Clone, Default)]
pub struct Edits {
    changed: std::collections::BTreeMap<u64, (u8, u8)>,
    /// The order they were made in, for undo.
    order: Vec<u64>,
}

impl Edits {
    /// Record a byte. `was` is what the file has at that offset.
    pub fn set(&mut self, at: u64, was: u8, now: u8) {
        if was == now {
            // Typed back to what it already was: not a change.
            self.changed.remove(&at);
            self.order.retain(|offset| *offset != at);
            return;
        }
        // The first recorded `was` is the one from the file; a second edit to
        // the same byte must not overwrite it with the first edit's value, or
        // undo would restore a value that was never on disk.
        let original = self.changed.get(&at).map(|(was, _)| *was).unwrap_or(was);
        if self.changed.insert(at, (original, now)).is_none() {
            self.order.push(at);
        }
    }

    pub fn get(&self, at: u64) -> Option<u8> {
        self.changed.get(&at).map(|(_, now)| *now)
    }

    pub fn is_changed(&self, at: u64) -> bool {
        self.changed.contains_key(&at)
    }

    pub fn len(&self) -> usize {
        self.changed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }

    pub fn clear(&mut self) {
        self.changed.clear();
        self.order.clear();
    }

    /// Undo the most recent change, and say where it was so the cursor can
    /// go and look at it.
    pub fn undo(&mut self) -> Option<u64> {
        let at = self.order.pop()?;
        self.changed.remove(&at);
        Some(at)
    }

    /// Lay the changes over a row read from the file.
    pub fn overlay(&self, row: &mut Row) {
        for (index, byte) in row.bytes.iter_mut().enumerate() {
            if let Some(now) = self.get(row.offset + index as u64) {
                *byte = now;
            }
        }
    }

    /// The changes as consecutive runs, which is what makes writing them one
    /// seek per run rather than one per byte.
    pub fn runs(&self) -> Vec<(u64, Vec<u8>)> {
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for (at, (_, now)) in &self.changed {
            match runs.last_mut() {
                Some((start, bytes)) if *start + bytes.len() as u64 == *at => bytes.push(*now),
                _ => runs.push((*at, vec![*now])),
            }
        }
        runs
    }

    /// What was changed, in words, for the line under the dump.
    pub fn describe(&self) -> String {
        match self.changed.len() {
            0 => String::new(),
            1 => {
                let (at, (was, now)) = self.changed.iter().next().expect("one");
                format!("1 byte changed: {at:#x} {was:02x} -> {now:02x}")
            }
            n => format!("{n} bytes changed"),
        }
    }
}

/// Write the changed bytes back where they came from.
///
/// In place, over the existing bytes, leaving the file exactly as long as it
/// was. Returns how many bytes were written.
pub fn write_back(path: &Path, edits: &Edits) -> io::Result<usize> {
    use std::io::Write;
    if edits.is_empty() {
        return Ok(0);
    }
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    let size = file.metadata()?.len();
    let mut written = 0;
    for (at, bytes) in edits.runs() {
        // A file that shrank under us must not be extended by a write past
        // its end - that would be an insert, which this deliberately is not.
        if at >= size {
            continue;
        }
        let room = (size - at) as usize;
        let bytes = &bytes[..bytes.len().min(room)];
        file.seek(SeekFrom::Start(at))?;
        file.write_all(bytes)?;
        written += bytes.len();
    }
    file.flush()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(offset: u64, bytes: &[u8]) -> Row {
        Row {
            offset,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn a_full_row_reads_the_way_hexdump_writes_it() {
        let bytes: Vec<u8> = b"Hello, world!\x00\x01\x02".to_vec();
        let row = row(0, &bytes);
        assert_eq!(
            line(&row, 8),
            "00000000  48 65 6c 6c 6f 2c 20 77  6f 72 6c 64 21 00 01 02  |Hello, world!...|"
        );
    }

    #[test]
    fn a_short_last_row_keeps_the_columns_where_they_were() {
        let full = line(&row(0, b"0123456789abcdef"), 8);
        let short = line(&row(0x10, b"tail"), 8);
        // The text column starts at the same place in both, which is the
        // whole reason the hex column is padded.
        assert_eq!(
            full.find('|').unwrap(),
            short.find('|').unwrap(),
            "\\n{full}\\n{short}"
        );
        assert_eq!(
            short.trim_end(),
            "00000010  74 61 69 6c                                       |tail|"
        );
    }

    #[test]
    fn only_printable_ascii_survives_into_the_text_column() {
        let row = row(0, &[0x00, 0x09, 0x1f, 0x20, 0x41, 0x7e, 0x7f, 0x80, 0xff]);
        assert_eq!(row.text(), "...  A~...".replace("  ", " "));
        assert_eq!(printable(b'A'), 'A');
        assert_eq!(printable(b' '), ' ');
        assert_eq!(printable(0x7f), '.', "delete is not printable");
        assert_eq!(printable(0xc3), '.', "nor is half of a UTF-8 character");
    }

    #[test]
    fn the_offset_column_is_eight_wide_until_it_has_to_be_more() {
        assert_eq!(offset_width(0), 8);
        assert_eq!(offset_width(1024), 8);
        assert_eq!(offset_width(0xffff_ffff), 8);
        assert_eq!(offset_width(0x1_0000_0000), 9, "past four gigabytes");
    }

    #[test]
    fn rows_are_cut_at_sixteen_and_numbered_from_where_they_start() {
        let bytes: Vec<u8> = (0..20u8).collect();
        let rows = rows_of(&bytes, 0x100);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, 0x100);
        assert_eq!(rows[0].bytes.len(), 16);
        assert_eq!(rows[1].offset, 0x110);
        assert_eq!(rows[1].bytes, [16, 17, 18, 19]);
    }

    #[test]
    fn differing_marks_the_bytes_that_differ_and_the_ones_that_are_missing() {
        let left = row(0, &[1, 2, 3, 4]);
        let right = row(0, &[1, 9, 3]);
        let marks = differing(Some(&left), Some(&right));
        assert_eq!(marks.len(), PER_ROW);
        assert!(!marks[0], "the same");
        assert!(marks[1], "2 against 9");
        assert!(!marks[2]);
        assert!(marks[3], "one row ended and the other did not");
        assert!(!marks[4], "past the end of both is not a difference");

        // A row against nothing at all is all difference, as far as it goes.
        let marks = differing(Some(&left), None);
        assert_eq!(marks[..4], [true, true, true, true]);
    }

    /// A file of `size` bytes, counting up and wrapping.
    fn a_file(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        let bytes: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn only_the_window_asked_for_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_file(dir.path(), "blob.bin", 1000);
        let dump = Dump::open(&path).unwrap();

        assert_eq!(dump.size, 1000);
        assert_eq!(
            dump.rows(),
            63,
            "1000 bytes is 62 full rows and a short one"
        );

        let window = dump.read(10, 3).unwrap();
        assert_eq!(window.len(), 3);
        assert_eq!(window[0].offset, 160);
        assert_eq!(window[0].bytes[0], 160u8, "byte 160 of a file counting up");

        // The last row is short, and asking past the end gives nothing rather
        // than an error.
        let last = dump.read(62, 4).unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].bytes.len(), 1000 - 62 * 16);
        assert!(dump.read(63, 4).unwrap().is_empty());
        assert!(dump.read(10, 0).unwrap().is_empty());
    }

    #[test]
    fn an_empty_file_has_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_file(dir.path(), "empty.bin", 0);
        let dump = Dump::open(&path).unwrap();
        assert_eq!(dump.rows(), 0);
        assert!(dump.read(0, 10).unwrap().is_empty());
        assert_eq!(dump.offset_width(), 8);
    }

    #[test]
    fn a_binary_is_told_from_text_by_its_head() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, "one\ntwo\nthree\n").unwrap();
        assert!(!is_binary(&text).unwrap());

        let binary = dir.path().join("a.bin");
        std::fs::write(&binary, [0x7f, b'E', b'L', b'F', 0, 0, 0, 0]).unwrap();
        assert!(is_binary(&binary).unwrap());

        // A long text file with a NUL a long way in still reads as text,
        // because only the head is looked at - which is the trade that keeps
        // this instant on a file of any size.
        let mostly = dir.path().join("mostly.txt");
        let mut bytes = vec![b'a'; SNIFF * 2];
        bytes.push(0);
        std::fs::write(&mostly, bytes).unwrap();
        assert!(!is_binary(&mostly).unwrap());
    }

    #[test]
    fn the_first_difference_is_an_offset_worth_going_to() {
        let dir = tempfile::tempdir().unwrap();
        let a = a_file(dir.path(), "a.bin", 100_000);
        let b = a_file(dir.path(), "b.bin", 100_000);
        assert_eq!(first_difference(&a, &b).unwrap(), None, "identical");

        // A change well past the first read block is still found.
        let mut bytes = std::fs::read(&b).unwrap();
        bytes[70_000] ^= 0xff;
        std::fs::write(&b, &bytes).unwrap();
        assert_eq!(first_difference(&a, &b).unwrap(), Some(70_000));

        // One file being a prefix of the other differs where it ends.
        let short = dir.path().join("short.bin");
        std::fs::write(&short, &bytes[..500]).unwrap();
        assert_eq!(first_difference(&short, &b).unwrap(), Some(500));
        assert_eq!(first_difference(&b, &short).unwrap(), Some(500));
    }

    // ---- editing ---------------------------------------------------------

    #[test]
    fn a_byte_takes_two_keystrokes_one_nibble_at_a_time() {
        // Replacing the whole byte on the first keystroke would make 0x4f
        // reachable only by typing 04 and then 4f.
        assert_eq!(with_nibble(0x00, 0x4, false), 0x40);
        assert_eq!(with_nibble(0x40, 0xf, true), 0x4f);
        // And the other half is left alone either way round.
        assert_eq!(with_nibble(0xab, 0x0, false), 0x0b);
        assert_eq!(with_nibble(0xab, 0x0, true), 0xa0);

        assert_eq!(hex_digit('0'), Some(0));
        assert_eq!(hex_digit('9'), Some(9));
        assert_eq!(hex_digit('a'), Some(10));
        assert_eq!(hex_digit('F'), Some(15));
        assert_eq!(hex_digit('g'), None);
        assert_eq!(hex_digit(' '), None);
    }

    #[test]
    fn a_byte_typed_back_to_what_it_was_stops_being_a_change() {
        let mut edits = Edits::default();
        edits.set(10, 0xaa, 0xbb);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits.get(10), Some(0xbb));
        assert!(edits.is_changed(10));

        edits.set(10, 0xaa, 0xaa);
        assert!(edits.is_empty(), "back to itself is not an edit");
        assert_eq!(edits.get(10), None);
    }

    #[test]
    fn undoing_restores_what_the_file_had_and_not_the_step_before() {
        // Editing the same byte twice must remember the value *on disk*, not
        // the first edit's - or one undo leaves a value that was never there.
        let mut edits = Edits::default();
        edits.set(4, 0x11, 0x22);
        edits.set(4, 0x22, 0x33);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits.get(4), Some(0x33));

        // And one undo clears the byte entirely, back to the file's own value.
        assert_eq!(edits.undo(), Some(4));
        assert!(edits.is_empty());

        // Undo goes back in the order the edits were made.
        let mut edits = Edits::default();
        edits.set(1, 0, 1);
        edits.set(9, 0, 2);
        edits.set(5, 0, 3);
        assert_eq!(edits.undo(), Some(5));
        assert_eq!(edits.undo(), Some(9));
        assert_eq!(edits.undo(), Some(1));
        assert_eq!(edits.undo(), None);
    }

    #[test]
    fn changes_are_laid_over_the_rows_read_from_the_file() {
        let mut edits = Edits::default();
        edits.set(0, b'h', b'H');
        edits.set(4, b'o', b'0');

        let mut row = Row {
            offset: 0,
            bytes: b"hello".to_vec(),
        };
        edits.overlay(&mut row);
        assert_eq!(row.bytes, b"Hell0".to_vec());

        // A row that does not contain the changed offsets is untouched.
        let mut elsewhere = Row {
            offset: 64,
            bytes: b"hello".to_vec(),
        };
        edits.overlay(&mut elsewhere);
        assert_eq!(elsewhere.bytes, b"hello".to_vec());
    }

    #[test]
    fn neighbouring_changes_become_one_run() {
        let mut edits = Edits::default();
        for (at, byte) in [(5u64, 1u8), (6, 2), (7, 3), (20, 9)] {
            edits.set(at, 0, byte);
        }
        assert_eq!(edits.runs(), vec![(5, vec![1, 2, 3]), (20, vec![9])]);
    }

    #[test]
    fn writing_back_changes_the_bytes_and_not_the_length() {
        // The whole contract of a hex editor: it overwrites. Inserting one
        // byte would move every byte after it, which is a rewrite of the file
        // rather than a fix to it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patch.bin");
        std::fs::write(&path, b"hello world").unwrap();

        let mut edits = Edits::default();
        edits.set(0, b'h', b'H');
        edits.set(6, b'w', b'W');
        assert_eq!(write_back(&path, &edits).unwrap(), 2);

        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, b"Hello World".to_vec());
        assert_eq!(after.len(), 11, "the same length as it went in");
    }

    #[test]
    fn writing_back_nothing_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("untouched.bin");
        std::fs::write(&path, b"as it was").unwrap();
        assert_eq!(write_back(&path, &Edits::default()).unwrap(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"as it was".to_vec());
    }

    #[test]
    fn a_file_that_shrank_is_not_extended_by_the_write() {
        // The dump was taken when the file was longer. Writing past the end
        // would grow it, which is the one thing this must never do.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shrank.bin");
        std::fs::write(&path, b"1234567890").unwrap();

        let mut edits = Edits::default();
        edits.set(8, 0, b'X');
        edits.set(9, 0, b'Y');
        edits.set(10, 0, b'Z'); // past the end
        std::fs::write(&path, b"12345678").unwrap();

        write_back(&path, &edits).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            8,
            "not one byte longer"
        );
    }

    #[test]
    fn the_cursor_walks_bytes_and_stops_at_the_ends() {
        let mut cursor = Cursor::default();
        assert_eq!(cursor.at, 0);
        assert_eq!(cursor.pane, Pane::Hex);

        cursor.step(1, 100);
        assert_eq!(cursor.at, 1);
        cursor.step(-5, 100);
        assert_eq!(cursor.at, 0, "not past the start");
        cursor.step(1000, 100);
        assert_eq!(cursor.at, 99, "not past the end");

        cursor.to(20, 100);
        assert_eq!((cursor.row(), cursor.column()), (1, 4));
        // Moving resets the half, so the next digit is the high one - the
        // alternative is a cursor that lands mid-byte and eats a keystroke.
        cursor.low = true;
        cursor.step(1, 100);
        assert!(!cursor.low);

        assert_eq!(Pane::Hex.other(), Pane::Text);
        assert_eq!(Pane::Text.other(), Pane::Hex);
    }

    #[test]
    fn what_was_changed_is_said_in_words() {
        let mut edits = Edits::default();
        assert_eq!(edits.describe(), "");
        edits.set(0x2a, 0x00, 0xff);
        assert_eq!(edits.describe(), "1 byte changed: 0x2a 00 -> ff");
        edits.set(0x2b, 0x00, 0x01);
        assert_eq!(edits.describe(), "2 bytes changed");
    }

    #[test]
    fn a_typed_offset_is_hex_because_every_offset_on_screen_is() {
        assert_eq!(parse_offset("1f400", 0x20000), Some(0x1f400));
        assert_eq!(parse_offset("1F400", 0x20000), Some(0x1f400));
        assert_eq!(parse_offset("0x1f400", 0x20000), Some(0x1f400));
        // Groupings, so an offset copied out of a dump or a spec parses.
        assert_eq!(parse_offset("  1f_40 0 ", 0x20000), Some(0x1f400));
    }

    #[test]
    fn there_is_a_way_to_ask_for_decimal() {
        // Without one, `100` could only mean 256 and somebody wanting byte
        // one hundred would have no way to say so.
        assert_eq!(parse_offset("100", 1000), Some(0x100));
        assert_eq!(parse_offset("0n100", 1000), Some(100));
        assert_eq!(parse_offset("+100", 1000), Some(100));
    }

    #[test]
    fn past_the_end_lands_on_the_last_byte() {
        // The reader asked to go as far as possible that way. An error where
        // the cursor should be is not an answer to that.
        assert_eq!(parse_offset("ffffffff", 0x100), Some(0xff));
        // And an empty file has nowhere to land at all.
        assert_eq!(parse_offset("0", 0), Some(0));
    }

    #[test]
    fn what_is_not_an_offset_is_refused_rather_than_guessed_at() {
        assert_eq!(parse_offset("", 0x100), None);
        assert_eq!(parse_offset("   ", 0x100), None);
        assert_eq!(parse_offset("zz", 0x100), None);
        assert_eq!(parse_offset("0x", 0x100), None);
        // Decimal-only digits under a decimal prefix: `f` is not one.
        assert_eq!(parse_offset("0nff", 0x100), None);
    }
}
