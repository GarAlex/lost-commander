// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A text file open for editing: its bytes, what they were read as, and what
//! they will be written back as.
//!
//! The two encodings are deliberately separate, because they answer different
//! questions and confusing them loses work:
//!
//! * **Read as** is what the bytes on disk mean. Getting it wrong shows a
//!   screen of replacement characters, and putting it right is re-reading the
//!   *same bytes* a different way - nothing has been written and nothing is
//!   lost.
//! * **Write as** is what to put back. Changing it converts the file, which is
//!   a real change to the disk and can lose characters the target cannot hold.
//!
//! A window with one encoding box cannot express "this is CP1251 and I want it
//! saved as UTF-8", which is the single most useful thing anyone opens such a
//! window to do.
//!
//! Line endings get the same treatment: whatever the file had is what it gets
//! back, because a diff that says every line changed because an editor
//! silently converted CRLF to LF is a diff nobody can review.

use std::io;
use std::path::{Path, PathBuf};

use crate::encoding::{self, Detected, Encoding, Newline};

/// The largest file this will open.
///
/// Editing means holding the whole thing as text, twice - once as it was and
/// once as it is - plus whatever the text widget keeps. Viewing has no such
/// limit: [`crate::textindex`] scrolls a gigabyte-long log by keeping only
/// where the lines are. This is the price of being able to type into it.
pub const MAX_BYTES: u64 = 16 << 20;

/// One text file, open.
#[derive(Debug, Clone)]
pub struct Document {
    pub path: PathBuf,
    /// Exactly what was read, kept so the encoding can be changed without
    /// going back to the disk - and so that "read it as something else" is
    /// free and cannot fail.
    bytes: Vec<u8>,
    /// What [`encoding::sniff`] made of it, kept for the line that says so.
    pub detected: Detected,
    /// The encoding the text was decoded with.
    pub read_as: Encoding,
    /// The encoding it will be written back as.
    pub write_as: Encoding,
    /// The line ending it will be written back with.
    pub newline: Newline,
    /// The line ending the file was read with. Kept, because `text` has
    /// already been normalised to `\n` and so cannot be asked - reading the
    /// endings back off it always answers `Lf` and makes every CRLF file look
    /// as though it had been converted.
    read_newline: Newline,
    /// The text, always with `\n` endings - which is what every text widget
    /// works in, and what [`encoding::to_newline`] puts back on the way out.
    pub text: String,
    /// The text as it was decoded, so "has this changed?" is a comparison
    /// rather than a flag somebody has to remember to set.
    original: String,
}

impl Document {
    /// Read a file and work out what its bytes mean.
    pub fn open(path: &Path) -> io::Result<Document> {
        let size = std::fs::metadata(path)?.len();
        if size > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is too big to edit here - {} MB, and the limit is {} MB",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    size / (1 << 20),
                    MAX_BYTES / (1 << 20)
                ),
            ));
        }
        Ok(Document::from_bytes(
            path.to_path_buf(),
            std::fs::read(path)?,
        ))
    }

    /// The same, from bytes already in hand - which is what makes every rule
    /// here testable without a filesystem.
    pub fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> Document {
        let detected = encoding::sniff(&bytes);
        let text = encoding::decode(&bytes, detected.encoding);
        let newline = encoding::sniff_newline(&text);
        let text = encoding::to_newline(&text, Newline::Lf);
        Document {
            path,
            bytes,
            detected,
            read_as: detected.encoding,
            write_as: detected.encoding,
            newline,
            read_newline: newline,
            original: text.clone(),
            text,
        }
    }

    /// Whether anything has been typed since it was opened or last saved.
    pub fn is_edited(&self) -> bool {
        self.text != self.original
    }

    /// Whether saving would write something different from what was read -
    /// including a conversion with nothing typed at all, which is exactly what
    /// "open it as CP1251, save it as UTF-8" is.
    pub fn would_change_the_file(&self) -> bool {
        self.is_edited() || self.write_as != self.read_as || self.newline_changed()
    }

    fn newline_changed(&self) -> bool {
        self.newline != self.read_newline
    }

    /// How big the file is, in bytes as read.
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// Read the same bytes a different way.
    ///
    /// Free and lossless - nothing has been written - so it can be tried until
    /// the text looks like text. Anything typed is discarded, which is why the
    /// front-ends ask first when [`Document::is_edited`].
    pub fn read_again_as(&mut self, encoding: Encoding) {
        let text = encoding::decode(&self.bytes, encoding);
        self.newline = encoding::sniff_newline(&text);
        self.read_newline = self.newline;
        self.text = encoding::to_newline(&text, Newline::Lf);
        self.original = self.text.clone();
        self.read_as = encoding;
        self.write_as = encoding;
    }

    /// The bytes a save would write, and what could not be represented.
    pub fn to_bytes(&self) -> encoding::Encoded {
        encoding::encode(
            &encoding::to_newline(&self.text, self.newline),
            self.write_as,
        )
    }

    /// Write it, and take the new state as the state it was opened in.
    ///
    /// Returns the characters that would not fit, having written `?` in their
    /// place - the caller says so out loud. It is not an error, because
    /// refusing to save is worse than saving with a warning, but it is not
    /// nothing either.
    pub fn save(&mut self, to: &Path) -> io::Result<Vec<char>> {
        let written = self.to_bytes();
        std::fs::write(to, &written.bytes)?;
        // Saved somewhere else is now the file being edited, as it is in every
        // editor: the next Save goes to where the last one went.
        self.path = to.to_path_buf();
        self.bytes = written.bytes;
        self.read_as = self.write_as;
        self.read_newline = self.newline;
        self.original = self.text.clone();
        Ok(written.lost)
    }

    /// How many lines there are, for the line under the box.
    pub fn lines(&self) -> usize {
        self.text.lines().count().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Confidence;

    fn document(bytes: Vec<u8>) -> Document {
        Document::from_bytes(PathBuf::from("/tmp/example.txt"), bytes)
    }

    #[test]
    fn a_utf8_file_opens_as_itself_and_is_not_edited() {
        let doc = document(b"hello\nworld\n".to_vec());
        assert_eq!(doc.text, "hello\nworld\n");
        assert_eq!(doc.read_as, Encoding::Utf8);
        assert_eq!(doc.write_as, Encoding::Utf8);
        assert_eq!(doc.newline, Newline::Lf);
        assert_eq!(doc.lines(), 2);
        assert!(!doc.is_edited());
        assert!(!doc.would_change_the_file());
    }

    #[test]
    fn line_endings_survive_a_round_trip_untouched() {
        // An editor that quietly converts CRLF to LF produces a diff in which
        // every line changed, which is a diff nobody can review.
        let mut doc = document(b"first\r\nsecond\r\n".to_vec());
        assert_eq!(doc.newline, Newline::Crlf);
        assert_eq!(doc.text, "first\nsecond\n", "editing happens in \\n");
        assert!(!doc.would_change_the_file());
        assert_eq!(doc.to_bytes().bytes, b"first\r\nsecond\r\n");

        // And changing them on purpose is a change to the file.
        doc.newline = Newline::Lf;
        assert!(doc.would_change_the_file());
        assert_eq!(doc.to_bytes().bytes, b"first\nsecond\n");
    }

    #[test]
    fn the_encoding_can_be_read_one_way_and_written_another() {
        // The thing anyone opens such a window to do: a Cyrillic file that
        // arrived as CP1251, saved out as UTF-8.
        let cp1251 = encoding::encode("Привет\n", Encoding::Cp1251).bytes;
        let mut doc = document(cp1251);
        assert_eq!(doc.read_as, Encoding::Cp1251);
        assert_eq!(doc.text, "Привет\n");
        assert!(!doc.would_change_the_file());

        doc.write_as = Encoding::Utf8;
        assert!(
            doc.would_change_the_file(),
            "a conversion is a change even with nothing typed"
        );
        let written = doc.to_bytes();
        assert!(written.is_lossless());
        assert_eq!(written.bytes, "Привет\n".as_bytes());
    }

    #[test]
    fn reading_it_again_a_different_way_costs_nothing_and_can_be_undone() {
        // The guess is wrong often enough that this has to be free: the bytes
        // are kept, so trying another encoding is a decode and not a re-read,
        // and going back to the first one gets exactly the first answer.
        let cp1251 = encoding::encode("Привет", Encoding::Cp1251).bytes;
        let mut doc = document(cp1251);
        let first = doc.text.clone();
        assert_eq!(doc.detected.confidence, Confidence::Guessed);

        doc.read_again_as(Encoding::Cp1252);
        assert_ne!(
            doc.text, first,
            "read the wrong way, it says something else"
        );
        assert!(!doc.is_edited(), "re-reading is not an edit");

        doc.read_again_as(Encoding::Cp1251);
        assert_eq!(doc.text, first, "and back again");
    }

    #[test]
    fn what_will_not_fit_is_reported_by_the_save_rather_than_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let mut doc = document(b"plain\n".to_vec());
        doc.text = "Привет\n".to_string();
        doc.write_as = Encoding::Cp1252;

        let lost = doc.save(&path).unwrap();
        assert_eq!(lost, vec!['П', 'р', 'и', 'в', 'е', 'т']);
        assert_eq!(std::fs::read(&path).unwrap(), b"??????\n");
    }

    #[test]
    fn saving_makes_what_was_typed_the_new_starting_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let mut doc = document(b"one\n".to_vec());
        doc.text = "one\ntwo\n".to_string();
        assert!(doc.is_edited());

        assert!(doc.save(&path).unwrap().is_empty());
        assert!(!doc.is_edited(), "saved is not edited");
        assert!(!doc.would_change_the_file());
        assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");

        // Saved somewhere else is now the file being edited, as it is in
        // every editor - the next save goes where the last one went.
        assert_eq!(doc.path, path);
    }

    #[test]
    fn saving_a_converted_file_leaves_it_settled_rather_than_always_dirty() {
        // After writing CP1251 out as UTF-8, the file *is* UTF-8 - so the
        // window must stop offering to convert it again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let mut doc = document(encoding::encode("Привет\n", Encoding::Cp1251).bytes);
        doc.write_as = Encoding::Utf8;
        assert!(doc.would_change_the_file());

        doc.save(&path).unwrap();
        assert_eq!(doc.read_as, Encoding::Utf8);
        assert!(!doc.would_change_the_file());
        assert_eq!(std::fs::read(&path).unwrap(), "Привет\n".as_bytes());
    }

    #[test]
    fn a_file_too_big_to_hold_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.txt");
        std::fs::write(&path, vec![b'x'; 64]).unwrap();
        assert!(Document::open(&path).is_ok(), "a small one is fine");

        let missing = dir.path().join("nothing.txt");
        assert!(Document::open(&missing).is_err());
    }

    #[test]
    fn an_empty_file_is_an_empty_document_and_not_a_problem() {
        let doc = document(Vec::new());
        assert_eq!(doc.text, "");
        assert_eq!(doc.lines(), 1);
        assert!(!doc.is_edited());
        assert!(!doc.would_change_the_file());
        assert!(doc.to_bytes().bytes.is_empty());
    }

    #[test]
    fn a_file_with_a_mark_keeps_it() {
        let mut doc = document(encoding::encode("hi\n", Encoding::Utf8Bom).bytes);
        assert_eq!(doc.read_as, Encoding::Utf8Bom);
        assert_eq!(doc.text, "hi\n", "the mark is not a character in the text");
        assert_eq!(
            doc.to_bytes().bytes,
            vec![0xEF, 0xBB, 0xBF, b'h', b'i', b'\n']
        );

        // And dropping it is a deliberate change, not something that happens.
        doc.write_as = Encoding::Utf8;
        assert!(doc.would_change_the_file());
        assert_eq!(doc.to_bytes().bytes, b"hi\n");
    }
}
