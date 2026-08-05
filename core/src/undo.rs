// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reversing the last file operation, out of the account.
//!
//! The journal already knows the last copy's destinations, the last move's
//! two ends and the last rename's both names - this reads that record and
//! turns it into a plan: exactly what would be done, and exactly what cannot
//! be, each with its reason. The front-end shows the plan before anything
//! moves, because an undo that guesses is worse than none.
//!
//! What is *not* reversed is said rather than skipped. A permanent delete
//! has nothing to bring back; a trashed file is safe but restoring is not
//! built yet; a run too large to record every item cannot be reversed whole.

use std::path::{Path, PathBuf};

use crate::journal::{Kind, Record, MAX_EVENTS_PER_GROUP};

/// One reversal, ready to be applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A copy's product: remove it. Folders the copy created are left if
    /// they are not empty, which the plan says out loud.
    RemoveCopied { copy: PathBuf },
    /// A move or rename, the other way round.
    MoveBack { now: PathBuf, was: PathBuf },
    /// A directory that was made: removed only if it is empty.
    RemoveMade { dir: PathBuf },
}

/// What undoing the last operation would mean.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The operation being reversed, in its own words.
    pub what: String,
    pub at: i64,
    pub steps: Vec<Step>,
    /// The items that cannot be reversed, each with its reason.
    pub refused: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Undoable {
    Plan(Plan),
    /// The last operation cannot be reversed at all, and this is why.
    Refused {
        what: String,
        why: String,
    },
    Nothing,
}

fn reversible(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Copy | Kind::Move | Kind::Rename | Kind::MakeDir | Kind::Trash | Kind::Delete
    )
}

/// Read the last file operation out of the records and say how to reverse
/// it - checked against the live filesystem, so the plan is about the world
/// as it is, not as the record left it.
pub fn plan(records: &[Record]) -> Undoable {
    // The newest event of a kind that is about files.
    let Some(last) = records.iter().rev().find_map(|record| match record {
        Record::Event(event) if reversible(event.kind) => Some(event),
        _ => None,
    }) else {
        return Undoable::Nothing;
    };

    // The whole run it belonged to, or the event alone.
    let (events, what): (Vec<_>, String) = match last.group {
        Some(id) => {
            let events: Vec<_> = records
                .iter()
                .filter_map(|record| match record {
                    Record::Event(event) if event.group == Some(id) => Some(event),
                    _ => None,
                })
                .collect();
            let summary = records
                .iter()
                .find_map(|record| match record {
                    Record::Group(group) if group.id == id => Some(group.summary.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| last.label().to_string());
            (events, summary)
        }
        None => (vec![last], format!("{} {}", last.kind.label(), last.path)),
    };

    match last.kind {
        Kind::Delete => {
            return Undoable::Refused {
                what,
                why: "deleted for good - there is nothing to bring back".into(),
            };
        }
        Kind::Trash => {
            return Undoable::Refused {
                what,
                why: "it is safe in the trash; restoring from there is not built yet".into(),
            };
        }
        _ => {}
    }
    if events.len() >= MAX_EVENTS_PER_GROUP {
        return Undoable::Refused {
            what,
            why: "the run was too large to record every item, so it cannot be reversed whole"
                .into(),
        };
    }

    let mut steps = Vec::new();
    let mut refused = Vec::new();
    for event in events {
        if event.failed.is_some() {
            // Nothing happened, so there is nothing to reverse.
            continue;
        }
        match event.kind {
            Kind::Copy => {
                let Some(copy) = event.to.as_deref().map(PathBuf::from) else {
                    refused.push((
                        event.path.clone(),
                        "the record does not say where it went".into(),
                    ));
                    continue;
                };
                if !copy.exists() {
                    refused.push((event.path.clone(), "the copy is already gone".into()));
                } else if modified_after(&copy, event.at) {
                    refused.push((
                        event.path.clone(),
                        "the copy has been changed since - removing it would lose work".into(),
                    ));
                } else {
                    steps.push(Step::RemoveCopied { copy });
                }
            }
            Kind::Move | Kind::Rename => {
                let was = PathBuf::from(&event.path);
                let Some(now) = event.to.as_deref().map(PathBuf::from) else {
                    refused.push((
                        event.path.clone(),
                        "the record does not say where it went".into(),
                    ));
                    continue;
                };
                if !now.exists() {
                    refused.push((
                        event.path.clone(),
                        "it is no longer where it was put".into(),
                    ));
                } else if was.exists() {
                    refused.push((
                        event.path.clone(),
                        "something else is where it came from".into(),
                    ));
                } else {
                    steps.push(Step::MoveBack { now, was });
                }
            }
            Kind::MakeDir => {
                let dir = PathBuf::from(&event.path);
                if dir.is_dir() {
                    steps.push(Step::RemoveMade { dir });
                } else {
                    refused.push((event.path.clone(), "it is already gone".into()));
                }
            }
            _ => {}
        }
    }

    Undoable::Plan(Plan {
        what,
        at: last.at,
        steps,
        refused,
    })
}

/// Later than the record by more than clock slack: somebody worked on it.
fn modified_after(path: &Path, at: i64) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    since.as_secs() as i64 > at + 2
}

/// Do it. Returns what failed, each with its reason; an empty list is a
/// clean reversal.
pub fn apply(plan: &Plan) -> Vec<(PathBuf, String)> {
    let mut failures = Vec::new();
    for step in &plan.steps {
        let outcome = match step {
            Step::RemoveCopied { copy } => {
                if copy.is_dir() {
                    // Only if empty: a directory that gained contents since
                    // the copy is not the copy's to take.
                    std::fs::remove_dir(copy).map_err(|e| (copy.clone(), e.to_string()))
                } else {
                    std::fs::remove_file(copy).map_err(|e| (copy.clone(), e.to_string()))
                }
            }
            Step::MoveBack { now, was } => {
                if was.exists() {
                    Err((now.clone(), "something else is where it came from".into()))
                } else {
                    std::fs::rename(now, was).map_err(|e| (now.clone(), e.to_string()))
                }
            }
            Step::RemoveMade { dir } => {
                std::fs::remove_dir(dir).map_err(|e| (dir.clone(), e.to_string()))
            }
        };
        if let Err(failure) = outcome {
            failures.push(failure);
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{now, Event, Group};

    fn group(records: &mut Vec<Record>, kind: Kind, summary: &str, id: u64) {
        records.push(Record::Group(Group {
            id,
            at: now(),
            kind,
            summary: summary.into(),
        }));
    }

    #[test]
    fn a_copy_is_undone_by_removing_what_it_made() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        let copy = dir.path().join("backup").join("a.txt");
        std::fs::create_dir(dir.path().join("backup")).unwrap();
        std::fs::write(&source, "x").unwrap();
        std::fs::write(&copy, "x").unwrap();

        let mut records = Vec::new();
        group(&mut records, Kind::Copy, "Copy 1 item to backup", 7);
        records.push(Record::Event(
            Event::new(Kind::Copy, &source).to(&copy).in_group(7),
        ));

        let Undoable::Plan(plan) = plan(&records) else {
            panic!("a plan")
        };
        assert_eq!(plan.what, "Copy 1 item to backup");
        assert_eq!(plan.steps, vec![Step::RemoveCopied { copy: copy.clone() }]);
        assert!(plan.refused.is_empty());

        assert!(apply(&plan).is_empty());
        assert!(!copy.exists(), "the copy is gone");
        assert!(source.exists(), "the source was never touched");
    }

    #[test]
    fn a_changed_copy_is_refused_rather_than_taken() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("a.txt");
        std::fs::write(&copy, "edited since").unwrap();

        let mut records = Vec::new();
        let mut event = Event::new(Kind::Copy, "/somewhere/a.txt").to(&copy);
        // Recorded well before the file's mtime: somebody worked on it.
        event.at = now() - 3_600;
        records.push(Record::Event(event));

        let Undoable::Plan(plan) = plan(&records) else {
            panic!("a plan")
        };
        assert!(plan.steps.is_empty());
        assert_eq!(plan.refused.len(), 1);
        assert!(
            plan.refused[0].1.contains("changed since"),
            "{:?}",
            plan.refused
        );
        assert!(copy.exists(), "nothing was touched");
    }

    #[test]
    fn a_move_goes_back_and_refuses_a_taken_seat() {
        let dir = tempfile::tempdir().unwrap();
        let was = dir.path().join("a.txt");
        let now_at = dir.path().join("moved").join("a.txt");
        std::fs::create_dir(dir.path().join("moved")).unwrap();
        std::fs::write(&now_at, "x").unwrap();

        let mut records = Vec::new();
        records.push(Record::Event(Event::new(Kind::Move, &was).to(&now_at)));

        let Undoable::Plan(the_plan) = plan(&records) else {
            panic!("a plan")
        };
        assert_eq!(
            the_plan.steps,
            vec![Step::MoveBack {
                now: now_at.clone(),
                was: was.clone()
            }]
        );
        assert!(apply(&the_plan).is_empty());
        assert!(was.exists() && !now_at.exists(), "back where it started");

        // Doing it again: the original seat is taken now.
        let Undoable::Plan(again) = plan(&records) else {
            panic!("a plan")
        };
        assert!(again.steps.is_empty());
        assert!(
            again.refused[0].1.contains("no longer where"),
            "{:?}",
            again.refused
        );
    }

    #[test]
    fn what_cannot_come_back_is_said_not_skipped() {
        let mut records = vec![Record::Event(Event::new(Kind::Delete, "/gone.txt"))];
        let Undoable::Refused { why, .. } = plan(&records) else {
            panic!("refused")
        };
        assert!(why.contains("nothing to bring back"));

        records.push(Record::Event(Event::new(Kind::Trash, "/binned.txt")));
        let Undoable::Refused { why, .. } = plan(&records) else {
            panic!("refused")
        };
        assert!(why.contains("safe in the trash"));
    }

    #[test]
    fn the_last_file_operation_wins_and_commands_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let made = dir.path().join("new-folder");
        std::fs::create_dir(&made).unwrap();

        let records = vec![
            Record::Event(Event::new(Kind::Copy, "/old").to("/older")),
            Record::Event(Event::new(Kind::MakeDir, &made)),
            Record::Event(Event::new(Kind::Command, "/here").note("cargo test")),
        ];
        let Undoable::Plan(the_plan) = plan(&records) else {
            panic!("a plan")
        };
        assert_eq!(the_plan.steps, vec![Step::RemoveMade { dir: made.clone() }]);
        assert!(apply(&the_plan).is_empty());
        assert!(!made.exists());
    }

    #[test]
    fn an_empty_account_has_nothing_to_undo() {
        assert_eq!(plan(&[]), Undoable::Nothing);
    }
}
