// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Commands worth keeping at the top: pinned to the directory they belong to.
//!
//! The history already offers back what was run here, newest first - but a
//! project's build command should not have to be re-earned every twenty
//! entries. A pin says "this line, this folder, always on top". It is the
//! reuse of commands made deliberate rather than incidental.
//!
//! Pins are their own file - the eighth kept thing - because they change at
//! their own rate and mean something different from history: a pin is a
//! choice, history is a record. A pinned line is kept exactly as typed, so
//! a template with `%f` in it stays a template and expands against whatever
//! the cursor is on when it runs.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One pinned line, and the directory it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub cwd: PathBuf,
    pub line: String,
}

/// Every pin, in the order they were made.
///
/// Order is kept rather than sorted: the reader builds the shelf, and a
/// shelf that rearranges itself is a shelf you stop trusting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pinned {
    #[serde(default, rename = "pin")]
    pub pins: Vec<Pin>,
}

impl Pinned {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("lost-commander").join("pinned.toml"))
    }

    /// Never fails: no file means nothing pinned yet.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| Self::load_from(&path).ok())
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, text)
    }

    /// Pin a line here, or take the pin back out - and say which happened.
    ///
    /// One operation rather than pin/unpin, because the caller is a command
    /// line: `pin cargo test` said twice should end where it started, not
    /// error the second time.
    pub fn toggle(&mut self, cwd: &Path, line: &str) -> bool {
        let line = line.trim();
        let before = self.pins.len();
        self.pins
            .retain(|pin| !(pin.cwd == cwd && pin.line == line));
        if self.pins.len() < before {
            return false;
        }
        self.pins.push(Pin {
            cwd: cwd.to_path_buf(),
            line: line.to_string(),
        });
        true
    }

    pub fn is_pinned(&self, cwd: &Path, line: &str) -> bool {
        self.pins
            .iter()
            .any(|pin| pin.cwd == cwd && pin.line == line)
    }

    /// The shelf for one directory, in the order it was built.
    pub fn here(&self, cwd: &Path) -> Vec<&Pin> {
        self.pins.iter().filter(|pin| pin.cwd == cwd).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_toggles_and_the_shelf_keeps_its_order() {
        let mut pinned = Pinned::default();
        let here = Path::new("/project");

        assert!(pinned.toggle(here, "cargo test"));
        assert!(pinned.toggle(here, "cargo build --release"));
        assert!(pinned.toggle(Path::new("/elsewhere"), "make"));

        let shelf: Vec<&str> = pinned.here(here).iter().map(|p| p.line.as_str()).collect();
        assert_eq!(
            shelf,
            vec!["cargo test", "cargo build --release"],
            "this folder's pins, in the order they were made"
        );
        assert!(pinned.is_pinned(here, "cargo test"));
        assert!(
            !pinned.is_pinned(here, "make"),
            "another folder's pin is not this folder's"
        );

        // Said twice, it ends where it started.
        assert!(!pinned.toggle(here, "cargo test"));
        assert!(!pinned.is_pinned(here, "cargo test"));
    }

    #[test]
    fn pins_survive_the_file_and_a_missing_file_is_just_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("pinned.toml");

        let mut pinned = Pinned::default();
        pinned.toggle(Path::new("/project"), "cargo test %f");
        pinned.save_to(&path).unwrap();

        let read = Pinned::load_from(&path).unwrap();
        assert_eq!(read, pinned, "as typed - a template stays a template");

        assert!(Pinned::load_from(&dir.path().join("nothing.toml")).is_err());
        assert!(Pinned::default().pins.is_empty());
    }

    #[test]
    fn whitespace_is_not_identity() {
        let mut pinned = Pinned::default();
        let here = Path::new("/project");
        pinned.toggle(here, "  cargo test  ");
        assert!(pinned.is_pinned(here, "cargo test"));
        assert!(
            !pinned.toggle(here, "cargo test"),
            "the same line, so it unpins"
        );
    }
}
