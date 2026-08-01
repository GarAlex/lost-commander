//! Drawing the terminal panel, and turning key presses into the bytes a
//! terminal sends.
//!
//! The grid comes straight from the emulator; this only paints cells and
//! translates input. Anything clever - completion, history, colour - is the
//! real shell's doing.

use eframe::egui::{self, Color32, CornerRadius, FontId, Rect, Vec2};

use rust_commander_core::pty::PtySession;

/// A monospace cell. Chosen so a standard 80-column shell fits a sane panel.
pub const CELL: Vec2 = Vec2::new(8.0, 16.0);
pub const FONT_SIZE: f32 = 13.0;

/// Translate a key press into what a terminal would send down the pty.
///
/// Returning `None` means "not ours" - the key falls through to the file
/// manager. Text itself arrives through egui's `Event::Text`, so this only
/// deals with the keys that have no character.
pub fn key_bytes(key: egui::Key, modifiers: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key;

    // Control codes: Ctrl-C is 0x03, Ctrl-D 0x04, and so on down the alphabet.
    if modifiers.ctrl || modifiers.command {
        let letter = match key {
            Key::A => Some(b'a'),
            Key::B => Some(b'b'),
            Key::C => Some(b'c'),
            Key::D => Some(b'd'),
            Key::E => Some(b'e'),
            Key::F => Some(b'f'),
            Key::G => Some(b'g'),
            Key::H => Some(b'h'),
            Key::I => Some(b'i'),
            Key::J => Some(b'j'),
            Key::K => Some(b'k'),
            Key::L => Some(b'l'),
            Key::M => Some(b'm'),
            Key::N => Some(b'n'),
            Key::O => Some(b'o'),
            Key::P => Some(b'p'),
            Key::Q => Some(b'q'),
            Key::R => Some(b'r'),
            Key::S => Some(b's'),
            Key::T => Some(b't'),
            Key::U => Some(b'u'),
            Key::V => Some(b'v'),
            Key::W => Some(b'w'),
            Key::X => Some(b'x'),
            Key::Y => Some(b'y'),
            Key::Z => Some(b'z'),
            _ => None,
        };
        if let Some(letter) = letter {
            return Some(vec![letter - b'a' + 1]);
        }
    }

    let bytes: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Tab => b"\t",
        Key::Backspace => b"\x7f",
        Key::Escape => b"\x1b",
        // Arrows and friends are escape sequences, not characters.
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Insert => b"\x1b[2~",
        Key::Delete => b"\x1b[3~",
        _ => return None,
    };
    Some(bytes.to_vec())
}

/// A movement through the scrollback, asked for by the keyboard or the wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Whole lines. Positive goes back into history.
    Lines(i64),
    /// Whole screens, keeping one line of overlap so nothing is stepped over.
    Pages(i64),
    /// The oldest line the session still holds.
    Top,
    /// The live screen, where the prompt is.
    Bottom,
}

/// Keys that move this panel's own view instead of reaching the shell.
///
/// Shift is what separates the two, and has since xterm: bare PageUp belongs
/// to whatever is running inside - `less`, an editor, a pager in `git log` -
/// so taking it here would break all of them. Shift-PageUp was never theirs.
pub fn scroll_command(key: egui::Key, modifiers: egui::Modifiers) -> Option<Scroll> {
    use egui::Key;

    if !modifiers.shift {
        return None;
    }
    match key {
        Key::PageUp => Some(Scroll::Pages(1)),
        Key::PageDown => Some(Scroll::Pages(-1)),
        Key::Home => Some(Scroll::Top),
        Key::End => Some(Scroll::Bottom),
        _ => None,
    }
}

/// Turn wheel movement into whole lines, keeping the remainder for next time.
///
/// The carry is what makes a trackpad work: its deltas are far smaller than a
/// row, so rounding each one on its own would floor every gesture to zero and
/// the screen would never move.
pub fn wheel_lines(carry: &mut f32, delta: f32) -> i64 {
    *carry += delta;
    let lines = (*carry / CELL.y).trunc();
    *carry -= lines * CELL.y;
    lines as i64
}

/// Carry out a [`Scroll`] against a session showing `rows` rows.
pub fn apply_scroll(session: &mut PtySession, scroll: Scroll, rows: u16) {
    match scroll {
        Scroll::Lines(lines) => {
            session.scroll_by(lines);
        }
        Scroll::Pages(pages) => {
            let page = (rows as i64 - 1).max(1);
            session.scroll_by(pages * page);
        }
        Scroll::Top => {
            session.scroll_to(usize::MAX);
        }
        Scroll::Bottom => session.scroll_to_bottom(),
    }
}

/// How many rows and columns fit in `size`.
pub fn grid_for(size: Vec2) -> (u16, u16) {
    let rows = (size.y / CELL.y).floor().max(1.0) as u16;
    let cols = (size.x / CELL.x).floor().max(1.0) as u16;
    (rows, cols)
}

fn to_colour(colour: vt100::Color, fallback: Color32) -> Color32 {
    match colour {
        vt100::Color::Default => fallback,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(index) => palette(index),
    }
}

/// The xterm 256-colour palette, as this front-end's colour type.
///
/// The values are [`rust_commander_core::termview::xterm`]'s. They live there because the
/// other front-end needs them too, and an `ls` that came out green in one
/// window and lime in the other would be two programs rather than one with
/// two windows.
fn palette(index: u8) -> Color32 {
    let (r, g, b) = rust_commander_core::termview::xterm(index);
    Color32::from_rgb(r, g, b)
}

/// Paint the emulator's grid into `rect`.
pub fn draw_screen(
    ui: &egui::Ui,
    rect: Rect,
    session: &PtySession,
    background: Color32,
    foreground: Color32,
    focused: bool,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(4), background);

    let scrolled_back = session.scrollback_offset();

    session.with_screen(|screen| {
        let (rows, cols) = screen.size();
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let contents = cell.contents();
                let has_background = cell.bgcolor() != vt100::Color::Default;
                if contents.is_empty() && !has_background {
                    continue;
                }

                let position = egui::pos2(
                    rect.min.x + col as f32 * CELL.x,
                    rect.min.y + row as f32 * CELL.y,
                );
                let cell_rect = Rect::from_min_size(position, CELL);

                // Reverse video swaps the pair, which is how selections and
                // some prompts are drawn.
                let (mut fg, mut bg) = (
                    to_colour(cell.fgcolor(), foreground),
                    to_colour(cell.bgcolor(), background),
                );
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }

                if bg != background {
                    painter.rect_filled(cell_rect, CornerRadius::ZERO, bg);
                }
                if !contents.is_empty() {
                    painter.text(
                        cell_rect.left_top(),
                        egui::Align2::LEFT_TOP,
                        contents,
                        FontId::monospace(FONT_SIZE),
                        if cell.bold() {
                            fg
                        } else {
                            fg.gamma_multiply(0.92)
                        },
                    );
                }
            }
        }

        // The cursor: solid when this panel has the keyboard, hollow when not,
        // which is the convention every terminal uses. Scrolled back it is not
        // drawn at all - the live cursor position means nothing up here, and
        // painting it would put a block in the middle of old output.
        if !screen.hide_cursor() && scrolled_back == 0 {
            let (row, col) = screen.cursor_position();
            let position = egui::pos2(
                rect.min.x + col as f32 * CELL.x,
                rect.min.y + row as f32 * CELL.y,
            );
            let cursor = Rect::from_min_size(position, CELL);
            if focused {
                painter.rect_filled(cursor, CornerRadius::ZERO, foreground.gamma_multiply(0.75));
            } else {
                painter.rect_stroke(
                    cursor,
                    CornerRadius::ZERO,
                    egui::Stroke::new(1.0, foreground.gamma_multiply(0.6)),
                    egui::StrokeKind::Inside,
                );
            }
        }
    });

    // Say so, and say how to get back. Without this a scrolled panel just
    // looks like a shell that has stopped responding.
    if scrolled_back > 0 {
        let label = format!(
            "scrolled back {scrolled_back} line{} - Shift+End for the prompt",
            if scrolled_back == 1 { "" } else { "s" }
        );
        let galley =
            painter.layout_no_wrap(label, FontId::proportional(11.0), super::theme::text());
        let padding = Vec2::new(8.0, 4.0);
        let pill = Rect::from_min_size(
            egui::pos2(
                rect.max.x - galley.size().x - padding.x * 2.0 - 8.0,
                rect.min.y + 6.0,
            ),
            galley.size() + padding * 2.0,
        );
        painter.rect_filled(pill, CornerRadius::same(4), super::theme::accent_dim());
        painter.galley(pill.min + padding, galley, super::theme::text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Key, Modifiers};

    #[test]
    fn control_codes_follow_the_alphabet() {
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL), Some(vec![0x03]));
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL), Some(vec![0x04]));
        assert_eq!(key_bytes(Key::A, Modifiers::CTRL), Some(vec![0x01]));
        assert_eq!(key_bytes(Key::Z, Modifiers::CTRL), Some(vec![0x1a]));
    }

    #[test]
    fn the_keys_with_no_character_send_their_sequences() {
        assert_eq!(key_bytes(Key::Enter, Modifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::Tab, Modifiers::NONE), Some(b"\t".to_vec()));
        assert_eq!(
            key_bytes(Key::Backspace, Modifiers::NONE),
            Some(b"\x7f".to_vec()),
            "terminals send DEL, not backspace"
        );
        // Arrows are escape sequences; this is what makes history recall work.
        assert_eq!(
            key_bytes(Key::ArrowUp, Modifiers::NONE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(Key::ArrowLeft, Modifiers::NONE),
            Some(b"\x1b[D".to_vec())
        );
    }

    #[test]
    fn ordinary_letters_are_left_to_the_text_event() {
        // Typing arrives as Event::Text, so a bare letter is not ours; taking
        // it here would double every character.
        assert_eq!(key_bytes(Key::A, Modifiers::NONE), None);
        assert_eq!(key_bytes(Key::Space, Modifiers::NONE), None);
    }

    #[test]
    fn the_grid_is_sized_by_whole_cells() {
        let (rows, cols) = grid_for(Vec2::new(800.0, 320.0));
        assert_eq!(cols, 100); // 800 / 8
        assert_eq!(rows, 20); //  320 / 16

        // A panel too small for one cell still reports a usable grid.
        let (rows, cols) = grid_for(Vec2::new(1.0, 1.0));
        assert_eq!((rows, cols), (1, 1));
    }

    #[test]
    fn shift_takes_the_paging_keys_and_nothing_else_does() {
        assert_eq!(
            scroll_command(Key::PageUp, Modifiers::SHIFT),
            Some(Scroll::Pages(1))
        );
        assert_eq!(
            scroll_command(Key::PageDown, Modifiers::SHIFT),
            Some(Scroll::Pages(-1))
        );
        assert_eq!(
            scroll_command(Key::Home, Modifiers::SHIFT),
            Some(Scroll::Top)
        );
        assert_eq!(
            scroll_command(Key::End, Modifiers::SHIFT),
            Some(Scroll::Bottom)
        );

        // Unshifted, these belong to whatever is running inside - less, vim,
        // a pager - and must still go down the pty untouched.
        assert_eq!(scroll_command(Key::PageUp, Modifiers::NONE), None);
        assert_eq!(
            key_bytes(Key::PageUp, Modifiers::NONE),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(scroll_command(Key::Home, Modifiers::NONE), None);
        // And an ordinary letter is never a scroll, shifted or not.
        assert_eq!(scroll_command(Key::A, Modifiers::SHIFT), None);
    }

    #[test]
    fn the_wheel_carries_its_remainder_between_events() {
        let mut carry = 0.0;
        // A trackpad's deltas are each far smaller than a row; rounding them
        // one at a time would floor every gesture to zero.
        assert_eq!(wheel_lines(&mut carry, 6.0), 0);
        assert_eq!(wheel_lines(&mut carry, 6.0), 0);
        assert_eq!(wheel_lines(&mut carry, 6.0), 1, "18 points is one 16pt row");

        // A wheel notch moves several rows at once.
        let mut carry = 0.0;
        assert_eq!(wheel_lines(&mut carry, 53.0), 3);

        // Downward gestures go the other way and carry just the same.
        let mut carry = 0.0;
        assert_eq!(wheel_lines(&mut carry, -8.0), 0);
        assert_eq!(wheel_lines(&mut carry, -8.0), -1);
    }

    #[test]
    fn the_colour_cube_and_grey_ramp_are_in_range() {
        // Spot-check the corners of the 6x6x6 cube.
        assert_eq!(palette(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(palette(231), Color32::from_rgb(255, 255, 255));
        // The greyscale ramp climbs.
        assert!(palette(255).r() > palette(232).r());
    }
}
