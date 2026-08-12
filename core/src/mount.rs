// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! "Mapping" a network location onto a browsable filesystem path.
//!
//! Rather than speaking SMB/FTP in-process, we ask the operating system to
//! attach the share and then browse the resulting path with the ordinary local
//! code. That keeps one filesystem implementation and, importantly, lets the OS
//! handle authentication with its own credential store — this program never
//! sees or stores a password.
//!
//! The per-platform decision is a pure function (`plan_for`) that takes the
//! platform as an argument, so the macOS and Windows behaviour is unit-tested
//! from any host.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::netloc::{Location, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapPlan {
    /// Already browsable; nothing to run. Windows UNC paths and local
    /// directories land here.
    Direct(PathBuf),
    /// Run this, then look for the mount point.
    Command {
        program: String,
        args: Vec<String>,
        /// Best guess at where the mount will appear; scanning is still done.
        hint: Option<PathBuf>,
    },
    Unsupported {
        reason: String,
    },
}

/// Decide how to attach `location` on `platform`.
pub fn plan_for(platform: Platform, location: &Location) -> MapPlan {
    if location.protocol == Protocol::Local {
        return MapPlan::Direct(PathBuf::from(&location.path));
    }

    match platform {
        // Windows speaks SMB natively: a UNC path is just a path.
        Platform::Windows => match location.protocol {
            Protocol::Smb => {
                let mut unc = format!(r"\\{}", location.host);
                for part in location.path.split('/').filter(|p| !p.is_empty()) {
                    unc.push('\\');
                    unc.push_str(part);
                }
                MapPlan::Direct(PathBuf::from(unc))
            }
            other => MapPlan::Unsupported {
                reason: format!(
                    "Windows cannot mount {} directly; map it in Explorer first, \
                     then bookmark the drive letter",
                    other.label()
                ),
            },
        },

        // macOS: hand the URL to the OS, which mounts under /Volumes and uses
        // Keychain for credentials.
        Platform::MacOs => match location.protocol {
            Protocol::Smb | Protocol::Afp | Protocol::Nfs => MapPlan::Command {
                program: "open".into(),
                // Encoded, because this is parsed as a URL rather than read
                // by a person: a share called "Shared Folder" handed over
                // with its space in it is not a URL, and the refusal comes
                // back as "Check the server name or IP address".
                args: vec![location.os_url()],
                hint: mac_volume_hint(location),
            },
            Protocol::Ftp | Protocol::Sftp => MapPlan::Unsupported {
                reason: format!(
                    "macOS removed {} mounting from Finder (11+); \
                     use a third-party mounter such as macFUSE/sshfs, or mount it \
                     and bookmark the local path",
                    location.protocol.label()
                ),
            },
            Protocol::Local => unreachable!("handled above"),
        },

        // Linux: gvfs mounts into the user session without root.
        Platform::Linux => match location.protocol {
            Protocol::Smb | Protocol::Ftp | Protocol::Sftp => MapPlan::Command {
                program: "gio".into(),
                args: vec!["mount".into(), location.os_url()],
                hint: None,
            },
            Protocol::Nfs | Protocol::Afp => MapPlan::Unsupported {
                reason: format!(
                    "{} needs a privileged mount on Linux; mount it with \
                     sudo and bookmark the local path",
                    location.protocol.label()
                ),
            },
            Protocol::Local => unreachable!("handled above"),
        },
    }
}

pub fn plan(location: &Location) -> MapPlan {
    plan_for(Platform::current(), location)
}

fn mac_volume_hint(location: &Location) -> Option<PathBuf> {
    let share = location.share();
    if share.is_empty() {
        None
    } else {
        Some(PathBuf::from("/Volumes").join(share))
    }
}

/// Directories where the OS parks mounted volumes.
pub fn candidate_roots(platform: Platform) -> Vec<PathBuf> {
    match platform {
        Platform::MacOs => vec![PathBuf::from("/Volumes")],
        Platform::Windows => Vec::new(), // UNC paths need no mount point
        Platform::Linux => {
            let mut roots = Vec::new();
            // gvfs lives under /run/user/<uid>/gvfs; enumerate rather than
            // guessing the uid.
            if let Ok(entries) = std::fs::read_dir("/run/user") {
                for entry in entries.flatten() {
                    let gvfs = entry.path().join("gvfs");
                    if gvfs.is_dir() {
                        roots.push(gvfs);
                    }
                }
            }
            for extra in ["/media", "/mnt", "/run/media"] {
                let path = PathBuf::from(extra);
                if path.is_dir() {
                    roots.push(path);
                }
            }
            roots
        }
    }
}

/// What is mounted right now, as (source, mount point) pairs.
///
/// Asked of the system rather than guessed at from directory names. A share
/// picked from the desktop's own list mounts under whatever it is called -
/// `/Volumes/public` - and that name contains neither the host that was
/// typed nor, when only a server was typed, any share name at all. Reading
/// the table turns "which of these is mine" from a guess into a lookup:
/// `//adm@nas10/public on /Volumes/public` says exactly whose it is.
fn mounts_now(platform: Platform) -> Vec<(String, PathBuf)> {
    match platform {
        Platform::MacOs => {
            let Ok(output) = Command::new("/sbin/mount").output() else {
                return Vec::new();
            };
            parse_mount_table(&String::from_utf8_lossy(&output.stdout))
        }
        Platform::Linux => std::fs::read_to_string("/proc/self/mounts")
            .map(|text| {
                text.lines()
                    .filter_map(|line| {
                        let mut parts = line.split_whitespace();
                        let from = parts.next()?;
                        let on = parts.next()?;
                        Some((from.to_string(), PathBuf::from(on)))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Platform::Windows => Vec::new(),
    }
}

/// `//adm@nas10/public on /Volumes/public (smbfs, nodev, ...)` - the shape
/// `mount` has printed since long before any of this.
fn parse_mount_table(text: &str) -> Vec<(String, PathBuf)> {
    text.lines()
        .filter_map(|line| {
            let (from, rest) = line.split_once(" on ")?;
            // The options follow in brackets; the mount point is what comes
            // before them, and it may contain spaces.
            let on = match rest.rfind(" (") {
                Some(i) => &rest[..i],
                None => rest,
            };
            Some((from.trim().to_string(), PathBuf::from(on.trim())))
        })
        .collect()
}

/// Whether a mount source belongs to this location.
///
/// The source is `//user@host/share` for SMB and `host:/export` for NFS, so
/// the host is matched loosely - a source may name it short where the reader
/// typed it long, or the other way about - and the share exactly when one
/// was asked for. With no share named, any mount from that host is the one:
/// it is the share the reader just picked from the desktop's list.
fn source_belongs_to(source: &str, location: &Location) -> bool {
    let source = source.to_lowercase();
    let host = location.host.to_lowercase();
    let short = host.split('.').next().unwrap_or(&host).to_string();
    if host.is_empty() || !(source.contains(&host) || source.contains(&short)) {
        return false;
    }
    let share = location.share().to_lowercase();
    if share.is_empty() {
        return true;
    }
    source
        .rsplit('/')
        .next()
        .map(|leaf| leaf == share)
        .unwrap_or(false)
}

/// Look for an already-attached mount belonging to `location`.
///
/// Mount point names vary a lot (`/Volumes/media`,
/// `smb-share:server=nas.local,share=media`, `ftp:host=example.org`), so this
/// scores candidates instead of trying to reproduce each naming scheme.
pub fn find_mount_in(roots: &[PathBuf], location: &Location) -> Option<PathBuf> {
    if !location.protocol.is_network() {
        return None;
    }

    // The table first, because it knows and the names only suggest.
    let platform = Platform::current();
    for (source, on) in mounts_now(platform) {
        if source_belongs_to(&source, location) && on.is_dir() {
            return Some(on);
        }
    }
    let host = location.host.to_lowercase();
    let share = location.share().to_lowercase();

    let mut best: Option<(u8, PathBuf)> = None;

    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let has_host = !host.is_empty() && name.contains(&host);
            let has_share = !share.is_empty() && name.contains(&share);

            let score = if has_host && has_share {
                3
            } else if !share.is_empty() && name == share {
                2
            } else if has_host {
                1
            } else {
                0
            };

            if score > 0 && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, entry.path()));
            }
        }
    }

    best.map(|(_, path)| path)
}

/// Append the part of the location below the share.
fn with_subpath(base: PathBuf, location: &Location) -> PathBuf {
    let sub = location.subpath();
    if sub.is_empty() {
        base
    } else {
        sub.split('/')
            .filter(|s| !s.is_empty())
            .fold(base, |acc, part| acc.join(part))
    }
}

/// Attach `location` if needed and return a path the panels can browse.
pub fn connect(location: &Location) -> Result<PathBuf, String> {
    let platform = Platform::current();
    let roots = candidate_roots(platform);

    // Local paths and Windows UNC need no work.
    match plan(location) {
        MapPlan::Direct(path) => {
            if path.is_dir() {
                Ok(path)
            } else {
                Err(format!("Not reachable: {}", path.display()))
            }
        }
        MapPlan::Unsupported { reason } => Err(reason),
        MapPlan::Command {
            program,
            args,
            hint,
        } => {
            // Already mounted from an earlier session?
            //
            // Only when a share was actually named. An address that is just
            // a server is a request to choose - the desktop answers it with
            // a list of shares - and reusing whatever else happens to be
            // mounted from that host silently picks one on the reader's
            // behalf. That is why connecting twice behaved differently the
            // second time: the first asked which share, the second went
            // straight to the one from the first, whether or not it was the
            // one wanted this time.
            if !location.share().is_empty() {
                if let Some(found) = find_mount_in(&roots, location) {
                    return Ok(with_subpath(found, location));
                }
            }

            // stdin is closed on purpose: a helper that wants to prompt for a
            // password must fail fast rather than hang the TUI.
            let output = Command::new(&program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("could not run {program}: {e}"))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.trim();
                return Err(if detail.is_empty() {
                    format!("{program} failed to mount {}", location.to_url())
                } else {
                    format!("{program}: {detail}")
                });
            }

            // Mounting is asynchronous, and the wait is not the network's -
            // it is a person's. `open` hands the URL to the desktop and
            // returns at once; what follows is a dialog asking for a
            // password and then, often, a list of shares to choose from.
            // Ten seconds was a guess about a machine and gave up while the
            // reader was still typing, reporting a failure for a mount that
            // then completed a moment later.
            let deadline = Instant::now() + Duration::from_secs(90);
            while Instant::now() < deadline {
                if let Some(found) = find_mount_in(&roots, location) {
                    return Ok(with_subpath(found, location));
                }
                if let Some(h) = &hint {
                    if h.is_dir() {
                        return Ok(with_subpath(h.clone(), location));
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }

            // Said as what it is: not a refusal, and not necessarily over.
            // The desktop may still have a dialog open, and the share will
            // appear under /Volumes when it is answered.
            Err(format!(
                "{} has not appeared yet. If the desktop is still asking for \
                 a password, answer it - the share will mount on its own.",
                location.to_url()
            ))
        }
    }
}

/// Best-effort detach, used by the connections screen.
pub fn disconnect(path: &Path) -> Result<(), String> {
    let platform = Platform::current();
    let (program, args): (&str, Vec<String>) = match platform {
        Platform::MacOs => (
            "diskutil",
            vec!["unmount".into(), path.display().to_string()],
        ),
        Platform::Linux => (
            "gio",
            vec!["mount".into(), "-u".into(), path.display().to_string()],
        ),
        Platform::Windows => return Err("nothing to unmount for a UNC path".into()),
    };

    let output = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(url: &str) -> Location {
        Location::parse(url).unwrap()
    }

    // ---- Windows -----------------------------------------------------------

    #[test]
    fn windows_maps_smb_to_a_unc_path_with_no_command() {
        let plan = plan_for(Platform::Windows, &loc("smb://nas.local/media/movies"));
        assert_eq!(
            plan,
            MapPlan::Direct(PathBuf::from(r"\\nas.local\media\movies"))
        );
    }

    #[test]
    fn windows_unc_input_round_trips_back_to_unc() {
        let plan = plan_for(Platform::Windows, &loc(r"\\fileserver\share"));
        assert_eq!(plan, MapPlan::Direct(PathBuf::from(r"\\fileserver\share")));
    }

    #[test]
    fn windows_reports_ftp_as_unsupported_with_guidance() {
        match plan_for(Platform::Windows, &loc("ftp://example.org/pub")) {
            MapPlan::Unsupported { reason } => {
                assert!(reason.contains("FTP"), "{reason}");
                assert!(reason.contains("Explorer"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ---- macOS -------------------------------------------------------------

    #[test]
    fn macos_opens_smb_urls_and_expects_a_volume() {
        match plan_for(Platform::MacOs, &loc("smb://alex@nas.local/media")) {
            MapPlan::Command {
                program,
                args,
                hint,
            } => {
                assert_eq!(program, "open");
                assert_eq!(args, vec!["smb://alex@nas.local/media".to_string()]);
                assert_eq!(hint, Some(PathBuf::from("/Volumes/media")));
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn macos_handles_afp_and_nfs_the_same_way() {
        for url in ["afp://server/vol", "nfs://server/export"] {
            match plan_for(Platform::MacOs, &loc(url)) {
                MapPlan::Command { program, .. } => assert_eq!(program, "open"),
                other => panic!("expected Command for {url}, got {other:?}"),
            }
        }
    }

    #[test]
    fn macos_explains_why_ftp_and_sftp_cannot_be_mounted() {
        for url in ["ftp://example.org/pub", "sftp://host/srv"] {
            match plan_for(Platform::MacOs, &loc(url)) {
                MapPlan::Unsupported { reason } => {
                    assert!(reason.contains("macOS"), "{reason}");
                }
                other => panic!("expected Unsupported for {url}, got {other:?}"),
            }
        }
    }

    // ---- Linux -------------------------------------------------------------

    #[test]
    fn linux_uses_gio_for_smb_ftp_and_sftp() {
        for url in [
            "smb://nas.local/media",
            "ftp://example.org/pub",
            "sftp://build@ci.internal/srv",
        ] {
            match plan_for(Platform::Linux, &loc(url)) {
                MapPlan::Command { program, args, .. } => {
                    assert_eq!(program, "gio");
                    assert_eq!(args[0], "mount");
                    assert_eq!(args[1], url);
                }
                other => panic!("expected Command for {url}, got {other:?}"),
            }
        }
    }

    #[test]
    fn linux_reports_nfs_as_needing_privileges() {
        match plan_for(Platform::Linux, &loc("nfs://server/export")) {
            MapPlan::Unsupported { reason } => assert!(reason.contains("sudo"), "{reason}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ---- shared ------------------------------------------------------------

    #[test]
    fn local_locations_are_direct_on_every_platform() {
        let local = Location::local("/home/user/code");
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            assert_eq!(
                plan_for(platform, &local),
                MapPlan::Direct(PathBuf::from("/home/user/code"))
            );
        }
    }

    // gvfs names a mount `smb-share:server=nas.local,share=media`. The colon
    // is not a legal character in a Windows filename, so the fixture cannot be
    // created there at all - and need not be: `candidate_roots(Windows)` is
    // empty because Windows reaches a share by UNC path and never mounts it.
    #[cfg(unix)]
    #[test]
    fn finds_a_gvfs_style_mount_point() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir(root.join("smb-share:server=nas.local,share=media")).unwrap();
        std::fs::create_dir(root.join("unrelated")).unwrap();

        let found =
            find_mount_in(std::slice::from_ref(&root), &loc("smb://nas.local/media")).unwrap();
        assert!(found.to_string_lossy().contains("nas.local"));
    }

    #[test]
    fn finds_a_macos_style_volume_named_after_the_share() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir(root.join("media")).unwrap();

        let found =
            find_mount_in(std::slice::from_ref(&root), &loc("smb://nas.local/media")).unwrap();
        assert_eq!(found, root.join("media"));
    }

    // Unix-only for the same reason as `finds_a_gvfs_style_mount_point`: the
    // whole point is a gvfs-shaped name, which contains a colon.
    #[cfg(unix)]
    #[test]
    fn prefers_the_mount_that_matches_both_host_and_share() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir(root.join("nas.local")).unwrap(); // host only
        std::fs::create_dir(root.join("smb-share:server=nas.local,share=media")).unwrap();

        let found =
            find_mount_in(std::slice::from_ref(&root), &loc("smb://nas.local/media")).unwrap();
        assert!(found.to_string_lossy().contains("share=media"));
    }

    #[test]
    fn a_mount_naming_both_host_and_share_beats_one_naming_either() {
        // The same ranking as the gvfs test above, spelled with names that are
        // legal on every platform, so the scoring itself stays covered where
        // `smb-share:server=...` cannot be created.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir(root.join("nas.local")).unwrap(); // host only
        std::fs::create_dir(root.join("media")).unwrap(); // share only
        std::fs::create_dir(root.join("nas.local-media")).unwrap(); // both

        let found =
            find_mount_in(std::slice::from_ref(&root), &loc("smb://nas.local/media")).unwrap();
        assert_eq!(found, root.join("nas.local-media"));
    }

    #[test]
    fn returns_nothing_when_no_mount_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("somethingelse")).unwrap();
        assert!(
            find_mount_in(&[dir.path().to_path_buf()], &loc("smb://nas.local/media")).is_none()
        );
    }

    #[test]
    fn local_locations_are_never_matched_against_mount_points() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_mount_in(&[dir.path().to_path_buf()], &Location::local("/tmp")).is_none());
    }

    #[test]
    fn subpath_is_appended_below_the_share() {
        let base = PathBuf::from("/Volumes/media");
        assert_eq!(
            with_subpath(base.clone(), &loc("smb://nas.local/media/movies/2024")),
            PathBuf::from("/Volumes/media/movies/2024")
        );
        assert_eq!(
            with_subpath(base.clone(), &loc("smb://nas.local/media")),
            base
        );
    }

    /// The line the reporter's own Mac printed while the sheet sat waiting.
    const REAL: &str = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
//adm@nas10/public on /Volumes/public (smbfs, nodev, nosuid, quarantine, mounted by alex)\n\
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)";

    #[test]
    fn a_share_picked_from_the_desktops_list_is_found() {
        // The bug: typing just the server and choosing a share in the
        // desktop's own list mounted it at /Volumes/public - a name holding
        // neither the host that was typed nor any share, since none was -
        // so the name-matching found nothing and the wait ran out on a
        // share that was already there.
        let table = parse_mount_table(REAL);
        assert_eq!(table.len(), 3);

        let server_only = Location::parse("smb://nas10").unwrap();
        let mine = table
            .iter()
            .find(|(from, _)| source_belongs_to(from, &server_only));
        assert_eq!(mine.map(|(_, on)| on.display().to_string()).as_deref(),
                   Some("/Volumes/public"));
    }

    #[test]
    fn a_named_share_matches_only_itself() {
        let table = parse_mount_table(REAL);
        let asked = Location::parse("smb://nas10/public").unwrap();
        assert!(table.iter().any(|(from, _)| source_belongs_to(from, &asked)));

        // A share of the same host that is not mounted must not match the
        // one that is - "connected" would then be a lie about the wrong
        // folder, and the pane would land somewhere nobody asked for.
        let other = Location::parse("smb://nas10/private").unwrap();
        assert!(!table.iter().any(|(from, _)| source_belongs_to(from, &other)));

        // And a different server entirely never matches.
        let elsewhere = Location::parse("smb://otherbox/public").unwrap();
        assert!(!table.iter().any(|(from, _)| source_belongs_to(from, &elsewhere)));
    }

    #[test]
    fn the_short_name_and_the_long_one_are_the_same_host() {
        // Typed as nas10.local, mounted as nas10 - or the other way about.
        let table = parse_mount_table(REAL);
        let dotted = Location::parse("smb://nas10.local/public").unwrap();
        assert!(table.iter().any(|(from, _)| source_belongs_to(from, &dotted)));
    }

    #[test]
    fn a_mount_point_with_a_space_survives_the_parse() {
        let table = parse_mount_table(
            "//alex@nas10/Shared Folder on /Volumes/Shared Folder (smbfs, nodev)",
        );
        assert_eq!(table[0].1.display().to_string(), "/Volumes/Shared Folder");
    }

    #[test]
    fn naming_a_server_alone_is_a_request_to_choose() {
        // Reported: the first connection asked which share, the second went
        // straight in without asking - the same address behaving two ways.
        // A named share may be reused; a bare server may not, because
        // reusing one means choosing for the reader.
        let bare = Location::parse("smb://nas10").unwrap();
        assert!(bare.share().is_empty(), "a server alone names no share");

        let named = Location::parse("smb://nas10/alex").unwrap();
        assert_eq!(named.share(), "alex");
    }
}
