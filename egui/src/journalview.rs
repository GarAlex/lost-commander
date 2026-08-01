//! The window that shows what was done.
//!
//! All of the arithmetic - what a day file holds, which records belong to
//! which run, what a filter keeps, when a day has aged out - is in
//! [`lost_commander_core::journal`] and tested without a window. What is here is the day
//! picker, the filter row, and the list.
//!
//! A day is read once, when the day or the stream changes, rather than every
//! frame: the file is on disk and the frame is sixty times a second.

use std::collections::HashSet;

use eframe::egui::{self, RichText};

use lost_commander_core::journal::{self, Day, Filter, Journal, Row, Shown};
#[cfg(test)]
use lost_commander_core::journal::{Kind, Stream};

use super::theme;

/// What the window is being asked to do once the frame is over.
pub enum Outcome {
    Nothing,
    Close,
    /// Show this file in the panes.
    GoTo(std::path::PathBuf),
    /// Throw the whole account away.
    Clear,
    /// Keep this many days from now on. Zero is for ever.
    Keep(u32),
}

/// The state of one open browser.
pub struct View {
    pub shown: Shown,
    /// The days that have records, newest first.
    pub days: Vec<Day>,
    /// Which of them is showing.
    pub at: usize,
    /// The day as read, before the filter.
    rows: Vec<Row>,
    pub filter: Filter,
    /// The runs whose files are shown rather than folded away.
    open: HashSet<u64>,
    /// The day the rows were read for, so a re-read happens when it changes
    /// and not once a frame.
    loaded: Option<(Shown, Day)>,
    /// Asked once before the account is thrown away.
    confirming: bool,
}

impl View {
    pub fn new(journal: &Journal) -> View {
        let shown = Shown::default();
        let mut view = View {
            shown,
            days: journal.days_shown(shown),
            at: 0,
            rows: Vec::new(),
            filter: Filter::default(),
            open: HashSet::new(),
            loaded: None,
            confirming: false,
        };
        view.reload(journal);
        view
    }

    pub fn day(&self) -> Option<Day> {
        self.days.get(self.at).copied()
    }

    /// Read the day being shown, if it is not the one already in hand.
    fn reload(&mut self, journal: &Journal) {
        let Some(day) = self.day() else {
            self.rows.clear();
            self.loaded = None;
            return;
        };
        if self.loaded == Some((self.shown, day)) {
            return;
        }
        self.rows = journal::arrange(journal.read_shown(self.shown, day));
        self.loaded = Some((self.shown, day));
    }

    /// Switch view, keeping the day being looked at if it has anything.
    ///
    /// Losing the date on every click would make comparing "what did I do"
    /// against "what did I run" on the same afternoon a matter of setting the
    /// date again each time.
    fn show(&mut self, journal: &Journal, shown: Shown) {
        if self.shown == shown {
            return;
        }
        let was = self.day();
        self.shown = shown;
        self.days = journal.days_shown(shown);
        self.at = was
            .and_then(|day| self.days.iter().position(|&d| d == day))
            .unwrap_or(0);
        self.loaded = None;
    }
}

pub fn draw(ctx: &egui::Context, view: &mut View, journal: &Journal) -> Outcome {
    let mut outcome = Outcome::Nothing;
    let mut closed = false;
    view.reload(journal);
    let showing = view.filter.apply(view.rows.clone());
    let counted = journal::tally(&showing);

    let escaped = super::modal(ctx, "What was done", |ui| {
        ui.set_min_width(860.0);
        streams_and_days(ui, view, journal);
        ui.add_space(4.0);
        filters(ui, view);
        ui.add_space(6.0);

        ui.label(
            RichText::new(match view.day() {
                None => "nothing recorded yet".to_string(),
                Some(day) => format!(
                    "{} - {} entr{}, {} file(s){}",
                    day.describe(),
                    counted.rows,
                    if counted.rows == 1 { "y" } else { "ies" },
                    counted.items,
                    match counted.failures {
                        0 => String::new(),
                        n => format!(", {n} that did not work"),
                    }
                ),
            })
            .size(11.0)
            .color(if counted.failures > 0 {
                theme::danger()
            } else {
                theme::text_dim()
            }),
        );
        ui.add_space(4.0);

        if let Some(go) = list(ui, view, &showing) {
            outcome = Outcome::GoTo(go);
        }

        ui.add_space(6.0);
        if let Some(asked) = footer(ui, view, journal, &mut closed) {
            outcome = asked;
        }
    });

    if escaped || closed {
        return Outcome::Close;
    }
    outcome
}

/// Which stream, and which day of it.
fn streams_and_days(ui: &mut egui::Ui, view: &mut View, journal: &Journal) {
    ui.horizontal(|ui| {
        for shown in journal::SHOWN {
            let here = view.shown == shown;
            if ui.selectable_label(here, shown.label()).clicked() {
                view.show(journal, shown);
            }
        }

        ui.separator();
        // Newer is up the list, so the arrows point the way the dates go
        // rather than the way the index does.
        let newer = ui.add_enabled(view.at > 0, egui::Button::new("\u{2039}"));
        if newer.on_hover_text("A later day").clicked() {
            view.at -= 1;
        }
        let older = ui.add_enabled(view.at + 1 < view.days.len(), egui::Button::new("\u{203A}"));
        if older.on_hover_text("An earlier day").clicked() {
            view.at += 1;
        }

        let chosen = view
            .day()
            .map(|day| day.describe())
            .unwrap_or_else(|| "no records".to_string());
        egui::ComboBox::from_id_salt("journal_day")
            .selected_text(chosen)
            .width(190.0)
            .show_ui(ui, |ui| {
                for (index, day) in view.days.clone().into_iter().enumerate() {
                    ui.selectable_value(&mut view.at, index, day.describe());
                }
            });
    });
}

/// The kinds, the failures-only switch, and the name box.
fn filters(ui: &mut egui::Ui, view: &mut View) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("show").size(11.0).color(theme::text_faint()));
        for kind in journal::KINDS {
            // Each stream only holds its own kinds; offering the rest would
            // be offering ways to see nothing.
            if !view.shown.holds(kind) {
                continue;
            }
            let on = view.filter.has(kind);
            if ui.selectable_label(on, kind.short()).clicked() {
                view.filter.toggle(kind);
            }
        }
        if !view.filter.kinds.is_empty() && ui.small_button("all").clicked() {
            view.filter.kinds.clear();
        }

        ui.separator();
        ui.checkbox(&mut view.filter.failures_only, "only what failed");
        ui.separator();
        ui.label(RichText::new("name").size(11.0).color(theme::text_faint()));
        ui.add(
            egui::TextEdit::singleline(&mut view.filter.text)
                .desired_width(180.0)
                .hint_text("anything on the line"),
        );
    });
}

/// The rows themselves.
fn list(ui: &mut egui::Ui, view: &mut View, rows: &[Row]) -> Option<std::path::PathBuf> {
    let mut go = None;
    egui::ScrollArea::vertical()
        .id_salt("journal_rows")
        .max_height(420.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.label(
                    RichText::new("Nothing here matches.")
                        .size(11.5)
                        .color(theme::text_faint()),
                );
                return;
            }
            for row in rows {
                match row {
                    Row::One(event) => {
                        if let Some(path) = event_line(ui, event, 0.0) {
                            go = Some(path);
                        }
                    }
                    Row::Run {
                        group,
                        events,
                        took,
                    } => {
                        let open = view.open.contains(&group.id);
                        ui.horizontal(|ui| {
                            let arrow = if open { "\u{25BE}" } else { "\u{25B8}" };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("{arrow} {}", group.summary))
                                            .size(11.5)
                                            .strong(),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                if open {
                                    view.open.remove(&group.id);
                                } else {
                                    view.open.insert(group.id);
                                }
                            }
                            let failures = events.iter().filter(|e| e.is_failure()).count();
                            ui.label(
                                RichText::new(format!(
                                    "{}  {} file(s){}{}",
                                    journal::clock(group.at),
                                    events.len(),
                                    match failures {
                                        0 => String::new(),
                                        n => format!(", {n} failed"),
                                    },
                                    // A run's total is worth having where a
                                    // per-file duration would be noise. Its
                                    // absence means the run never reached its
                                    // end.
                                    match took {
                                        Some(ms) => format!("  {}", journal::took(*ms)),
                                        None => String::new(),
                                    }
                                ))
                                .size(10.5)
                                .color(if failures > 0 {
                                    theme::danger()
                                } else {
                                    theme::text_faint()
                                }),
                            );
                        });
                        if open {
                            for event in events {
                                if let Some(path) = event_line(ui, event, 18.0) {
                                    go = Some(path);
                                }
                            }
                            if events.len() >= journal::MAX_EVENTS_PER_GROUP {
                                ui.horizontal(|ui| {
                                    ui.add_space(18.0);
                                    ui.label(
                                        RichText::new(
                                            "the first few thousand - the run kept going, \
                                             the record stopped naming them",
                                        )
                                        .size(10.5)
                                        .color(theme::text_faint()),
                                    );
                                });
                            }
                        }
                    }
                }
            }
        });
    go
}

/// One file, one line. Clicking it goes there.
fn event_line(
    ui: &mut egui::Ui,
    event: &journal::Event,
    indent: f32,
) -> Option<std::path::PathBuf> {
    let mut go = None;
    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.label(
            RichText::new(journal::clock(event.at))
                .size(10.5)
                .monospace()
                .color(theme::text_faint()),
        );
        // The shell's name where there is one: "Command" on every line of a
        // list of commands says nothing, and which shell ran it is the thing
        // that is not otherwise recoverable.
        ui.label(
            RichText::new(event.label())
                .size(10.5)
                .color(if event.kind.is_destructive() {
                    theme::danger()
                } else {
                    theme::text_dim()
                }),
        );

        let ink = if event.is_failure() {
            theme::danger()
        } else {
            theme::text()
        };
        let path = ui.add(
            egui::Label::new(RichText::new(&event.path).size(11.0).monospace().color(ink))
                .sense(egui::Sense::click()),
        );
        if path.on_hover_text("Show this in the pane").clicked() {
            go = Some(std::path::PathBuf::from(&event.path));
        }

        if let Some(to) = &event.to {
            ui.label(
                RichText::new("\u{2192}")
                    .size(10.5)
                    .color(theme::text_faint()),
            );
            let target = ui.add(
                egui::Label::new(RichText::new(to).size(11.0).monospace().color(ink))
                    .sense(egui::Sense::click()),
            );
            if target.on_hover_text("Show this in the pane").clicked() {
                go = Some(std::path::PathBuf::from(to));
            }
        }
        if !event.note.is_empty() {
            ui.label(
                RichText::new(&event.note)
                    .size(10.5)
                    .color(theme::text_faint()),
            );
        }
        if let Some(why) = &event.failed {
            ui.label(RichText::new(why).size(10.5).color(theme::danger()));
        }
        if let Some(ms) = event.ms {
            ui.label(
                RichText::new(journal::took(ms))
                    .size(10.5)
                    .color(theme::text_faint()),
            );
        }
    });
    go
}

/// Retention, clearing, and the way out.
fn footer(
    ui: &mut egui::Ui,
    view: &mut View,
    journal: &Journal,
    closed: &mut bool,
) -> Option<Outcome> {
    let mut outcome = None;
    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            *closed = true;
        }

        ui.separator();
        ui.label(RichText::new("keep").size(11.0).color(theme::text_faint()));
        // The handful anyone actually wants, rather than a number box: this
        // is a decision, not a measurement.
        for days in [7u32, 30, 90, 0] {
            let here = journal.keep.0 == days;
            let label = match days {
                0 => "for ever".to_string(),
                n => format!("{n} days"),
            };
            if ui.selectable_label(here, label).clicked() && !here {
                outcome = Some(Outcome::Keep(days));
            }
        }

        ui.separator();
        if view.confirming {
            ui.label(
                RichText::new("Throw the whole account away?")
                    .size(11.0)
                    .color(theme::danger()),
            );
            if ui.button("Clear it").clicked() {
                view.confirming = false;
                outcome = Some(Outcome::Clear);
            }
            if ui.button("Keep it").clicked() {
                view.confirming = false;
            }
        } else if ui
            .add_enabled(!view.days.is_empty(), egui::Button::new("Clear..."))
            .clicked()
        {
            view.confirming = true;
        }
    });

    ui.label(
        RichText::new(format!(
            "Kept {} in {}",
            journal.keep.describe(),
            journal.dir().display()
        ))
        .size(10.5)
        .color(theme::text_faint()),
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_with(records: Vec<journal::Record>) -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path().join("j"), journal::Keep::default());
        for record in records {
            journal.write(Stream::Files, &record);
        }
        (dir, journal)
    }

    #[test]
    fn a_view_opens_on_the_newest_day_it_has() {
        let (_dir, journal) = journal_with(vec![journal::Record::Event(journal::Event::new(
            Kind::Copy,
            "/a",
        ))]);
        let view = View::new(&journal);
        assert_eq!(view.shown, Shown::All, "the fullest answer is the default");
        assert_eq!(view.at, 0);
        assert_eq!(view.day(), Some(Day::today()));
        assert_eq!(view.rows.len(), 1);
    }

    #[test]
    fn the_mixed_view_puts_the_two_streams_in_one_order() {
        // A copy, then the command that consumed it, is one story - and it is
        // told in two files because of how they are stored, not how they
        // happened.
        let (_dir, journal) = journal_with(vec![]);
        journal.record(journal::Event {
            at: 100,
            ..journal::Event::new(Kind::Copy, "/src/a.c")
        });
        journal.record(journal::Event {
            at: 200,
            ..journal::Event::new(Kind::Command, "/build").note("make")
        });

        let mut view = View::new(&journal);
        view.at = view
            .days
            .iter()
            .position(|&d| d == Day::of_time(100))
            .expect("the day is offered");
        view.reload(&journal);

        let kinds: Vec<Kind> = view
            .rows
            .iter()
            .map(|row| match row {
                Row::One(event) => event.kind,
                Row::Run { group, .. } => group.kind,
            })
            .collect();
        assert_eq!(kinds, vec![Kind::Command, Kind::Copy], "newest first");
    }

    #[test]
    fn the_day_survives_a_change_of_view() {
        // Comparing what was done against what was run on the same afternoon
        // should not mean setting the date again on every click.
        let (_dir, journal) = journal_with(vec![]);
        let old = 1_700_000_000;
        journal.record(journal::Event {
            at: old,
            ..journal::Event::new(Kind::Copy, "/a")
        });
        journal.record(journal::Event {
            at: old,
            ..journal::Event::new(Kind::Command, "/w").note("ls")
        });
        journal.record(journal::Event::new(Kind::Copy, "/today"));

        let mut view = View::new(&journal);
        view.at = view
            .days
            .iter()
            .position(|&d| d == Day::of_time(old))
            .expect("offered");
        let chosen = view.day();

        view.show(&journal, Shown::Commands);
        assert_eq!(view.day(), chosen, "still the same afternoon");
        view.reload(&journal);
        assert_eq!(view.rows.len(), 1);
    }

    #[test]
    fn a_day_the_new_view_has_nothing_on_falls_back_to_its_newest() {
        let (_dir, journal) = journal_with(vec![]);
        journal.record(journal::Event {
            at: 1_700_000_000,
            ..journal::Event::new(Kind::Copy, "/a")
        });
        journal.record(journal::Event::new(Kind::Command, "/w").note("ls"));

        let mut view = View::new(&journal);
        view.at = view
            .days
            .iter()
            .position(|&d| d == Day::of_time(1_700_000_000))
            .expect("offered");
        view.show(&journal, Shown::Commands);
        assert_eq!(view.day(), Some(Day::today()), "nothing that day to show");
    }

    #[test]
    fn a_view_of_nothing_is_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path().join("empty"), journal::Keep::default());
        let view = View::new(&journal);
        assert!(view.days.is_empty());
        assert_eq!(view.day(), None);
        assert!(view.rows.is_empty());
    }

    #[test]
    fn a_day_is_read_once_rather_than_every_frame() {
        // The file is on disk and the frame is sixty times a second.
        let (_dir, journal) = journal_with(vec![journal::Record::Event(journal::Event::new(
            Kind::Copy,
            "/a",
        ))]);
        let mut view = View::new(&journal);
        let loaded = view.loaded;

        // Another record arrives, but nothing has asked for a re-read.
        journal.record(journal::Event::new(Kind::Copy, "/b"));
        view.reload(&journal);
        assert_eq!(view.loaded, loaded);
        assert_eq!(view.rows.len(), 1, "still the copy it already had");

        // Changing the view and back is what asks.
        view.show(&journal, Shown::Commands);
        view.reload(&journal);
        assert!(view.rows.is_empty(), "no commands were run");
        view.show(&journal, Shown::Files);
        view.reload(&journal);
        assert_eq!(view.rows.len(), 2, "and now it has both");
    }

    #[test]
    fn switching_view_shows_only_what_that_view_holds() {
        let (_dir, journal) = journal_with(vec![journal::Record::Event(journal::Event::new(
            Kind::Copy,
            "/a",
        ))]);
        journal.record(journal::Event::new(Kind::Command, "/work").note("ls"));

        let mut view = View::new(&journal);
        assert_eq!(view.rows.len(), 2, "All has the copy and the command");
        view.show(&journal, Shown::Commands);
        view.reload(&journal);
        assert_eq!(view.at, 0);
        assert_eq!(view.days, vec![Day::today()]);
        assert_eq!(view.rows.len(), 1);
        let Row::One(event) = &view.rows[0] else {
            panic!("grouped?")
        };
        assert_eq!(event.kind, Kind::Command);
        assert_eq!(event.note, "ls");
    }
}
