//! Recording a shell session to a file, from the pty byte stream.
//!
//! This is deliberately *not* taken from the screen. The emulator holds a
//! bounded scrollback and resolves things away - an editor's alternate screen,
//! a `clear` - so it can only ever answer "what could you scroll back to now".
//! A recording answers "everything that came out from the moment I asked",
//! however long it runs, and it catches whatever writes to that terminal: the
//! shell, the programs it starts, and anything that inherited the descriptor.
//!
//! What is written is plain text, not the raw stream: escape sequences are
//! dropped and carriage returns are honoured, so a progress bar that rewrote
//! its own line lands as one line in its final state rather than as forty.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Turns a terminal byte stream into plain text.
///
/// Enough of a terminal to make a readable log, and no more: it tracks one
/// line and a column within it, which is what carriage returns and backspaces
/// move. It deliberately does not track a screen, so cursor addressing is
/// dropped rather than obeyed - a full-screen program's output is noise in a
/// log either way.
#[derive(Debug, Default)]
pub struct Transcriber {
    /// The line being built. Indexed by byte, because a column is where the
    /// escape sequences put it, not where a grapheme boundary is.
    line: Vec<u8>,
    column: usize,
    state: State,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum State {
    #[default]
    Text,
    /// Just saw ESC.
    Escape,
    /// Inside `ESC [ ... final`, the sequences that colour and move.
    Csi,
    /// Inside `ESC ] ... BEL|ST` - window titles and the like.
    Osc,
    /// Saw ESC while in an OSC, which may be the start of its terminator.
    OscEscape,
    /// Inside a string-terminated sequence that is not OSC: DCS, SOS, PM, APC.
    String,
    StringEscape,
}

impl Transcriber {
    pub fn new() -> Transcriber {
        Transcriber::default()
    }

    /// Feed raw bytes; returns the plain text they completed.
    ///
    /// Only whole lines come back. Whatever is still being written stays in
    /// the buffer, since a carriage return could still rewrite it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Text => self.text(byte, &mut out),
                State::Escape => self.escape(byte),
                State::Csi => {
                    // A CSI ends at its final byte, 0x40..=0x7E.
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = State::Text;
                    }
                }
                State::Osc => match byte {
                    0x07 => self.state = State::Text, // BEL terminates
                    0x1b => self.state = State::OscEscape,
                    _ => {}
                },
                State::OscEscape => {
                    // ESC \ is the other terminator; anything else and we are
                    // still inside the string.
                    self.state = if byte == b'\\' {
                        State::Text
                    } else {
                        State::Osc
                    };
                }
                State::String => {
                    if byte == 0x1b {
                        self.state = State::StringEscape;
                    }
                }
                State::StringEscape => {
                    self.state = if byte == b'\\' {
                        State::Text
                    } else {
                        State::String
                    };
                }
            }
        }
        out
    }

    fn text(&mut self, byte: u8, out: &mut Vec<u8>) {
        match byte {
            0x1b => self.state = State::Escape,
            b'\n' => {
                out.extend_from_slice(&self.line);
                out.push(b'\n');
                self.line.clear();
                self.column = 0;
            }
            // The progress-bar case: back to the start, then overwrite.
            b'\r' => self.column = 0,
            0x08 => self.column = self.column.saturating_sub(1),
            b'\t' => self.put(b'\t'),
            // Bell, and the other C0 controls, mean nothing in a file.
            byte if byte < 0x20 || byte == 0x7f => {}
            byte => self.put(byte),
        }
    }

    fn put(&mut self, byte: u8) {
        if self.column < self.line.len() {
            self.line[self.column] = byte;
        } else {
            self.line.push(byte);
        }
        self.column += 1;
    }

    fn escape(&mut self, byte: u8) {
        self.state = match byte {
            b'[' => State::Csi,
            b']' => State::Osc,
            // DCS, SOS, PM, APC: all run until a string terminator.
            b'P' | b'X' | b'^' | b'_' => State::String,
            // Two-byte escapes - charset selection, keypad modes - are done.
            _ => State::Text,
        };
    }

    /// Whatever is still half-written, for when the recording stops.
    pub fn flush(&mut self) -> Vec<u8> {
        if self.line.is_empty() {
            return Vec::new();
        }
        let mut out = std::mem::take(&mut self.line);
        out.push(b'\n');
        self.column = 0;
        out
    }
}

/// A recording in progress: a file, and the state to write plain text into it.
pub struct Recorder {
    path: PathBuf,
    writer: BufWriter<std::fs::File>,
    transcriber: Transcriber,
    bytes: u64,
    lines: u64,
}

impl Recorder {
    /// Start writing to `path`. Fails rather than truncating something.
    pub fn create(path: &Path) -> std::io::Result<Recorder> {
        // `create_new` on the options rather than `File::create_new`, which
        // needs a newer compiler than this crate asks for.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Recorder {
            path: path.to_path_buf(),
            writer: BufWriter::new(file),
            transcriber: Transcriber::new(),
            bytes: 0,
            lines: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How much plain text has been written so far.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Take a slice of the pty stream. Errors are dropped on purpose: this
    /// runs on the reader thread, and a full disk must not take the shell
    /// down with it.
    ///
    /// Flushed on every chunk rather than when the buffer fills, so the file
    /// is worth `tail -f`-ing while the recording runs and shows its real
    /// size in the panel. One write per pty read is nothing next to the read.
    pub fn write(&mut self, bytes: &[u8]) {
        let text = self.transcriber.feed(bytes);
        if text.is_empty() {
            return;
        }
        self.bytes += text.len() as u64;
        self.lines += text.iter().filter(|&&b| b == b'\n').count() as u64;
        let _ = self.writer.write_all(&text);
        let _ = self.writer.flush();
    }

    /// Write the half-finished line and close the file.
    pub fn finish(mut self) -> PathBuf {
        let tail = self.transcriber.flush();
        if !tail.is_empty() {
            self.bytes += tail.len() as u64;
            self.lines += 1;
            let _ = self.writer.write_all(&tail);
        }
        let _ = self.writer.flush();
        self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcribe(chunks: &[&[u8]]) -> String {
        let mut transcriber = Transcriber::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(transcriber.feed(chunk));
        }
        out.extend(transcriber.flush());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_lines_pass_straight_through() {
        assert_eq!(transcribe(&[b"one\ntwo\n"]), "one\ntwo\n");
        // A line with no newline yet is held back until the flush, since a
        // carriage return could still rewrite it.
        assert_eq!(transcribe(&[b"half"]), "half\n");
    }

    #[test]
    fn colour_and_cursor_sequences_are_dropped() {
        // What a coloured `ls` actually emits.
        assert_eq!(
            transcribe(&[b"\x1b[0m\x1b[01;34mdir\x1b[0m  file\n"]),
            "dir  file\n"
        );
        // Cursor addressing, erase-line, and a window title.
        assert_eq!(
            transcribe(&[b"\x1b[2J\x1b[H\x1b]0;a title\x07hello\n"]),
            "hello\n"
        );
        // The other string terminator, ESC \.
        assert_eq!(transcribe(&[b"\x1b]0;title\x1b\\text\n"]), "text\n");
    }

    #[test]
    fn a_rewritten_line_lands_once_in_its_final_state() {
        // This is the whole reason the stream is not just copied to the file:
        // a progress bar would otherwise be forty lines of noise.
        assert_eq!(
            transcribe(&[b"  0%\r 50%\r100% done\n"]),
            "100% done\n",
            "the carriage returns rewrite the same line"
        );
        // Backspace rubs out, as it does on a terminal.
        assert_eq!(transcribe(&[b"cat\x08\x08ut\n"]), "cut\n");
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_understood() {
        // 8 KiB reads land wherever they land; a colour code straddling two
        // of them must not leak half of itself into the file.
        assert_eq!(
            transcribe(&[b"a\x1b[", b"01;32mb\n"]),
            "ab\n",
            "the CSI was split mid-sequence"
        );
        assert_eq!(transcribe(&[b"x\x1b", b"]0;t\x07y\n"]), "xy\n");
    }

    #[test]
    fn utf8_survives() {
        assert_eq!(transcribe(&[b"caf\xc3\xa9 \xe2\x9c\x93\n"]), "café ✓\n");
    }

    #[test]
    fn a_recording_writes_what_the_stream_said() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");

        let mut recorder = Recorder::create(&path).unwrap();
        recorder.write(b"\x1b[32mgreen\x1b[0m line\n");
        recorder.write(b"  0%\r100%\n");
        recorder.write(b"no newline yet");
        assert_eq!(recorder.lines(), 2);

        let written = recorder.finish();
        assert_eq!(written, path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "green line\n100%\nno newline yet\n"
        );
    }

    #[test]
    fn a_running_recording_is_readable_before_it_is_stopped() {
        // A recording that only appeared once it ended could not be watched,
        // and watching a long one is half the point.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.log");

        let mut recorder = Recorder::create(&path).unwrap();
        recorder.write(b"first line\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first line\n");
        recorder.write(b"second line\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first line\nsecond line\n"
        );
        recorder.finish();
    }

    #[test]
    fn a_recording_will_not_overwrite_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taken.log");
        std::fs::write(&path, "precious").unwrap();

        assert!(Recorder::create(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
    }
}
