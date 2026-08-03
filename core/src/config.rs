// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! User preferences that outlive a session.
//!
//! Kept apart from the bookmark file: bookmarks are data the user builds up,
//! settings are choices they make once.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

// PartialEq without Eq: the two layout fractions are floats, and a float is
// exactly the thing Eq promises more about than it can keep.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Which shell to run commands with. `None` means "whatever the
    /// environment says", which is the right default for most people but no
    /// use on a machine with four shells installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,

    /// The chosen theme, when it is one of the presets. Kept as a name rather
    /// than a copy of its colours, so a preset that is later improved is
    /// picked up rather than frozen at whatever it was the day it was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// A palette that is nobody's preset, kept in full.
    ///
    /// Held as an unread table rather than a typed palette, because the roles
    /// a palette has are the drawing front-end's business and this crate does
    /// not draw. It used to be `gui::theme::Palette` behind a feature flag,
    /// which is the same thing as the engine knowing what a sidebar is.
    ///
    /// The point of keeping it at all is that it must **survive** a front-end
    /// that has no idea what it means: the terminal binary and the C ABI both
    /// read and write this file, and a key they dropped on the way through
    /// would silently destroy a reader's custom colours the first time they
    /// changed any other setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<toml::Value>,

    /// Where the divider between the panes sits, as the left pane's share.
    ///
    /// A fraction rather than pixels, so the same setting means the same
    /// thing on a resized window and a different monitor. Absent means half
    /// and half. Not gui-gated: how a reader likes the window arranged is a
    /// fact about the reader, not about which front-end is drawing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_split: Option<f32>,

    /// How much of a pane's height the tree half gets, when it has one.
    ///
    /// A fraction for the same reason as [`Settings::pane_split`]: it means
    /// the same thing on a resized window and on a different monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_split: Option<f32>,

    /// How tall the shell drawer is, in the front-end's own units.
    ///
    /// Points here, because a drawer's useful height is "eight lines of
    /// output", which does not scale with the window the way the split does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_height: Option<f32>,

    /// How many days of the account of what was done to keep.
    ///
    /// Absent means the default of thirty. Zero means for ever, which is a
    /// real answer somebody wants - not a way of saying "keep none of it",
    /// which is [`Settings::journal`] set to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_days: Option<u32>,

    /// Whether to keep an account at all. Absent means yes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<bool>,

    /// Whether the shell picker should offer only the shells whose commands
    /// can be recorded. Absent means no - every shell the machine offers.
    ///
    /// It is worth being clear about what turning this on does and does not
    /// buy. It stops a shell being *chosen* here that cannot report what it
    /// runs; it cannot stop one being *reached* - `ssh`, `docker exec`, or
    /// simply typing `sh` - and commands run in those are as invisible as
    /// they ever were. It narrows the hole; it does not close it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journaled_shells_only: Option<bool>,
}

impl Settings {
    /// How long the account is kept for.
    pub fn keep(&self) -> crate::journal::Keep {
        self.journal_days
            .map(crate::journal::Keep)
            .unwrap_or_default()
    }

    /// Whether anything is recorded at all.
    pub fn keeps_a_journal(&self) -> bool {
        self.journal.unwrap_or(true)
    }

    /// Whether the picker is narrowed to the shells that can be recorded.
    pub fn only_journaled_shells(&self) -> bool {
        self.journaled_shells_only.unwrap_or(false)
    }

    /// The shells to offer, given that preference.
    ///
    /// Narrowing never empties the list: a machine with nothing but `sh` on
    /// it would otherwise offer no shell at all, and a file manager that
    /// cannot open a terminal because of a logging preference has its
    /// priorities the wrong way round.
    pub fn shells_to_offer(&self, found: Vec<String>) -> Vec<String> {
        if !self.only_journaled_shells() {
            return found;
        }
        let narrowed: Vec<String> = found
            .iter()
            .filter(|program| crate::shellhook::journals(program))
            .cloned()
            .collect();
        if narrowed.is_empty() {
            return found;
        }
        narrowed
    }

    /// The journal these settings ask for, or nothing where it is turned off
    /// or there is nowhere to put it.
    pub fn journal(&self) -> Option<crate::journal::Journal> {
        if !self.keeps_a_journal() {
            return None;
        }
        crate::journal::Journal::default_dir()
            .map(|dir| crate::journal::Journal::at(dir, self.keep()))
    }
}

impl Settings {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("lost-commander").join("settings.toml"))
    }

    /// Never fails: a missing or unreadable file just means "defaults".
    pub fn load() -> Self {
        Self::config_path()
            .and_then(|p| Self::load_from(&p).ok())
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

    /// Write these settings to the user's own configuration file.
    ///
    /// **Never call this from a test.** It writes the real file belonging to
    /// whoever ran the suite, and a test that did so once took somebody's
    /// chosen theme with it. Tests want [`Settings::save_to`] and a temporary
    /// directory; front-ends that want to test the *deciding* should split it
    /// from the saving, as `remember_layout` does.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| io::Error::other("no config directory on this platform"))?;
        self.save_to(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");

        let settings = Settings {
            shell: Some("/bin/zsh".into()),
            ..Settings::default()
        };
        settings.save_to(&path).unwrap();
        assert!(path.exists(), "the parent directory is created on demand");

        assert_eq!(Settings::load_from(&path).unwrap(), settings);
    }

    #[test]
    fn an_unset_shell_is_omitted_rather_than_written_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        Settings::default().save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("shell"), "{text}");
        assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    }

    #[test]
    fn a_missing_or_broken_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Settings::load_from(&dir.path().join("absent.toml")).is_err());

        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "shell = [[[").unwrap();
        assert!(Settings::load_from(&broken).is_err());
    }

    #[test]
    fn every_shell_is_offered_unless_the_setting_says_otherwise() {
        let found: Vec<String> = ["/bin/bash", "/bin/dash", "/usr/bin/fish", "/bin/sh"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // The default: the machine's answer, untouched. Which shell to use is
        // the user's decision, not a logging feature's.
        let settings = Settings::default();
        assert!(!settings.only_journaled_shells());
        assert_eq!(settings.shells_to_offer(found.clone()), found);

        let strict = Settings {
            journaled_shells_only: Some(true),
            ..Settings::default()
        };
        assert_eq!(
            strict.shells_to_offer(found.clone()),
            vec!["/bin/bash".to_string(), "/usr/bin/fish".to_string()]
        );
    }

    #[test]
    fn narrowing_never_leaves_someone_with_no_shell_at_all() {
        // A machine with nothing but sh on it would otherwise offer none, and
        // a file manager that cannot open a terminal because of a logging
        // preference has its priorities the wrong way round.
        let strict = Settings {
            journaled_shells_only: Some(true),
            ..Settings::default()
        };
        let only_posix = vec!["/bin/sh".to_string(), "/bin/dash".to_string()];
        assert_eq!(strict.shells_to_offer(only_posix.clone()), only_posix);
    }

    #[test]
    fn unknown_keys_from_a_newer_version_do_not_break_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "shell = \"/bin/fish\"\nfuture_option = 42\n").unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.shell.as_deref(), Some("/bin/fish"));
    }
}
