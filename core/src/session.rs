// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The windows you had open, written down.
//!
//! A workspace is a window: two directories, how the panes were arranged,
//! what each was drawing, and where its shell was standing. This is that,
//! flattened into something a file can hold and a front-end can read back.
//!
//! Deliberately made of strings and paths rather than the front-end's own
//! types. The engine has no idea what a "grid view" is and should not learn:
//! a name crosses the boundary, and the front-end decides what it means. An
//! unknown name read back from a file somebody edited is not an error
//! either: it falls back to the ordinary listing, because a session file is
//! not worth refusing to start over.
//!
//! A shell is not saved, because a shell cannot be reopened: the process is
//! gone and its scrollback with it. Its *directory* is saved, which is the
//! part that still means something - and the account already holds what was
//! run there, so nothing about the work is lost.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One window: its panes, its arrangement, and where its shell stood.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// The directory of the pane that is always there.
    pub left: PathBuf,
    /// The second pane's directory, kept even when it is folded away: it is
    /// where the pane comes back to.
    pub right: PathBuf,
    #[serde(default)]
    pub show_right: bool,
    /// `list`, `grid`, `tree`, `preview` or `history` - whatever the
    /// front-end calls its views.
    #[serde(default)]
    pub left_view: String,
    #[serde(default)]
    pub right_view: String,
    /// `both`, `files` or `shell`: which halves of the window this one shows.
    #[serde(default)]
    pub half: String,
    /// Which pane was being worked in: `left` or `right`.
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub split: f32,
    /// Whether the shell followed the panes here.
    #[serde(default)]
    pub synced: bool,
    /// Where this workspace's shell was standing, if it had one. Not which
    /// shell: that is a setting, and the reader may well have changed it
    /// between one run and the next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<PathBuf>,
}

/// Every window, and which was on show.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    /// Which of them was in front. Out of range is treated as the first,
    /// because a file with an index and no workspaces is a file somebody
    /// edited by hand.
    #[serde(default)]
    pub at: usize,
}

impl Session {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("lost-commander").join("workspaces.toml"))
    }

    /// Never fails: no file means no saved session, which is not a problem.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| Self::load_from(&p).ok())
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut session: Session = toml::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if session.at >= session.workspaces.len() {
            session.at = 0;
        }
        Ok(session)
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, text)
    }

    /// Write to the reader's own session file.
    ///
    /// **Never call this from a test** - it writes the real file belonging to
    /// whoever ran the suite. Tests want [`Session::save_to`] and a temporary
    /// directory.
    pub fn save(&self) -> io::Result<()> {
        let path =
            Self::path().ok_or_else(|| io::Error::other("no config directory on this platform"))?;
        self.save_to(&path)
    }

    /// Only the workspaces whose left-hand directory is still there.
    ///
    /// A saved window pointing at an unplugged disk or a deleted branch would
    /// otherwise come back as a pane showing an error, and a session that
    /// opens on four error messages is worse than one that opens on three
    /// windows and says so.
    pub fn still_there(&self) -> (Vec<Workspace>, Vec<PathBuf>) {
        let mut kept = Vec::new();
        let mut gone = Vec::new();
        for workspace in &self.workspaces {
            if workspace.left.is_dir() {
                kept.push(workspace.clone());
            } else {
                gone.push(workspace.left.clone());
            }
        }
        (kept, gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(left: &str) -> Workspace {
        Workspace {
            left: PathBuf::from(left),
            right: PathBuf::from(left),
            show_right: true,
            left_view: "tree".into(),
            right_view: "list".into(),
            half: "both".into(),
            active: "left".into(),
            split: 0.4,
            synced: true,
            shell: Some(PathBuf::from(left)),
        }
    }

    #[test]
    fn a_session_round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("workspaces.toml");
        let session = Session {
            workspaces: vec![one("/one"), one("/two")],
            at: 1,
        };
        session.save_to(&path).unwrap();

        let read = Session::load_from(&path).unwrap();
        assert_eq!(read, session, "everything a window is, back again");
    }

    #[test]
    fn no_file_is_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Session::load_from(&dir.path().join("nothing.toml")).is_err());
        // And the front-end's entry point turns that into "no windows saved".
        assert_eq!(Session::default().workspaces.len(), 0);
    }

    #[test]
    fn an_index_past_the_end_is_the_first_window() {
        // A file somebody edited by hand, or one written by a version that
        // saved more windows than this one reads.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.toml");
        Session {
            workspaces: vec![one("/one")],
            at: 7,
        }
        .save_to(&path)
        .unwrap();
        assert_eq!(Session::load_from(&path).unwrap().at, 0);
    }

    #[test]
    fn a_window_whose_directory_is_gone_is_named_rather_than_opened() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().display().to_string();
        let session = Session {
            workspaces: vec![one(&here), one("/definitely/not/here")],
            at: 0,
        };

        let (kept, gone) = session.still_there();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].left, PathBuf::from(&here));
        assert_eq!(gone, vec![PathBuf::from("/definitely/not/here")]);
    }

    #[test]
    fn a_file_with_missing_fields_still_reads() {
        // Written by an older version, or by hand. Everything but the two
        // directories has a default, so the window still opens.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.toml");
        std::fs::write(&path, "[[workspaces]]\nleft = \"/a\"\nright = \"/b\"\n").unwrap();

        let read = Session::load_from(&path).unwrap();
        assert_eq!(read.workspaces.len(), 1);
        assert!(read.workspaces[0].shell.is_none());
        assert_eq!(read.workspaces[0].half, "");
    }
}
