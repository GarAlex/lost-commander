// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The engine behind lost-commander, with no user interface attached.
//!
//! Everything here is pure filesystem and state logic, which is what makes a
//! second front-end possible: the terminal UI (`lostc`) and the graphical one
//! (`lostc-gui`) are two thin shells over exactly the same code.

pub mod apps;
pub mod archive;
pub mod compare;
pub mod config;
pub mod diff;
pub mod dupes;
pub mod elevate;
pub mod encoding;
pub mod entry;
pub mod filekind;
pub mod find;
pub mod fsops;
pub mod hex;
pub mod imageops;
pub mod journal;
pub mod markdown;
pub mod mount;
pub mod netloc;
pub mod open;
pub mod panel;
pub mod paths;
pub mod perms;
pub mod preview;
pub mod progress;
pub mod pty;
pub mod record;
pub mod rename;
pub mod shell;
pub mod shellhook;
pub mod tabs;
pub mod termview;
pub mod textedit;
pub mod textindex;
pub mod themes;
pub mod trash;
pub mod tree;
