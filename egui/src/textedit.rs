//! The text editor window.
//!
//! Everything about what the bytes mean is in [`rust_commander_core::textedit`] and
//! [`rust_commander_core::encoding`], tested without a window. What is here is the box you
//! type into and the two choosers either side of it.
//!
//! Two choosers, not one, because they answer different questions. **Read as**
//! re-reads the same bytes a different way, which is free and reversible and
//! is how you rescue a file the guess got wrong. **Save as** is what to write
//! back, which converts the file and can lose characters the target has no
//! room for. A single box cannot express "this is CP1251 and I want UTF-8",
//! and that is the main reason anyone opens this window at all.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};

use rust_commander_core::encoding::{self, Encoding};
use rust_commander_core::textedit::Document;

use super::theme;

/// What the editor is being asked to do once the frame is over.
pub enum Outcome {
    Nothing,
    Close,
    /// Write to this path. The caller does it, so one place reports what
    /// happened.
    Write(PathBuf),
}

/// One file open in the editor.
pub struct Session {
    pub document: Document,
    /// Where "save as" would write, shown only once it has been asked for.
    save_as: Option<String>,
    /// Overwriting is asked about once, then done.
    confirming: bool,
    /// Re-reading throws away what has been typed, so that is asked about too.
    confirming_reread: Option<Encoding>,
    /// What the last save could not represent, said until something changes.
    pub lost: Vec<char>,
}

impl Session {
    pub fn new(document: Document) -> Session {
        Session {
            document,
            save_as: None,
            confirming: false,
            confirming_reread: None,
            lost: Vec::new(),
        }
    }
}

pub fn draw(ctx: &egui::Context, session: &mut Session) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let mut closed = false;
    let name = file_name(&session.document.path);
    let edited = session.document.is_edited();

    let escaped = super::modal(ctx, "Edit text", |ui| {
        ui.set_min_width(760.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(session.document.path.display().to_string())
                    .size(11.0)
                    .monospace()
                    .color(theme::text_dim()),
            );
            if edited {
                ui.label(
                    RichText::new("edited")
                        .size(11.0)
                        .color(theme::accent())
                        .italics(),
                );
            }
        });

        ui.add_space(6.0);
        encodings(ui, session);
        ui.add_space(6.0);
        box_(ui, session);
        ui.add_space(6.0);
        outcome = buttons(ui, session, &name, &mut closed);
    });

    if escaped || closed {
        return Outcome::Close;
    }
    outcome
}

/// The two choosers, the line endings, and what was detected.
fn encodings(ui: &mut egui::Ui, session: &mut Session) {
    let detected = session.document.detected;
    let mut reread: Option<Encoding> = None;

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("read as")
                .size(11.0)
                .color(theme::text_faint()),
        );
        let mut chosen = session.document.read_as;
        egui::ComboBox::from_id_salt("text_read_as")
            .selected_text(chosen.label())
            .width(140.0)
            .show_ui(ui, |ui| {
                for option in encoding::ALL {
                    ui.selectable_value(&mut chosen, option, option.label());
                }
            });
        if chosen != session.document.read_as {
            reread = Some(chosen);
        }

        ui.separator();
        ui.label(
            RichText::new("save as")
                .size(11.0)
                .color(theme::text_faint()),
        );
        egui::ComboBox::from_id_salt("text_write_as")
            .selected_text(session.document.write_as.label())
            .width(140.0)
            .show_ui(ui, |ui| {
                for option in encoding::ALL {
                    ui.selectable_value(&mut session.document.write_as, option, option.label());
                }
            });

        ui.separator();
        ui.label(
            RichText::new("lines end")
                .size(11.0)
                .color(theme::text_faint()),
        );
        egui::ComboBox::from_id_salt("text_newline")
            .selected_text(session.document.newline.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for option in encoding::NEWLINES {
                    ui.selectable_value(&mut session.document.newline, option, option.label());
                }
            });
    });

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("found: {}", detected.describe()))
                .size(10.5)
                .color(theme::text_faint()),
        );
        // A guess is worth saying out loud, because the way to fix a wrong one
        // is right there and costs nothing to try.
        if detected.confidence == encoding::Confidence::Guessed {
            ui.label(
                RichText::new("- if the text looks wrong, read it as something else")
                    .size(10.5)
                    .color(theme::text_faint()),
            );
        }
    });

    // Re-reading discards what has been typed, so it is asked about first.
    if let Some(encoding) = reread {
        if session.document.is_edited() {
            session.confirming_reread = Some(encoding);
        } else {
            session.document.read_again_as(encoding);
            session.lost.clear();
        }
    }
    if let Some(encoding) = session.confirming_reread {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "Reading it as {} again throws away what you have typed.",
                    encoding.label()
                ))
                .size(11.0)
                .color(theme::danger()),
            );
            if ui.button("Throw it away").clicked() {
                session.document.read_again_as(encoding);
                session.lost.clear();
                session.confirming_reread = None;
            }
            if ui.button("Keep typing").clicked() {
                session.confirming_reread = None;
            }
        });
    }
}

/// The box itself.
fn box_(ui: &mut egui::Ui, session: &mut Session) {
    egui::ScrollArea::vertical()
        .id_salt("text_body")
        .max_height(420.0)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut session.document.text)
                    .desired_width(f32::INFINITY)
                    .desired_rows(22)
                    .code_editor(),
            );
        });
}

fn buttons(ui: &mut egui::Ui, session: &mut Session, name: &str, closed: &mut bool) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let changes = session.document.would_change_the_file();
    // Worked out before the buttons, so "this will mangle six characters" is
    // on screen *before* Save is pressed rather than after.
    let would_lose = session
        .document
        .to_bytes()
        .complaint(session.document.write_as);

    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            *closed = true;
        }

        if session.confirming {
            ui.label(
                RichText::new(format!("Write over {name}?"))
                    .size(11.5)
                    .color(theme::danger()),
            );
            if ui.button("Overwrite").clicked() {
                session.confirming = false;
                outcome = Outcome::Write(session.document.path.clone());
            }
            if ui.button("Leave it").clicked() {
                session.confirming = false;
            }
        } else {
            if ui
                .add_enabled(changes, egui::Button::new("Save"))
                .on_hover_text("Write over the original")
                .on_disabled_hover_text("Nothing has changed")
                .clicked()
            {
                session.confirming = true;
            }
            if ui.button("Save as...").clicked() {
                session.save_as = Some(match &session.save_as {
                    Some(existing) => existing.clone(),
                    None => name.to_string(),
                });
            }
        }

        ui.label(
            RichText::new(format!(
                "{} lines, {}",
                session.document.lines(),
                rust_commander_core::entry::size_in_words(session.document.size() as u64)
            ))
            .size(11.0)
            .color(theme::text_faint()),
        );
    });

    // Taken out and put back, so the box can be edited without the closure
    // holding a borrow of the session it also writes to.
    if let Some(mut typed) = session.save_as.take() {
        let (mut cancelled, mut go) = (false, false);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("save as")
                    .size(11.0)
                    .color(theme::text_faint()),
            );
            let box_ = ui.add(
                egui::TextEdit::singleline(&mut typed)
                    .desired_width(300.0)
                    .font(egui::TextStyle::Monospace),
            );
            let entered = box_.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            go = ui.button("Write").clicked() || entered;
            cancelled = ui.button("Cancel").clicked();
        });

        let name = typed.trim().to_string();
        if go && !name.is_empty() {
            let target = Path::new(&name);
            let target = if target.is_absolute() {
                target.to_path_buf()
            } else {
                session
                    .document
                    .path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(target)
            };
            outcome = Outcome::Write(target);
        }
        if !cancelled {
            session.save_as = Some(typed);
        }
    }

    if let Some(complaint) = would_lose {
        ui.label(
            RichText::new(format!("{complaint} - they will be written as ?"))
                .size(10.5)
                .color(theme::danger()),
        );
    } else if !session.lost.is_empty() {
        let shown: String = session.lost.iter().take(8).collect();
        ui.label(
            RichText::new(format!("Saved, but these could not be written: {shown}"))
                .size(10.5)
                .color(theme::danger()),
        );
    }

    outcome
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_commander_core::textedit::Document;

    #[test]
    fn a_session_starts_settled() {
        let document = Document::from_bytes(PathBuf::from("/tmp/a.txt"), b"hi\n".to_vec());
        let session = Session::new(document);
        assert!(!session.document.is_edited());
        assert!(session.lost.is_empty());
        assert!(session.save_as.is_none());
        assert!(!session.confirming);
    }

    #[test]
    fn the_warning_about_what_will_not_fit_is_available_before_saving() {
        // The complaint is worked out from what a save *would* write, so it
        // can be shown while there is still time to change the encoding -
        // rather than reported afterwards, when the file is already question
        // marks.
        let mut document = Document::from_bytes(PathBuf::from("/tmp/a.txt"), b"hi\n".to_vec());
        document.text = "Привет\n".to_string();
        document.write_as = Encoding::Cp1252;
        let complaint = document.to_bytes().complaint(document.write_as);
        assert!(complaint.is_some());
        assert!(complaint.unwrap().contains("Windows-1252"));

        document.write_as = Encoding::Utf8;
        assert!(document.to_bytes().complaint(document.write_as).is_none());
    }
}
