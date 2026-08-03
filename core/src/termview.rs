// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a terminal screen looks like, in a shape that can cross a boundary.
//!
//! The emulator holds a grid of cells. A front-end drawing in the same process
//! reads that grid directly and paints it; one on the other side of a C ABI
//! cannot, and an 80x25 screen is two thousand cells - a hundred kilobytes of
//! JSON per frame to say what is overwhelmingly "this line is all one colour".
//!
//! So a screen crosses as **rows of styled runs**. A run is a stretch of
//! characters sharing every attribute, which is what a terminal line actually
//! is: a prompt, a path, a filename, an error. The same screen comes to
//! perhaps a hundred runs rather than two thousand cells, and the boundary's
//! rule - values cross as JSON, the front-end polls - survives intact.

use serde::Serialize;

/// A stretch of characters sharing every attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Run {
    /// Where it starts, in columns.
    pub col: u16,
    pub text: String,
    /// `#rrggbb`, or `None` for "whatever the front-end calls default".
    ///
    /// Left open on purpose. A terminal's default foreground is the panel's
    /// own colour, and the engine has no business naming it: the two
    /// front-ends have different chrome and both are right.
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Reverse video, already applied to `fg` and `bg`.
    ///
    /// Said as well as applied, because a front-end drawing its own cursor
    /// over a cell needs to know which way round the pair already is.
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub row: u16,
    pub runs: Vec<Run>,
}

/// The xterm 256-colour palette, as plain bytes.
///
/// The 216-colour cube and the grey ramp are arithmetic, fixed by the
/// standard. The first sixteen are not: they are a scheme, and this one is
/// picked to sit on dark chrome. It lives here so both front-ends draw a
/// shell's colours the same way - a `ls` that is green in one and lime in the
/// other would be two programs, not one with two windows.
pub fn xterm(index: u8) -> (u8, u8, u8) {
    match index {
        0 => (0x1D, 0x21, 0x28),
        1 => (0xE9, 0x6A, 0x7B),
        2 => (0x4F, 0xC1, 0x9A),
        3 => (0xF2, 0xB4, 0x4C),
        4 => (0x6E, 0x9E, 0xF5),
        5 => (0xB4, 0x86, 0xE8),
        6 => (0x5F, 0xC8, 0xE3),
        7 => (0xC8, 0xCE, 0xD8),
        8 => (0x5E, 0x67, 0x76),
        9 => (0xFF, 0x8B, 0x9A),
        10 => (0x6F, 0xE0, 0xB8),
        11 => (0xFF, 0xD1, 0x6B),
        12 => (0x8F, 0xBA, 0xFF),
        13 => (0xD0, 0xA6, 0xFF),
        14 => (0x8A, 0xE4, 0xFF),
        15 => (0xF2, 0xF5, 0xFA),
        // The 6x6x6 cube.
        16..=231 => {
            let value = index - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(value / 36), step((value / 6) % 6), step(value % 6))
        }
        // The greyscale ramp.
        _ => {
            let level = 8u8.saturating_add((index - 232).saturating_mul(10));
            (level, level, level)
        }
    }
}

fn colour(from: vt100::Color) -> Option<String> {
    match from {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        vt100::Color::Idx(index) => {
            let (r, g, b) = xterm(index);
            Some(format!("#{r:02x}{g:02x}{b:02x}"))
        }
    }
}

/// Cut a screen into rows of runs.
///
/// Rows with nothing on them are left out entirely, and so are blank cells
/// with no colour behind them: the front-end paints its own background there,
/// and sending a run of spaces to say "nothing here" would be most of the
/// screen most of the time.
pub fn rows_of(screen: &vt100::Screen) -> Vec<Row> {
    let (rows, cols) = screen.size();
    let mut out = Vec::new();
    for row in 0..rows {
        let mut runs: Vec<Run> = Vec::new();
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let contents = cell.contents();
            let blank = contents.is_empty();
            let (mut fg, mut bg) = (cell.fgcolor(), cell.bgcolor());
            if cell.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            if blank && bg == vt100::Color::Default && !cell.inverse() {
                continue;
            }
            let (fg, bg) = (colour(fg), colour(bg));
            let letter: &str = if blank { " " } else { &contents };

            // Joined onto the run before it only when it is adjacent *and*
            // identical. Adjacency matters because skipped blanks leave gaps,
            // and a run that swallowed one would draw its text a column left
            // of where it belongs.
            let joins = runs.last().is_some_and(|run| {
                run.col as usize + run.text.chars().count() == col as usize
                    && run.fg == fg
                    && run.bg == bg
                    && run.bold == cell.bold()
                    && run.italic == cell.italic()
                    && run.underline == cell.underline()
                    && run.inverse == cell.inverse()
            });
            if joins {
                runs.last_mut().expect("just checked").text.push_str(letter);
            } else {
                runs.push(Run {
                    col,
                    text: letter.to_string(),
                    fg,
                    bg,
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                });
            }
        }
        if !runs.is_empty() {
            out.push(Row { row, runs });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_of(bytes: &[u8], rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(bytes);
        parser
    }

    #[test]
    fn plain_text_is_one_run() {
        let parser = screen_of(b"hello", 3, 20);
        let rows = rows_of(parser.screen());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].runs.len(), 1);
        assert_eq!(rows[0].runs[0].text, "hello");
        assert_eq!(rows[0].runs[0].col, 0);
        // Nothing said about colour: the front-end's default is the answer.
        assert_eq!(rows[0].runs[0].fg, None);
    }

    #[test]
    fn a_colour_change_starts_a_new_run() {
        // "ab" plain, then red "cd".
        let parser = screen_of(b"ab\x1b[31mcd", 3, 20);
        let rows = rows_of(parser.screen());
        assert_eq!(rows[0].runs.len(), 2, "{rows:?}");
        assert_eq!(rows[0].runs[0].text, "ab");
        assert_eq!(rows[0].runs[1].text, "cd");
        assert_eq!(rows[0].runs[1].col, 2);
        assert!(rows[0].runs[1].fg.is_some());
    }

    #[test]
    fn a_screen_of_one_colour_is_a_run_a_line_and_not_two_thousand_cells() {
        // The whole reason this module exists: an 80x25 screen full of text
        // is eighty runs and not two thousand.
        let mut bytes = Vec::new();
        for _ in 0..25 {
            bytes.extend_from_slice(&b"x".repeat(80));
        }
        let parser = screen_of(&bytes, 25, 80);
        let rows = rows_of(parser.screen());
        assert_eq!(rows.len(), 25);
        assert!(
            rows.iter().all(|row| row.runs.len() == 1),
            "each full line of one style should be one run"
        );
    }

    #[test]
    fn empty_lines_are_left_out_altogether() {
        let parser = screen_of(b"only the first", 10, 20);
        let rows = rows_of(parser.screen());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row, 0);
    }

    #[test]
    fn a_gap_does_not_shift_what_follows_it() {
        // Write, jump right past some blanks, write again. The second run
        // must carry its own column, or it would be drawn against the first.
        let parser = screen_of(b"ab\x1b[10Gcd", 3, 20);
        let rows = rows_of(parser.screen());
        assert_eq!(rows[0].runs.len(), 2, "{rows:?}");
        assert_eq!(rows[0].runs[0].col, 0);
        assert_eq!(rows[0].runs[1].col, 9, "column 10, one-based");
        assert_eq!(rows[0].runs[1].text, "cd");
    }

    #[test]
    fn reverse_video_comes_across_swapped_and_says_so() {
        let parser = screen_of(b"\x1b[7mlit", 3, 20);
        let rows = rows_of(parser.screen());
        let run = &rows[0].runs[0];
        assert!(run.inverse);
        // Swapped already, so a front-end that ignores `inverse` still draws
        // it the right way round.
        assert_eq!(run.text, "lit");
    }

    #[test]
    fn the_palette_is_the_standard_where_the_standard_says_so() {
        // The cube: index 16 is black, 231 is white.
        assert_eq!(xterm(16), (0, 0, 0));
        assert_eq!(xterm(231), (255, 255, 255));
        // And the ramp climbs.
        assert!(xterm(240).0 > xterm(232).0);
    }
}
