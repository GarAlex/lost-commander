// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The places a machine already has: drives, volumes and the user's folders.
//!
//! Without these a file manager can only show you where you already are.
//! Everything else - another drive, a memory stick, the Downloads folder -
//! has to be reached by typing a path or by walking to the root and back
//! down, and a reader who does not already know the layout has no way to find
//! out that a second drive exists at all.
//!
//! Discovery is here rather than in a front-end because every front-end needs
//! the same answer and none of them should be enumerating drive letters. The
//! platform and the "does this exist" test are parameters, so the Windows
//! answer is testable from Linux - the same trick [`crate::mount`] and
//! [`crate::shell`] use.

use std::path::{Path, PathBuf};

use crate::mount::Platform;

/// What kind of place this is, so a front-end can pick an icon and an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The user's own home.
    Home,
    /// One of the well-known folders inside it.
    Folder,
    /// A disk: a drive letter on Windows, the root or a mounted volume
    /// elsewhere.
    Drive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// Bytes free on the disk this sits on, where that could be asked.
    ///
    /// How much room is left is the first thing anybody wants to know of a
    /// drive, and the Commanders have shown it since there were floppies.
    /// `None` for a folder - repeating the same figure under Documents,
    /// Downloads and Pictures says nothing, since they are all the same disk.
    pub free: Option<u64>,
}

/// The user's home and the well-known folders inside it.
///
/// Taken as arguments rather than read here so this is testable: on a build
/// machine `dirs::download_dir()` may be missing, which is a fact about that
/// machine and not something to encode in a test.
pub fn user_places(home: Option<PathBuf>, folders: &[(&str, Option<PathBuf>)]) -> Vec<Place> {
    let mut out = Vec::new();
    if let Some(home) = home {
        out.push(Place {
            name: "Home".to_string(),
            path: home,
            kind: Kind::Home,
            free: None,
        });
    }
    for (name, path) in folders {
        let Some(path) = path else { continue };
        // A folder that is the home itself is not a second entry: some
        // systems answer with the home for a folder they do not have.
        if out.iter().any(|place| place.path == *path) {
            continue;
        }
        out.push(Place {
            name: (*name).to_string(),
            path: path.clone(),
            kind: Kind::Folder,
            free: None,
        });
    }
    out
}

/// The disks this machine has.
///
/// On Windows that is the drive letters that answer; there is no root above
/// them, which is the thing that most surprises somebody arriving from Unix -
/// `C:` and `D:` are two trees, not two directories.
///
/// Elsewhere it is `/`, plus wherever removable media is mounted. Those
/// directories are looked in rather than assumed: `/media/you/stick` exists
/// only while the stick does.
pub fn drives(
    platform: Platform,
    exists: &dyn Fn(&Path) -> bool,
    user: Option<&str>,
) -> Vec<Place> {
    let mut out = Vec::new();
    match platform {
        Platform::Windows => {
            for letter in b'A'..=b'Z' {
                let path = PathBuf::from(format!("{}:\\", letter as char));
                if exists(&path) {
                    out.push(Place {
                        name: format!("{}:", letter as char),
                        free: free_on(&path),
                        path,
                        kind: Kind::Drive,
                    });
                }
            }
        }
        platform => {
            out.push(Place {
                name: "/".to_string(),
                free: free_on(Path::new("/")),
                path: PathBuf::from("/"),
                kind: Kind::Drive,
            });
            // Where each system puts removable media. `/run/media/<user>` is
            // what udisks uses on current Linux; `/media` and `/mnt` are
            // older and still common; `/Volumes` is macOS.
            let mut roots: Vec<PathBuf> = vec![PathBuf::from("/media"), PathBuf::from("/mnt")];
            if let Some(user) = user {
                roots.insert(0, PathBuf::from(format!("/run/media/{user}")));
                roots.insert(1, PathBuf::from(format!("/media/{user}")));
            }
            if platform == Platform::MacOs {
                roots.insert(0, PathBuf::from("/Volumes"));
            }
            for root in roots {
                if !exists(&root) {
                    continue;
                }
                let Ok(entries) = std::fs::read_dir(&root) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if out.iter().any(|place: &Place| place.path == path) {
                        continue;
                    }
                    out.push(Place {
                        name,
                        free: free_on(&path),
                        path,
                        kind: Kind::Drive,
                    });
                }
            }
        }
    }
    out
}

/// Bytes free on the disk holding `path`, or `None` if it will not say.
///
/// A drive that is not ready - an empty optical drive, a card reader with no
/// card - answers with an error rather than a number, and asking again will
/// not help. `None` is the honest result, and a front-end showing nothing
/// beats one showing zero, which reads as a full disk.
pub fn free_on(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

/// Everything this machine offers, for a sidebar.
///
/// Drives first: which disk you are on is the coarser question, and on
/// Windows it is the one with no other answer on screen.
pub fn system_places() -> Vec<Place> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok();
    let mut out = drives(Platform::current(), &|path| path.exists(), user.as_deref());
    out.extend(user_places(
        dirs::home_dir(),
        &[
            ("Desktop", dirs::desktop_dir()),
            ("Documents", dirs::document_dir()),
            ("Downloads", dirs::download_dir()),
            ("Pictures", dirs::picture_dir()),
            ("Music", dirs::audio_dir()),
            ("Videos", dirs::video_dir()),
        ],
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_offers_the_drive_letters_that_answer() {
        // There is no root above them: `C:` and `D:` are two trees, not two
        // directories, which is exactly why they have to be listed.
        let there = |path: &Path| {
            let shown = path.display().to_string();
            shown.starts_with('C') || shown.starts_with('D')
        };
        let found = drives(Platform::Windows, &there, None);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "C:");
        assert_eq!(found[0].path, PathBuf::from("C:\\"));
        assert_eq!(found[1].name, "D:");
        assert!(found.iter().all(|place| place.kind == Kind::Drive));
    }

    #[test]
    fn unix_always_offers_the_root() {
        let found = drives(Platform::Linux, &|_| false, None);
        assert_eq!(found.len(), 1, "the root is always there");
        assert_eq!(found[0].path, PathBuf::from("/"));
    }

    #[test]
    fn the_home_and_its_folders() {
        let home = PathBuf::from("/home/you");
        let found = user_places(
            Some(home.clone()),
            &[
                ("Downloads", Some(home.join("Downloads"))),
                ("Documents", None),
            ],
        );
        assert_eq!(
            found.len(),
            2,
            "a folder the machine has not got is not one"
        );
        assert_eq!(found[0].kind, Kind::Home);
        assert_eq!(found[1].name, "Downloads");
    }

    #[test]
    fn a_folder_that_is_the_home_is_not_listed_twice() {
        // Some systems answer with the home for a folder they do not have,
        // and "Home, Documents" both pointing at the same place is a sidebar
        // that has stopped meaning anything.
        let home = PathBuf::from("/home/you");
        let found = user_places(Some(home.clone()), &[("Documents", Some(home.clone()))]);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn no_home_is_not_an_error() {
        // A service account may have none, and a sidebar with only drives is
        // still a useful sidebar.
        assert!(user_places(None, &[("Downloads", None)]).is_empty());
    }

    #[test]
    fn a_drive_says_how_much_room_is_left_and_a_folder_does_not() {
        // Repeating the same figure under Documents, Downloads and Pictures
        // says nothing: they are all the same disk.
        let places = system_places();
        let drive = places.iter().find(|p| p.kind == Kind::Drive);
        if let Some(drive) = drive {
            assert!(
                drive.free.is_some(),
                "a mounted drive knows: {}",
                drive.path.display()
            );
        }
        assert!(places
            .iter()
            .filter(|p| p.kind == Kind::Folder)
            .all(|p| p.free.is_none()));
    }

    #[test]
    fn a_drive_that_will_not_say_is_not_reported_as_empty() {
        // An optical drive with no disc in it answers with an error, and
        // showing zero would read as a full disk rather than an unknown one.
        assert_eq!(free_on(Path::new("Z:\no-such-drive")), None);
    }
}
