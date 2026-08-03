// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Working out what a text file's bytes mean, and turning text back into them.
//!
//! Rust strings are UTF-8, and a file on disk is bytes. Most of the time those
//! are the same thing and none of this matters. The times it does matter are
//! the ones that ruin a day: a Windows-made `.txt` in CP1252 whose smart
//! quotes come out as replacement characters, a Cyrillic README in CP1251 that
//! is unreadable, a UTF-16 file from a PowerShell redirect that looks like
//! every other byte is a null.
//!
//! Two things are needed and neither is guessing for its own sake:
//!
//! * **Detection**, so opening a file usually just works.
//! * **Overriding it**, because detection cannot always be right and the
//!   person looking at the file can see what it should have been.
//!
//! Detection is deliberately conservative. A byte-order mark is proof; valid
//! UTF-8 is near enough to proof; past that it is a *guess*, and a guess that
//! presents itself as an answer is worse than one that admits it - so
//! [`sniff`] returns its confidence along with its verdict, and the front-ends
//! say which it was.
//!
//! The single-byte tables are the two that matter in practice: CP1252 for
//! western European text out of Windows, and CP1251 for Cyrillic. Latin-1 is
//! there as the identity mapping - every byte is the code point of the same
//! number - which is what makes it the one encoding that can round-trip
//! arbitrary bytes without loss.

/// How a file's bytes are to be read as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    /// UTF-8 with the byte-order mark Windows editors like to write.
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Every byte is the code point of the same number. Lossless both ways,
    /// which is what makes it the safe fallback.
    Latin1,
    /// Windows western European: Latin-1 with the 0x80..0x9F range filled in.
    Cp1252,
    /// Windows Cyrillic.
    Cp1251,
}

/// Every encoding, in the order a chooser offers them.
pub const ALL: [Encoding; 7] = [
    Encoding::Utf8,
    Encoding::Utf8Bom,
    Encoding::Utf16Le,
    Encoding::Utf16Be,
    Encoding::Cp1252,
    Encoding::Cp1251,
    Encoding::Latin1,
];

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 with BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "Latin-1",
            Encoding::Cp1252 => "Windows-1252",
            Encoding::Cp1251 => "Windows-1251",
        }
    }

    /// The bytes that go in front of the text, if any.
    pub fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8Bom => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            _ => &[],
        }
    }

    /// Whether every possible sequence of bytes reads as *something*.
    ///
    /// True for the single-byte encodings, which is what makes them a
    /// fallback that cannot fail rather than one that fails differently.
    pub fn accepts_anything(self) -> bool {
        matches!(self, Encoding::Latin1 | Encoding::Cp1252 | Encoding::Cp1251)
    }
}

/// How sure [`sniff`] is, which the front-ends show rather than hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// A byte-order mark said so. This is not a guess.
    Marked,
    /// The bytes are valid UTF-8 and there is enough of them to mean it.
    Certain,
    /// Nothing contradicted it, but a single-byte encoding never can be
    /// contradicted - so this is a guess, and says so.
    Guessed,
}

/// What the bytes appear to be, and how sure that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub encoding: Encoding,
    pub confidence: Confidence,
}

impl Detected {
    /// The words that go beside the encoding in the window.
    pub fn describe(&self) -> String {
        match self.confidence {
            Confidence::Marked => format!("{} (byte-order mark)", self.encoding.label()),
            Confidence::Certain => self.encoding.label().to_string(),
            Confidence::Guessed => format!("{} (a guess)", self.encoding.label()),
        }
    }
}

/// How much of the file detection looks at.
///
/// Enough to be sure, little enough that opening a gigabyte log does not mean
/// reading a gigabyte before the first line appears.
pub const SNIFF: usize = 64 * 1024;

/// What these bytes most likely are.
///
/// In order: a byte-order mark, which is proof; UTF-16 without one, which the
/// nulls give away; valid UTF-8, which is near enough to proof because
/// arbitrary bytes are overwhelmingly unlikely to be accidentally valid; and
/// then a guess between the single-byte tables.
pub fn sniff(bytes: &[u8]) -> Detected {
    let marked = |encoding| Detected {
        encoding,
        confidence: Confidence::Marked,
    };
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return marked(Encoding::Utf8Bom);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return marked(Encoding::Utf16Le);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return marked(Encoding::Utf16Be);
    }

    let looked_at = &bytes[..bytes.len().min(SNIFF)];
    if looked_at.is_empty() {
        return Detected {
            encoding: Encoding::Utf8,
            confidence: Confidence::Certain,
        };
    }

    // UTF-16 without a mark. Latin text in it is every other byte a null, and
    // which half the nulls fall in says which way round it is. Nulls are
    // otherwise vanishingly rare in text, so this is a strong signal.
    if let Some(encoding) = utf16_without_a_mark(looked_at) {
        return Detected {
            encoding,
            confidence: Confidence::Guessed,
        };
    }

    // Valid UTF-8 is the answer. Arbitrary bytes are overwhelmingly unlikely
    // to be accidentally valid, so this is as near proof as sniffing gets.
    if is_utf8_allowing_a_cut(looked_at) {
        return Detected {
            encoding: Encoding::Utf8,
            confidence: Confidence::Certain,
        };
    }

    // Not UTF-8. One of the single-byte tables, then - and since every one of
    // them accepts every byte, nothing can rule any of them out. Cyrillic text
    // in CP1251 is nearly all high bytes; western text in CP1252 is mostly
    // low ones with the occasional accent.
    let high = looked_at.iter().filter(|byte| **byte >= 0x80).count();
    let encoding = if high * 4 >= looked_at.len() {
        Encoding::Cp1251
    } else {
        Encoding::Cp1252
    };
    Detected {
        encoding,
        confidence: Confidence::Guessed,
    }
}

/// Whether these bytes are UTF-8, allowing for the window having cut a
/// character in half.
///
/// Sniffing looks at the first [`SNIFF`] bytes, and for a file of Chinese or
/// emoji that cut lands mid-character two times in three - so a plain
/// `from_utf8` would declare most large UTF-8 files invalid.
///
/// The error says which case it is, and the distinction is the whole thing:
/// `error_len()` of `None` means the input simply ran out mid-sequence, which
/// is the window's doing and not the file's. `Some` means a byte that cannot
/// be there at all, which is the answer. Trimming bytes off the end until it
/// parses would blur the two, and read `caf\xE9` as a truncated UTF-8 `caf`.
fn is_utf8_allowing_a_cut(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

/// UTF-16 with no byte-order mark, told by where the nulls fall.
fn utf16_without_a_mark(bytes: &[u8]) -> Option<Encoding> {
    if bytes.len() < 4 {
        return None;
    }
    let pairs = bytes.len() / 2;
    let (mut high_nulls, mut low_nulls) = (0usize, 0usize);
    for pair in bytes[..pairs * 2].chunks_exact(2) {
        if pair[0] == 0 {
            low_nulls += 1;
        }
        if pair[1] == 0 {
            high_nulls += 1;
        }
    }
    // Well over half of one side and almost none of the other. Anything less
    // lopsided is not UTF-16, it is a file that happens to contain some nulls.
    let strong = pairs * 3 / 4;
    if high_nulls >= strong && low_nulls * 8 < pairs {
        return Some(Encoding::Utf16Le);
    }
    if low_nulls >= strong && high_nulls * 8 < pairs {
        return Some(Encoding::Utf16Be);
    }
    None
}

/// Bytes to text.
///
/// Never fails. Where a byte cannot mean anything in the chosen encoding it
/// becomes U+FFFD, because a viewer that refuses to show a file with one bad
/// byte in it is a viewer that cannot show you the bad byte.
pub fn decode(bytes: &[u8], encoding: Encoding) -> String {
    let body = strip_bom(bytes, encoding);
    match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => String::from_utf8_lossy(body).into_owned(),
        Encoding::Utf16Le => decode_utf16(body, true),
        Encoding::Utf16Be => decode_utf16(body, false),
        Encoding::Latin1 => body.iter().map(|byte| *byte as char).collect(),
        Encoding::Cp1252 => body.iter().map(|byte| from_table(*byte, &CP1252)).collect(),
        Encoding::Cp1251 => body.iter().map(|byte| from_table(*byte, &CP1251)).collect(),
    }
}

/// The bytes after any mark this encoding carries.
pub fn strip_bom(bytes: &[u8], encoding: Encoding) -> &[u8] {
    let bom = encoding.bom();
    match !bom.is_empty() && bytes.starts_with(bom) {
        true => &bytes[bom.len()..],
        false => bytes,
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    let mut text = String::from_utf16_lossy(&units);
    // An odd number of bytes is a truncated file, not a reason to lose the
    // last one in silence.
    if bytes.len() % 2 == 1 {
        text.push('\u{FFFD}');
    }
    text
}

fn from_table(byte: u8, table: &[char; 128]) -> char {
    if byte < 0x80 {
        byte as char
    } else {
        table[byte as usize - 0x80]
    }
}

/// Text back to bytes.
///
/// Returns what could not be represented alongside the bytes: writing a file
/// back as CP1252 when it has picked up a character CP1252 has no room for is
/// a silent loss, and silent loss on save is the one thing an editor must
/// never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    /// Characters that had to be replaced, in the order they were met, with
    /// no repeats. Empty means nothing was lost.
    pub lost: Vec<char>,
}

impl Encoded {
    pub fn is_lossless(&self) -> bool {
        self.lost.is_empty()
    }

    /// What to warn with, or nothing where nothing was lost.
    pub fn complaint(&self, encoding: Encoding) -> Option<String> {
        if self.lost.is_empty() {
            return None;
        }
        let shown: String = self.lost.iter().take(8).collect();
        Some(format!(
            "{} cannot hold {} of these characters: {shown}{}",
            encoding.label(),
            self.lost.len(),
            if self.lost.len() > 8 { "..." } else { "" }
        ))
    }
}

/// The byte used where a character will not fit. `?` is what every other
/// encoder uses and what anyone reading the result will recognise.
const UNREPRESENTABLE: u8 = b'?';

pub fn encode(text: &str, encoding: Encoding) -> Encoded {
    let mut bytes = encoding.bom().to_vec();
    let mut lost: Vec<char> = Vec::new();
    let note = |lost: &mut Vec<char>, character: char| {
        if !lost.contains(&character) {
            lost.push(character);
        }
    };

    match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => bytes.extend_from_slice(text.as_bytes()),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let little = encoding == Encoding::Utf16Le;
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        Encoding::Latin1 => {
            for character in text.chars() {
                match u32::from(character) {
                    code if code < 0x100 => bytes.push(code as u8),
                    _ => {
                        note(&mut lost, character);
                        bytes.push(UNREPRESENTABLE);
                    }
                }
            }
        }
        Encoding::Cp1252 | Encoding::Cp1251 => {
            let table = if encoding == Encoding::Cp1252 {
                &CP1252
            } else {
                &CP1251
            };
            for character in text.chars() {
                match to_table(character, table) {
                    Some(byte) => bytes.push(byte),
                    None => {
                        note(&mut lost, character);
                        bytes.push(UNREPRESENTABLE);
                    }
                }
            }
        }
    }

    Encoded { bytes, lost }
}

fn to_table(character: char, table: &[char; 128]) -> Option<u8> {
    if (character as u32) < 0x80 {
        return Some(character as u8);
    }
    table
        .iter()
        .position(|entry| *entry == character)
        .map(|at| (at + 0x80) as u8)
}

/// How a file ends its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Newline {
    #[default]
    Lf,
    Crlf,
    /// Classic Mac. Vanishingly rare, and cheap to keep rather than mangle.
    Cr,
}

impl Newline {
    pub fn label(self) -> &'static str {
        match self {
            Newline::Lf => "LF",
            Newline::Crlf => "CRLF",
            Newline::Cr => "CR",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::Crlf => "\r\n",
            Newline::Cr => "\r",
        }
    }
}

/// Every line ending, in the order a chooser offers them.
pub const NEWLINES: [Newline; 3] = [Newline::Lf, Newline::Crlf, Newline::Cr];

/// How this text ends its lines, by majority.
///
/// A file is allowed to be inconsistent, and the answer for a mixed one is
/// whichever it mostly is - so saving does not convert the majority to match
/// a stray minority.
pub fn sniff_newline(text: &str) -> Newline {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => {
                crlf += 1;
                at += 2;
                continue;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        at += 1;
    }
    if crlf >= lf && crlf >= cr && crlf > 0 {
        Newline::Crlf
    } else if cr > lf && cr > 0 {
        Newline::Cr
    } else {
        Newline::Lf
    }
}

/// Every line ending in the text turned into one kind.
///
/// Editors work in `\n` internally - every text widget does - so this is what
/// puts a CRLF file back the way it was found.
pub fn to_newline(text: &str, newline: Newline) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    match newline {
        Newline::Lf => unified,
        other => unified.replace('\n', other.as_str()),
    }
}

/// The 0x80..0xFF half of Windows-1252.
#[rustfmt::skip]
const CP1252: [char; 128] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
    '\u{A0}', '\u{A1}', '\u{A2}', '\u{A3}', '\u{A4}', '\u{A5}', '\u{A6}', '\u{A7}',
    '\u{A8}', '\u{A9}', '\u{AA}', '\u{AB}', '\u{AC}', '\u{AD}', '\u{AE}', '\u{AF}',
    '\u{B0}', '\u{B1}', '\u{B2}', '\u{B3}', '\u{B4}', '\u{B5}', '\u{B6}', '\u{B7}',
    '\u{B8}', '\u{B9}', '\u{BA}', '\u{BB}', '\u{BC}', '\u{BD}', '\u{BE}', '\u{BF}',
    '\u{C0}', '\u{C1}', '\u{C2}', '\u{C3}', '\u{C4}', '\u{C5}', '\u{C6}', '\u{C7}',
    '\u{C8}', '\u{C9}', '\u{CA}', '\u{CB}', '\u{CC}', '\u{CD}', '\u{CE}', '\u{CF}',
    '\u{D0}', '\u{D1}', '\u{D2}', '\u{D3}', '\u{D4}', '\u{D5}', '\u{D6}', '\u{D7}',
    '\u{D8}', '\u{D9}', '\u{DA}', '\u{DB}', '\u{DC}', '\u{DD}', '\u{DE}', '\u{DF}',
    '\u{E0}', '\u{E1}', '\u{E2}', '\u{E3}', '\u{E4}', '\u{E5}', '\u{E6}', '\u{E7}',
    '\u{E8}', '\u{E9}', '\u{EA}', '\u{EB}', '\u{EC}', '\u{ED}', '\u{EE}', '\u{EF}',
    '\u{F0}', '\u{F1}', '\u{F2}', '\u{F3}', '\u{F4}', '\u{F5}', '\u{F6}', '\u{F7}',
    '\u{F8}', '\u{F9}', '\u{FA}', '\u{FB}', '\u{FC}', '\u{FD}', '\u{FE}', '\u{FF}',
];

/// The 0x80..0xFF half of Windows-1251.
#[rustfmt::skip]
const CP1251: [char; 128] = [
    '\u{0402}', '\u{0403}', '\u{201A}', '\u{0453}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20AC}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040A}', '\u{040C}', '\u{040B}', '\u{040F}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{98}', '\u{2122}', '\u{0459}', '\u{203A}', '\u{045A}', '\u{045C}', '\u{045B}', '\u{045F}',
    '\u{A0}', '\u{040E}', '\u{045E}', '\u{0408}', '\u{A4}', '\u{0490}', '\u{A6}', '\u{A7}',
    '\u{0401}', '\u{A9}', '\u{0404}', '\u{AB}', '\u{AC}', '\u{AD}', '\u{AE}', '\u{0407}',
    '\u{B0}', '\u{B1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{B5}', '\u{B6}', '\u{B7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{BB}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
    '\u{0410}', '\u{0411}', '\u{0412}', '\u{0413}', '\u{0414}', '\u{0415}', '\u{0416}', '\u{0417}',
    '\u{0418}', '\u{0419}', '\u{041A}', '\u{041B}', '\u{041C}', '\u{041D}', '\u{041E}', '\u{041F}',
    '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0424}', '\u{0425}', '\u{0426}', '\u{0427}',
    '\u{0428}', '\u{0429}', '\u{042A}', '\u{042B}', '\u{042C}', '\u{042D}', '\u{042E}', '\u{042F}',
    '\u{0430}', '\u{0431}', '\u{0432}', '\u{0433}', '\u{0434}', '\u{0435}', '\u{0436}', '\u{0437}',
    '\u{0438}', '\u{0439}', '\u{043A}', '\u{043B}', '\u{043C}', '\u{043D}', '\u{043E}', '\u{043F}',
    '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0444}', '\u{0445}', '\u{0446}', '\u{0447}',
    '\u{0448}', '\u{0449}', '\u{044A}', '\u{044B}', '\u{044C}', '\u{044D}', '\u{044E}', '\u{044F}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_is_proof_rather_than_a_guess() {
        for (encoding, bytes) in [
            (Encoding::Utf8Bom, vec![0xEF, 0xBB, 0xBF, b'h', b'i']),
            (Encoding::Utf16Le, vec![0xFF, 0xFE, b'h', 0, b'i', 0]),
            (Encoding::Utf16Be, vec![0xFE, 0xFF, 0, b'h', 0, b'i']),
        ] {
            let found = sniff(&bytes);
            assert_eq!(found.encoding, encoding);
            assert_eq!(found.confidence, Confidence::Marked);
            assert_eq!(decode(&bytes, found.encoding), "hi");
        }
    }

    #[test]
    fn plain_utf8_is_recognised_and_said_to_be_certain() {
        let found = sniff("hello, wörld — ok\n".as_bytes());
        assert_eq!(found.encoding, Encoding::Utf8);
        assert_eq!(found.confidence, Confidence::Certain);
        assert_eq!(found.describe(), "UTF-8");
    }

    #[test]
    fn utf16_without_a_mark_is_found_by_where_the_nulls_fall() {
        let little: Vec<u8> = "hello there"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let found = sniff(&little);
        assert_eq!(found.encoding, Encoding::Utf16Le);
        assert_eq!(decode(&little, found.encoding), "hello there");

        let big: Vec<u8> = "hello there"
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect();
        assert_eq!(sniff(&big).encoding, Encoding::Utf16Be);

        // A file that merely contains a null is not UTF-16.
        let mut mostly_text = b"a perfectly ordinary line of text".to_vec();
        mostly_text.push(0);
        assert_eq!(sniff(&mostly_text).encoding, Encoding::Utf8);
    }

    #[test]
    fn a_windows_file_that_is_not_utf8_is_guessed_and_says_so() {
        // "café" in CP1252: the é is a single byte, which is not valid UTF-8.
        let bytes = vec![b'c', b'a', b'f', 0xE9, b'\n'];
        let found = sniff(&bytes);
        assert_eq!(found.encoding, Encoding::Cp1252);
        assert_eq!(found.confidence, Confidence::Guessed);
        assert_eq!(found.describe(), "Windows-1252 (a guess)");
        assert_eq!(decode(&bytes, Encoding::Cp1252), "café\n");
    }

    #[test]
    fn cyrillic_is_mostly_high_bytes_and_reads_as_cyrillic() {
        // "Привет" in CP1251.
        let bytes = vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        let found = sniff(&bytes);
        assert_eq!(found.encoding, Encoding::Cp1251);
        assert_eq!(decode(&bytes, Encoding::Cp1251), "Привет");
    }

    #[test]
    fn a_bad_byte_at_the_end_is_a_bad_byte_and_not_a_cut() {
        // The distinction that matters. `caf\xE9\n` has a byte that cannot be
        // where it is - 0xE9 opens a three-byte sequence and a newline cannot
        // continue one - so the file is not UTF-8. Trimming bytes off the end
        // until it parses, which is the obvious way to allow for the window
        // cutting a character in half, reads this as a truncated "caf" and
        // calls the whole file UTF-8.
        assert!(!is_utf8_allowing_a_cut(&[b'c', b'a', b'f', 0xE9, b'\n']));
        // 0xFF cannot appear in UTF-8 at all, in any position.
        assert!(!is_utf8_allowing_a_cut(&[b'a', 0xFF, b'b']));

        // Whereas a sequence that simply ran out is a cut, and allowed. Note
        // that `caf\xE9` on its own is one of these: 0xE9 is a legitimate
        // opening byte, so ending there is indistinguishable from a window
        // that stopped one byte early. It is the *next* byte that decides.
        let character = "文".as_bytes();
        assert!(is_utf8_allowing_a_cut(&character[..2]));
        assert!(is_utf8_allowing_a_cut(character));
        assert!(is_utf8_allowing_a_cut(&[b'c', b'a', b'f', 0xE9]));
    }

    #[test]
    fn a_cut_in_the_middle_of_a_character_does_not_make_a_file_not_utf8() {
        // Sixty-four kilobytes of a three-byte character, so the sniff window
        // is guaranteed to land mid-sequence. Without trimming to a boundary
        // every such file is declared "not UTF-8" and read as CP1251.
        let text = "文".repeat(SNIFF);
        let found = sniff(text.as_bytes());
        assert_eq!(found.encoding, Encoding::Utf8);
        assert_eq!(found.confidence, Confidence::Certain);
    }

    #[test]
    fn nothing_at_all_is_utf8() {
        let found = sniff(b"");
        assert_eq!(found.encoding, Encoding::Utf8);
        assert_eq!(found.confidence, Confidence::Certain);
        assert_eq!(decode(b"", Encoding::Utf8), "");
    }

    #[test]
    fn every_encoding_round_trips_what_it_can_hold() {
        for encoding in ALL {
            let text = match encoding {
                Encoding::Cp1251 => "Привет, мир!\nline two\n",
                Encoding::Latin1 => "café ± ½\nline two\n",
                _ => "café — “quoted” ±\nline two\n",
            };
            let there = encode(text, encoding);
            assert!(
                there.is_lossless(),
                "{}: lost {:?}",
                encoding.label(),
                there.lost
            );
            assert_eq!(decode(&there.bytes, encoding), text, "{}", encoding.label());
        }
    }

    #[test]
    fn what_will_not_fit_is_reported_rather_than_lost_in_silence() {
        // Saving a file with Cyrillic in it as western European cannot work,
        // and doing it quietly is how an afternoon's work becomes question
        // marks.
        let there = encode("Привет", Encoding::Cp1252);
        assert!(!there.is_lossless());
        assert_eq!(there.bytes, b"??????");
        assert_eq!(there.lost, vec!['П', 'р', 'и', 'в', 'е', 'т']);
        let complaint = there.complaint(Encoding::Cp1252).expect("a complaint");
        assert!(complaint.contains("Windows-1252"), "{complaint}");
        assert!(complaint.contains('П'), "{complaint}");

        // And nothing to complain about where nothing was lost.
        assert!(encode("plain", Encoding::Cp1252)
            .complaint(Encoding::Cp1252)
            .is_none());
    }

    #[test]
    fn latin1_carries_any_byte_at_all_both_ways() {
        // The identity mapping, which is what makes it the safe fallback: a
        // file of arbitrary bytes comes back exactly as it went in.
        let bytes: Vec<u8> = (0u8..=255).collect();
        let text = decode(&bytes, Encoding::Latin1);
        assert_eq!(text.chars().count(), 256);
        let there = encode(&text, Encoding::Latin1);
        assert!(there.is_lossless());
        assert_eq!(there.bytes, bytes);
    }

    #[test]
    fn the_mark_is_written_and_read_back_off_again() {
        let there = encode("hi", Encoding::Utf8Bom);
        assert_eq!(there.bytes, vec![0xEF, 0xBB, 0xBF, b'h', b'i']);
        assert_eq!(decode(&there.bytes, Encoding::Utf8Bom), "hi");
        // And a file written without one does not grow a phantom character.
        assert_eq!(decode(b"hi", Encoding::Utf8), "hi");
    }

    #[test]
    fn a_truncated_utf16_file_keeps_its_last_byte_as_a_complaint() {
        let bytes = vec![b'h', 0, b'i'];
        assert_eq!(decode(&bytes, Encoding::Utf16Le), "h\u{FFFD}");
    }

    #[test]
    fn line_endings_are_found_by_majority_and_put_back() {
        assert_eq!(sniff_newline("a\nb\nc"), Newline::Lf);
        assert_eq!(sniff_newline("a\r\nb\r\nc"), Newline::Crlf);
        assert_eq!(sniff_newline("a\rb\rc"), Newline::Cr);
        assert_eq!(sniff_newline("no endings at all"), Newline::Lf);
        // Mixed: whichever it mostly is, so saving does not convert the
        // majority to match one stray line.
        assert_eq!(sniff_newline("a\r\nb\r\nc\nd"), Newline::Crlf);
        assert_eq!(sniff_newline("a\nb\nc\r\nd"), Newline::Lf);

        assert_eq!(to_newline("a\nb", Newline::Crlf), "a\r\nb");
        assert_eq!(to_newline("a\r\nb", Newline::Lf), "a\nb");
        assert_eq!(to_newline("a\rb", Newline::Crlf), "a\r\nb");
        // Idempotent: converting to what it already is changes nothing, and
        // in particular does not turn one CRLF into two.
        assert_eq!(to_newline("a\r\nb", Newline::Crlf), "a\r\nb");
    }

    #[test]
    fn a_file_read_and_written_back_unchanged_is_byte_for_byte_the_same() {
        // The property that matters for an editor: open, save, and the file
        // on disk has not moved. Checked for each encoding through the whole
        // round trip a save actually takes, line endings included.
        for encoding in ALL {
            let original = match encoding {
                Encoding::Cp1251 => "Первая строка\r\nвторая\r\n",
                _ => "first line\r\nsecond\r\n",
            };
            let bytes = encode(original, encoding).bytes;

            let found = sniff(&bytes);
            let read_as = if found.confidence == Confidence::Marked {
                found.encoding
            } else {
                encoding
            };
            let text = decode(&bytes, read_as);
            let endings = sniff_newline(&text);
            assert_eq!(endings, Newline::Crlf, "{}", encoding.label());

            let written = encode(&to_newline(&text, endings), read_as);
            assert!(written.is_lossless(), "{}", encoding.label());
            assert_eq!(written.bytes, bytes, "{}", encoding.label());
        }
    }
}
