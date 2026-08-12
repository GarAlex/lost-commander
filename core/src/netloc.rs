// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network locations: parsing, and the on-disk bookmark store ("remember").
//!
//! Passwords are deliberately **never** stored. Only the user name is kept, and
//! authentication is delegated to the operating system's own credential store
//! (Keychain on macOS, gvfs/kwallet on Linux, Credential Manager on Windows),
//! which is both more secure and less surprising than a private password file.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Local,
    Smb,
    Ftp,
    Sftp,
    Nfs,
    Afp,
}

impl Protocol {
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Local => "file",
            Protocol::Smb => "smb",
            Protocol::Ftp => "ftp",
            Protocol::Sftp => "sftp",
            Protocol::Nfs => "nfs",
            Protocol::Afp => "afp",
        }
    }

    pub fn from_scheme(scheme: &str) -> Option<Self> {
        match scheme.to_ascii_lowercase().as_str() {
            "file" => Some(Protocol::Local),
            "smb" | "cifs" => Some(Protocol::Smb),
            "ftp" => Some(Protocol::Ftp),
            "sftp" | "ssh" => Some(Protocol::Sftp),
            "nfs" => Some(Protocol::Nfs),
            "afp" => Some(Protocol::Afp),
            _ => None,
        }
    }

    pub fn is_network(self) -> bool {
        self != Protocol::Local
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Local => "local",
            Protocol::Smb => "SMB",
            Protocol::Ftp => "FTP",
            Protocol::Sftp => "SFTP",
            Protocol::Nfs => "NFS",
            Protocol::Afp => "AFP",
        }
    }
}

/// A saved place: either a plain directory or a network share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub name: String,
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// For `Local` this is the filesystem path; for network protocols it is
    /// `share/subdirectory`.
    #[serde(default)]
    pub path: String,
}

impl Location {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        let path: PathBuf = path.into();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        Location {
            name,
            protocol: Protocol::Local,
            user: None,
            host: String::new(),
            port: None,
            path: path.display().to_string(),
        }
    }

    /// Accepts `smb://user@host/share/sub`, a Windows UNC path
    /// `\\host\share`, or any plain filesystem path.
    pub fn parse(input: &str) -> Result<Location, String> {
        let raw = input.trim();
        if raw.is_empty() {
            return Err("empty location".into());
        }

        // Windows UNC form.
        if let Some(rest) = raw.strip_prefix(r"\\") {
            let normalised = rest.replace('\\', "/");
            let mut parts = normalised.splitn(2, '/');
            let host = parts.next().unwrap_or_default().to_string();
            if host.is_empty() {
                return Err("UNC path is missing a host".into());
            }
            let path = parts
                .next()
                .unwrap_or_default()
                .trim_matches('/')
                .to_string();
            let mut location = Location {
                name: String::new(),
                protocol: Protocol::Smb,
                user: None,
                host,
                port: None,
                path,
            };
            location.name = location.default_name();
            return Ok(location);
        }

        if let Some(index) = raw.find("://") {
            let scheme = &raw[..index];
            let rest = &raw[index + 3..];
            let protocol = Protocol::from_scheme(scheme)
                .ok_or_else(|| format!("unsupported scheme: {scheme}"))?;

            if protocol == Protocol::Local {
                return Ok(Location::local(rest));
            }

            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], rest[i + 1..].trim_matches('/')),
                None => (rest, ""),
            };

            let (user, host_port) = match authority.rfind('@') {
                Some(i) => (Some(authority[..i].to_string()), &authority[i + 1..]),
                None => (None, authority),
            };

            // Note: bracketed IPv6 literals are not handled yet.
            let (host, port) = match host_port.rfind(':') {
                Some(i) => {
                    let port = host_port[i + 1..]
                        .parse::<u16>()
                        .map_err(|_| format!("invalid port in {host_port}"))?;
                    (host_port[..i].to_string(), Some(port))
                }
                None => (host_port.to_string(), None),
            };

            if host.is_empty() {
                return Err("location is missing a host".into());
            }

            let mut location = Location {
                name: String::new(),
                protocol,
                user,
                host,
                port,
                path: path.to_string(),
            };
            location.name = location.default_name();
            return Ok(location);
        }

        Ok(Location::local(raw))
    }

    /// The share name (first path component) for network protocols.
    pub fn share(&self) -> &str {
        self.path.split('/').next().unwrap_or("")
    }

    /// Everything below the share.
    pub fn subpath(&self) -> &str {
        match self.path.find('/') {
            Some(i) => &self.path[i + 1..],
            None => "",
        }
    }

    pub fn default_name(&self) -> String {
        if self.protocol == Protocol::Local {
            return Path::new(&self.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.path.clone());
        }
        if self.path.is_empty() {
            self.host.clone()
        } else {
            format!("{}/{}", self.host, self.path)
        }
    }

    pub fn to_url(&self) -> String {
        if self.protocol == Protocol::Local {
            return self.path.clone();
        }
        let user = self
            .user
            .as_ref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let path = if self.path.is_empty() {
            String::new()
        } else {
            format!("/{}", self.path)
        };
        format!(
            "{}://{user}{}{port}{path}",
            self.protocol.scheme(),
            self.host
        )
    }

    /// The same location, spelled the way a URL parser needs it.
    ///
    /// `to_url` is for reading and for saving: it keeps the spaces, because
    /// "smb://nas/Shared Folder" is what a person typed and what they should
    /// see in a rail. Handing that to the operating system is a different
    /// matter - LaunchServices parses its argument as a URL, a raw space
    /// ends the URL early, and macOS reports the wreckage as "Check the
    /// server name or IP address", which sends the reader off to look at a
    /// server that was never the problem.
    ///
    /// A Windows domain login has the same trouble with its backslash.
    ///
    /// Percent-encoded per component: the separators must survive as
    /// separators, so the slashes between path segments and the @ before the
    /// host are written by this and never by the encoder.
    pub fn os_url(&self) -> String {
        if self.protocol == Protocol::Local {
            return self.path.clone();
        }
        let user = self
            .user
            .as_ref()
            .map(|u| format!("{}@", encoded(u)))
            .unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let path = if self.path.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = self.path.split('/').map(|p| encoded(p)).collect();
            format!("/{}", parts.join("/"))
        };
        format!(
            "{}://{user}{}{port}{path}",
            self.protocol.scheme(),
            self.host
        )
    }

    /// One-line description for the connections list.
    pub fn summary(&self) -> String {
        format!("{:<6} {}", self.protocol.label(), self.to_url())
    }
}

/// One path component or user name, percent-encoded.
///
/// Written here rather than taken from a crate: this is the whole of what is
/// needed, the rules are RFC 3986's unreserved set, and a dependency that
/// crosses into the FFI is a dependency two front-ends inherit.
fn encoded(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// How many visited directories are kept.
pub const MAX_RECENT: usize = 20;

/// The places you saved, and the places you have been.
///
/// Two lists, two files. A bookmark is something you chose and expect to
/// find again; a recent location is a side effect of walking around, and it
/// changes on nearly every keystroke. Keeping them in one file meant every
/// step rewrote the file holding the things you had deliberately saved,
/// which is a great deal of writing to risk somebody's bookmarks on.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Bookmarks {
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
    /// Most recent first.
    ///
    /// Read from an old `bookmarks.toml` that still has it, so nobody's list
    /// disappears on the upgrade, but never written back there: it belongs to
    /// [`Bookmarks::recent_path`] now.
    #[serde(default, rename = "recent", skip_serializing)]
    pub recent: Vec<Location>,
}

/// The recent list on its own, which is all its file holds.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default, rename = "recent")]
    recent: Vec<Location>,
}

impl Bookmarks {
    /// `~/.config/lost-commander/bookmarks.toml` and the platform equivalents.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("lost-commander").join("bookmarks.toml"))
    }

    /// Never fails: a missing or unreadable file simply means "no bookmarks".
    ///
    /// Reads both files. An old `bookmarks.toml` with a recent list inside it
    /// still gives up its recents; a `recent.toml` beside it wins, because it
    /// is the one being kept up to date.
    pub fn load() -> Self {
        let mut bookmarks = Self::config_path()
            .and_then(|p| Self::load_from(&p).ok())
            .unwrap_or_default();
        if let Some(path) = Self::recent_path() {
            bookmarks.load_recent_from(&path);
        }
        bookmarks
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// `~/.config/lost-commander/recent.toml` and the platform equivalents.
    pub fn recent_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("lost-commander").join("recent.toml"))
    }

    /// Read the recent list, keeping whatever an old bookmarks file had if
    /// there is no separate file yet.
    pub fn load_recent_from(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        if let Ok(file) = toml::from_str::<RecentFile>(&text) {
            self.recent = file.recent;
        }
    }

    pub fn save_recent_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = RecentFile {
            recent: self.recent.clone(),
        };
        let text = toml::to_string_pretty(&file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, text)
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, text)
    }

    /// Adding a name that already exists replaces it, so re-saving a location
    /// updates it instead of creating duplicates.
    pub fn add(&mut self, location: Location) {
        match self.locations.iter_mut().find(|l| l.name == location.name) {
            Some(existing) => *existing = location,
            None => self.locations.push(location),
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<Location> {
        if index < self.locations.len() {
            Some(self.locations.remove(index))
        } else {
            None
        }
    }

    /// Used by the test suite; the UI checks the list it is drawing instead.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// Record a visit. Re-visiting somewhere moves it back to the top rather
    /// than adding a duplicate, and the list is capped at [`MAX_RECENT`].
    pub fn push_recent(&mut self, location: Location) {
        let url = location.to_url();
        self.recent.retain(|l| l.to_url() != url);
        self.recent.insert(0, location);
        self.recent.truncate(MAX_RECENT);
    }

    pub fn remove_recent(&mut self, index: usize) -> Option<Location> {
        if index < self.recent.len() {
            Some(self.recent.remove(index))
        } else {
            None
        }
    }

    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_smb_url_with_user_and_subpath() {
        let l = Location::parse("smb://alex@nas.local/media/movies").unwrap();
        assert_eq!(l.protocol, Protocol::Smb);
        assert_eq!(l.user.as_deref(), Some("alex"));
        assert_eq!(l.host, "nas.local");
        assert_eq!(l.path, "media/movies");
        assert_eq!(l.share(), "media");
        assert_eq!(l.subpath(), "movies");
        assert_eq!(l.port, None);
    }

    #[test]
    fn parses_ftp_with_an_explicit_port() {
        let l = Location::parse("ftp://files.example.com:2121/pub").unwrap();
        assert_eq!(l.protocol, Protocol::Ftp);
        assert_eq!(l.host, "files.example.com");
        assert_eq!(l.port, Some(2121));
        assert_eq!(l.path, "pub");
        assert!(l.user.is_none());
    }

    #[test]
    fn parses_a_windows_unc_path_as_smb() {
        let l = Location::parse(r"\\fileserver\share\deep").unwrap();
        assert_eq!(l.protocol, Protocol::Smb);
        assert_eq!(l.host, "fileserver");
        assert_eq!(l.path, "share/deep");
        assert_eq!(l.share(), "share");
    }

    #[test]
    fn parses_a_plain_path_as_local() {
        let l = Location::parse("/home/user/projects").unwrap();
        assert_eq!(l.protocol, Protocol::Local);
        assert_eq!(l.path, "/home/user/projects");
        assert_eq!(l.name, "projects");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(Location::parse("").is_err());
        assert!(Location::parse("gopher://example.com/x").is_err());
        assert!(Location::parse("smb://").is_err());
        assert!(Location::parse("smb://host:notaport/share").is_err());
    }

    #[test]
    fn url_round_trips() {
        for raw in [
            "smb://alex@nas.local/media/movies",
            "ftp://files.example.com:2121/pub",
            "sftp://build@ci.internal/srv",
            "smb://nas.local",
        ] {
            let parsed = Location::parse(raw).unwrap();
            assert_eq!(parsed.to_url(), raw, "round trip failed for {raw}");
        }
    }

    #[test]
    fn default_names_are_readable() {
        assert_eq!(
            Location::parse("smb://alex@nas.local/media").unwrap().name,
            "nas.local/media"
        );
        assert_eq!(
            Location::parse("smb://nas.local").unwrap().name,
            "nas.local"
        );
    }

    #[test]
    fn bookmarks_survive_a_save_and_load_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("bookmarks.toml");

        let mut marks = Bookmarks::default();
        marks.add(Location::parse("smb://alex@nas.local/media").unwrap());
        marks.add(Location::parse("ftp://ftp.example.org/pub").unwrap());
        marks.add(Location::local("/home/user/code"));
        marks.save_to(&path).unwrap();

        // The parent directory is created on demand.
        assert!(path.exists());

        let loaded = Bookmarks::load_from(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.locations[0].host, "nas.local");
        assert_eq!(loaded.locations[0].user.as_deref(), Some("alex"));
        assert_eq!(loaded.locations[1].protocol, Protocol::Ftp);
        assert_eq!(loaded.locations[2].protocol, Protocol::Local);
    }

    #[test]
    fn saving_the_same_name_updates_rather_than_duplicates() {
        let mut marks = Bookmarks::default();
        let mut first = Location::parse("smb://nas.local/media").unwrap();
        first.name = "NAS".into();
        marks.add(first);

        let mut second = Location::parse("smb://nas.local/backups").unwrap();
        second.name = "NAS".into();
        marks.add(second);

        assert_eq!(marks.len(), 1);
        assert_eq!(marks.locations[0].path, "backups");
    }

    #[test]
    fn removing_by_index_works_and_is_bounds_checked() {
        let mut marks = Bookmarks::default();
        marks.add(Location::parse("smb://a.local/x").unwrap());
        marks.add(Location::parse("smb://b.local/y").unwrap());

        let removed = marks.remove(0).unwrap();
        assert_eq!(removed.host, "a.local");
        assert_eq!(marks.len(), 1);
        assert!(marks.remove(99).is_none());
    }

    #[test]
    fn a_missing_or_corrupt_file_yields_no_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Bookmarks::load_from(&dir.path().join("nope.toml")).is_err());

        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "this is not = valid toml [[[").unwrap();
        assert!(Bookmarks::load_from(&broken).is_err());
    }

    #[test]
    fn recent_keeps_the_newest_first_without_duplicates() {
        let mut marks = Bookmarks::default();
        marks.push_recent(Location::local("/a"));
        marks.push_recent(Location::local("/b"));
        marks.push_recent(Location::local("/c"));
        assert_eq!(
            marks
                .recent
                .iter()
                .map(|l| l.path.clone())
                .collect::<Vec<_>>(),
            ["/c", "/b", "/a"]
        );

        // Re-visiting moves it back to the top instead of duplicating.
        marks.push_recent(Location::local("/a"));
        assert_eq!(
            marks
                .recent
                .iter()
                .map(|l| l.path.clone())
                .collect::<Vec<_>>(),
            ["/a", "/c", "/b"]
        );
        assert_eq!(marks.recent.len(), 3);
    }

    #[test]
    fn recent_is_capped() {
        let mut marks = Bookmarks::default();
        for i in 0..(MAX_RECENT + 10) {
            marks.push_recent(Location::local(format!("/dir{i}")));
        }
        assert_eq!(marks.recent.len(), MAX_RECENT);
        // The newest survived, the oldest fell off.
        assert_eq!(marks.recent[0].path, format!("/dir{}", MAX_RECENT + 9));
        assert!(!marks.recent.iter().any(|l| l.path == "/dir0"));
    }

    #[test]
    fn recent_distinguishes_network_locations_by_url() {
        let mut marks = Bookmarks::default();
        marks.push_recent(Location::parse("smb://nas.local/media").unwrap());
        marks.push_recent(Location::parse("smb://nas.local/backups").unwrap());
        assert_eq!(marks.recent.len(), 2);

        marks.push_recent(Location::parse("smb://nas.local/media").unwrap());
        assert_eq!(marks.recent.len(), 2);
        assert_eq!(marks.recent[0].path, "media");
    }

    #[test]
    fn bookmarks_and_recents_are_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let marks_at = dir.path().join("bookmarks.toml");
        let recent_at = dir.path().join("recent.toml");

        let mut marks = Bookmarks::default();
        marks.add(Location::parse("smb://nas.local/media").unwrap());
        marks.push_recent(Location::local("/home/user/code"));
        marks.push_recent(Location::parse("smb://nas.local/media/movies").unwrap());
        marks.save_to(&marks_at).unwrap();
        marks.save_recent_to(&recent_at).unwrap();

        // What you chose to save is not rewritten every time you walk into a
        // directory, which is the point of the split.
        let saved = Bookmarks::load_from(&marks_at).unwrap();
        assert_eq!(saved.len(), 1);
        assert!(
            saved.recent.is_empty(),
            "the recent list is not in the bookmarks file any more"
        );

        let mut read = Bookmarks::load_from(&marks_at).unwrap();
        read.load_recent_from(&recent_at);
        assert_eq!(read.recent.len(), 2);
        assert_eq!(read.recent[0].protocol, Protocol::Smb);
        assert_eq!(read.recent[1].path, "/home/user/code");
    }

    #[test]
    fn an_old_bookmarks_file_still_gives_up_its_recents() {
        // Written by a version that kept both in one file. Nobody's list of
        // where they have been should disappear on an upgrade.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.toml");
        std::fs::write(
            &path,
            "[[location]]
name = \"nas\"
protocol = \"smb\"
host = \"nas.local\"
path = \"media\"

             [[recent]]
name = \"code\"
protocol = \"local\"
path = \"/home/user/code\"
",
        )
        .unwrap();

        let read = Bookmarks::load_from(&path).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read.recent.len(), 1, "read from the old file");

        // And never written back there: the next save leaves the bookmarks
        // file holding bookmarks alone.
        let again = dir.path().join("rewritten.toml");
        read.save_to(&again).unwrap();
        assert!(Bookmarks::load_from(&again).unwrap().recent.is_empty());
    }

    #[test]
    fn removing_and_clearing_recent() {
        let mut marks = Bookmarks::default();
        marks.push_recent(Location::local("/a"));
        marks.push_recent(Location::local("/b"));

        assert_eq!(marks.remove_recent(0).unwrap().path, "/b");
        assert_eq!(marks.recent.len(), 1);
        assert!(marks.remove_recent(9).is_none());

        marks.clear_recent();
        assert!(marks.recent.is_empty());
    }

    #[test]
    fn old_files_without_a_recent_section_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.toml");
        std::fs::write(
            &path,
            "[[location]]\nname = \"NAS\"\nprotocol = \"smb\"\nhost = \"nas.local\"\npath = \"media\"\n",
        )
        .unwrap();

        let loaded = Bookmarks::load_from(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.recent.is_empty());
    }

    #[test]
    fn passwords_are_never_written_to_disk() {
        // The struct has no password field at all; this test documents that
        // decision and guards against it being added carelessly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.toml");
        let mut marks = Bookmarks::default();
        marks.add(Location::parse("smb://alex@nas.local/media").unwrap());
        marks.save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
        assert!(!text.contains("password"));
        assert!(!text.contains("passwd"));
        assert!(!text.contains("secret"));
    }


    #[test]
    fn what_the_operating_system_is_handed_is_a_url() {
        // The bug this pins, reported from a real connection: a share with a
        // space in its name went to macOS with the space in it, which is not
        // a URL, and the refusal came back as "Check the server name or IP
        // address" - pointing at a server that was never the problem.
        let share = Location::parse("smb://nas10/Shared Folder").unwrap();
        assert_eq!(share.os_url(), "smb://nas10/Shared%20Folder");
        // And what a person reads keeps its space, because that is what they
        // typed and what a rail should show.
        assert_eq!(share.to_url(), "smb://nas10/Shared Folder");

        // A Windows domain login has the same trouble with its backslash.
        let domain = Location::parse("smb://DOMAIN\\alex@nas10/team").unwrap();
        assert_eq!(domain.os_url(), "smb://DOMAIN%5Calex@nas10/team");

        // Separators survive as separators - the encoder never sees them.
        let deep = Location::parse("smb://nas10/public/sub dir/one").unwrap();
        assert_eq!(deep.os_url(), "smb://nas10/public/sub%20dir/one");

        // Ordinary names are left exactly alone, so nothing that worked
        // before now looks different.
        let plain = Location::parse("smb://nas10/team").unwrap();
        assert_eq!(plain.os_url(), plain.to_url());
    }
}
