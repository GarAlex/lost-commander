//! The byte editor window.
//!
//! Everything about *what* an edit is - a nibble typed over a byte, an undo, a
//! run written back in place - lives in [`rust_commander_core::hex`] and is tested without a
//! window. What is here is the grid, the cursor, and the keys.
//!
//! The rule this inherits and must not break: a hex editor **overwrites**. It
//! never inserts and never deletes, because inserting one byte moves every
//! byte after it, and that turns a two-byte fix to a header into a rewrite of
//! the file. The length going out is the length that came in.
//!
//! Nothing holds the file. Rows sit at fixed offsets, so only the ones on
//! screen are ever read and the pending changes are laid over them on the way
//! past - which is what lets this open a four-gigabyte file as fast as a small
//! one and still show four edited bytes in the middle of it.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use rust_commander_core::hex::{self, Cursor, Dump, Edits, Pane};

use super::theme;

/// What the editor is being asked to do once the frame is over.
pub enum Outcome {
    Nothing,
    Close,
    /// Put the pending changes back on disk.
    Write,
}

/// One file open as bytes.
pub struct Session {
    pub dump: Dump,
    pub cursor: Cursor,
    pub edits: Edits,
    /// Overwriting is asked about once, then done.
    confirming: bool,
    /// Set by a key, cleared once the list has been told to scroll there.
    pub follow: bool,
}

impl Session {
    pub fn new(dump: Dump) -> Session {
        Session {
            dump,
            cursor: Cursor::default(),
            edits: Edits::default(),
            confirming: false,
            follow: true,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.dump.path
    }

    /// The byte the file has at an offset, before any pending change.
    fn on_disk(&self, at: u64) -> u8 {
        self.dump
            .read(at / hex::PER_ROW as u64, 1)
            .ok()
            .and_then(|rows| {
                rows.first()?
                    .bytes
                    .get((at % hex::PER_ROW as u64) as usize)
                    .copied()
            })
            .unwrap_or(0)
    }

    /// What is there now: the pending change if any, else the file.
    fn current(&self, at: u64) -> u8 {
        self.edits.get(at).unwrap_or_else(|| self.on_disk(at))
    }

    fn type_into(&mut self, character: char) {
        let at = self.cursor.at;
        let (was, current) = (self.on_disk(at), self.current(at));
        match self.cursor.pane {
            Pane::Hex => {
                let Some(digit) = hex::hex_digit(character) else {
                    return;
                };
                let now = hex::with_nibble(current, digit, self.cursor.low);
                self.edits.set(at, was, now);
                // Half a byte at a time, on to the next once both halves are
                // in - so 0x4f is `4` then `f` rather than an unreachable
                // value only a full-byte editor could not express.
                if self.cursor.low {
                    self.cursor.step(1, self.dump.size);
                } else {
                    self.cursor.low = true;
                }
            }
            Pane::Text => {
                if !character.is_control() && character.is_ascii() {
                    self.edits.set(at, was, character as u8);
                    self.cursor.step(1, self.dump.size);
                }
            }
        }
        self.confirming = false;
        self.follow = true;
    }
}

pub fn draw(ctx: &egui::Context, session: &mut Session) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let mut closed = false;
    let name = session
        .path()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    keys(ctx, session, &mut closed);

    let escaped = super::modal(ctx, "Edit bytes", |ui| {
        ui.set_min_width(720.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(session.path().display().to_string())
                    .size(11.0)
                    .monospace()
                    .color(theme::text_dim()),
            );
            ui.label(
                RichText::new(rust_commander_core::entry::size_in_words(session.dump.size))
                    .size(11.0)
                    .color(theme::text_faint()),
            );
        });
        ui.add_space(6.0);
        grid(ui, session);
        ui.add_space(6.0);
        outcome = buttons(ui, session, &name, &mut closed);
    });

    if escaped || closed {
        return Outcome::Close;
    }
    outcome
}

/// The keys, read before the window is drawn so a cursor move is on screen in
/// the same frame that caused it.
fn keys(ctx: &egui::Context, session: &mut Session, closed: &mut bool) {
    let size = session.dump.size;
    let per_row = hex::PER_ROW as i64;
    let events = ctx.input(|input| input.events.clone());

    for event in events {
        match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let step = |session: &mut Session, by: i64| {
                    session.cursor.step(by, size);
                    session.follow = true;
                };
                match key {
                    egui::Key::ArrowLeft => step(session, -1),
                    egui::Key::ArrowRight => step(session, 1),
                    egui::Key::ArrowUp => step(session, -per_row),
                    egui::Key::ArrowDown => step(session, per_row),
                    egui::Key::PageUp => step(session, -per_row * 16),
                    egui::Key::PageDown => step(session, per_row * 16),
                    egui::Key::Home => {
                        session.cursor.to(0, size);
                        session.follow = true;
                    }
                    egui::Key::End => {
                        session.cursor.to(size.saturating_sub(1), size);
                        session.follow = true;
                    }
                    egui::Key::Tab => {
                        session.cursor.pane = session.cursor.pane.other();
                        session.cursor.low = false;
                    }
                    egui::Key::Backspace => {
                        if let Some(at) = session.edits.undo() {
                            session.cursor.to(at, size);
                            session.follow = true;
                        }
                        session.confirming = false;
                    }
                    egui::Key::Escape => *closed = true,
                    // Ctrl-S is what every editor uses; F2 is what the
                    // terminal view uses, and neither costs the other
                    // anything.
                    egui::Key::S if modifiers.ctrl || modifiers.command => {
                        session.confirming = !session.edits.is_empty();
                    }
                    egui::Key::F2 => session.confirming = !session.edits.is_empty(),
                    _ => {}
                }
            }
            // Typing, which in the hex column means hex digits only. Taken
            // from the text events rather than the key ones so the keyboard
            // layout decides what a key produces, as it does everywhere else.
            egui::Event::Text(text) => {
                for character in text.chars() {
                    session.type_into(character);
                }
            }
            _ => {}
        }
    }
}

/// The dump itself: offset, sixteen bytes, and the same bytes as characters.
fn grid(ui: &mut egui::Ui, session: &mut Session) {
    let font = egui::FontId::monospace(12.0);
    let row_height = ui.fonts(|f| f.row_height(&font)) + 2.0;
    let total = session.dump.rows() as usize;
    let width = session.dump.offset_width();
    // Taken before the closure, which needs `session` mutably.
    let (cursor, follow) = (session.cursor, std::mem::take(&mut session.follow));

    let mut area = egui::ScrollArea::vertical()
        .id_salt("hex_rows")
        .max_height(420.0);
    if follow {
        // Keep the cursor in view however it moved - including the jump
        // Backspace makes back to wherever the undone change was.
        area = area.vertical_scroll_offset((cursor.row() as f32 * row_height - 200.0).max(0.0));
    }

    area.show_rows(ui, row_height, total, |ui, shown| {
        let from = shown.start as u64;
        let mut rows = session.dump.read(from, shown.len()).unwrap_or_default();
        for row in &mut rows {
            session.edits.overlay(row);
        }

        let mut clicked: Option<(u64, Pane)> = None;
        for row in &rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(
                    RichText::new(format!("{:0width$x}  ", row.offset, width = width))
                        .font(font.clone())
                        .color(theme::text_faint()),
                );
                for at in 0..hex::PER_ROW {
                    let offset = row.offset + at as u64;
                    let here = offset == cursor.at && cursor.pane == Pane::Hex;
                    let text = RichText::new(row.pair(at))
                        .font(font.clone())
                        .color(ink(session, offset, here));
                    if cell(ui, text, here).clicked() {
                        clicked = Some((offset, Pane::Hex));
                    }
                    ui.label(
                        RichText::new(if at + 1 == hex::PER_ROW / 2 {
                            "   "
                        } else {
                            " "
                        })
                        .font(font.clone()),
                    );
                }

                ui.label(
                    RichText::new(" |")
                        .font(font.clone())
                        .color(theme::text_faint()),
                );
                for (at, byte) in row.bytes.iter().enumerate() {
                    let offset = row.offset + at as u64;
                    let here = offset == cursor.at && cursor.pane == Pane::Text;
                    let text = RichText::new(hex::printable(*byte).to_string())
                        .font(font.clone())
                        .color(ink(session, offset, here));
                    if cell(ui, text, here).clicked() {
                        clicked = Some((offset, Pane::Text));
                    }
                }
                ui.label(
                    RichText::new("|")
                        .font(font.clone())
                        .color(theme::text_faint()),
                );
            });
        }

        if let Some((at, pane)) = clicked {
            session.cursor.to(at, session.dump.size);
            session.cursor.pane = pane;
        }
    });
}

/// What colour a byte is drawn in: where the cursor is, what has been changed,
/// and everything else. Three states, three answers.
fn ink(session: &Session, at: u64, here: bool) -> egui::Color32 {
    if here {
        theme::marked_text()
    } else if session.edits.is_changed(at) {
        theme::ok()
    } else {
        theme::text()
    }
}

/// One byte, clickable, with the cursor's own background behind it.
fn cell(ui: &mut egui::Ui, text: RichText, here: bool) -> egui::Response {
    let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
    if here {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            2.0,
            egui::Stroke::new(1.0, theme::accent()),
            egui::StrokeKind::Middle,
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    response
}

fn buttons(ui: &mut egui::Ui, session: &mut Session, name: &str, closed: &mut bool) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let pending = session.edits.len();

    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            *closed = true;
        }
        if session.confirming {
            ui.label(
                RichText::new(format!("Write {pending} byte(s) into {name}?"))
                    .size(11.5)
                    .color(theme::danger()),
            );
            if ui.button("Write").clicked() {
                session.confirming = false;
                outcome = Outcome::Write;
            }
            if ui.button("Not yet").clicked() {
                session.confirming = false;
            }
        } else {
            if ui
                .add_enabled(pending > 0, egui::Button::new("Write"))
                .on_hover_text("Put the changed bytes back, in place")
                .on_disabled_hover_text("Nothing has been changed")
                .clicked()
            {
                session.confirming = true;
            }
            if ui
                .add_enabled(pending > 0, egui::Button::new("Undo"))
                .clicked()
            {
                if let Some(at) = session.edits.undo() {
                    session.cursor.to(at, session.dump.size);
                    session.follow = true;
                }
            }
        }

        ui.label(
            RichText::new(format!(
                "{:#x}  {} column",
                session.cursor.at,
                session.cursor.pane.label()
            ))
            .size(11.0)
            .monospace()
            .color(theme::text_dim()),
        );
        let changed = session.edits.describe();
        if !changed.is_empty() {
            ui.label(RichText::new(changed).size(11.0).color(theme::ok()));
        }
    });

    ui.label(
        RichText::new(
            "0-9 a-f type into the hex column, any character into the text one. \
             Tab swaps them, Backspace undoes. The file's length never changes.",
        )
        .size(10.5)
        .color(theme::text_faint()),
    );

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(bytes: &[u8]) -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, bytes).unwrap();
        let dump = Dump::open(&path).unwrap();
        (dir, Session::new(dump))
    }

    #[test]
    fn two_hex_digits_make_a_byte_and_move_on() {
        let (_dir, mut session) = session(&[0x00, 0x11, 0x22]);
        session.type_into('4');
        assert_eq!(session.edits.get(0), Some(0x40));
        assert_eq!(session.cursor.at, 0, "still on the same byte");
        assert!(session.cursor.low);

        session.type_into('f');
        assert_eq!(session.edits.get(0), Some(0x4f));
        assert_eq!(session.cursor.at, 1, "and on to the next");
        assert!(!session.cursor.low);
    }

    #[test]
    fn a_character_typed_in_the_text_column_is_one_byte() {
        let (_dir, mut session) = session(b"hello");
        session.cursor.pane = Pane::Text;
        session.type_into('H');
        assert_eq!(session.edits.get(0), Some(b'H'));
        assert_eq!(session.cursor.at, 1);

        // Anything that is not a byte is not typed rather than being mangled
        // into one.
        session.type_into('\u{20AC}');
        assert_eq!(session.edits.len(), 1);
    }

    #[test]
    fn a_digit_that_is_not_hex_does_nothing_in_the_hex_column() {
        let (_dir, mut session) = session(&[0xab]);
        session.type_into('z');
        assert!(session.edits.is_empty());
        assert_eq!(session.cursor.at, 0);
    }

    #[test]
    fn what_is_typed_lies_over_the_file_without_touching_it() {
        let (_dir, mut session) = session(b"hello world");
        session.type_into('4');
        session.type_into('8'); // 'H'
        assert_eq!(session.current(0), b'H');
        assert_eq!(session.on_disk(0), b'h', "the file is untouched");

        let mut row = session.dump.read(0, 1).unwrap().remove(0);
        session.edits.overlay(&mut row);
        assert_eq!(&row.bytes[..5], b"Hello");
    }
}
