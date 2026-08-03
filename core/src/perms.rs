// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a file is, beyond its name: permissions, ownership, dates.
//!
//! Two platforms with genuinely different models, kept apart rather than
//! flattened into a lie:
//!
//! * **Unix** has the twelve permission bits - read, write and execute for
//!   owner, group and other, plus setuid, setgid and the sticky bit - and an
//!   owning user and group.
//! * **Windows** has none of those. It has a read-only flag, and hidden,
//!   system and archive attributes, and its real access control is ACLs, which
//!   are not a thing a checkbox grid can honestly represent.
//!
//! So [`Properties`] carries what the platform it was read on actually has,
//! and the parts that are absent are `None` rather than zero.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::entry::EntryKind;

/// The set-user-ID bit: run as the file's owner rather than as whoever ran it.
pub const SETUID: u32 = 0o4000;
/// The set-group-ID bit. On a directory, new entries inherit its group.
pub const SETGID: u32 = 0o2000;
/// The sticky bit. On a directory, only an entry's owner may remove it -
/// which is what makes `/tmp` shared without being a free-for-all.
pub const STICKY: u32 = 0o1000;

/// Whose permission a bit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Owner,
    Group,
    Other,
}

/// Which permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Read,
    Write,
    Execute,
}

impl Who {
    pub const ALL: [Who; 3] = [Who::Owner, Who::Group, Who::Other];

    pub fn label(self) -> &'static str {
        match self {
            Who::Owner => "owner",
            Who::Group => "group",
            Who::Other => "other",
        }
    }

    /// How far the triple is shifted: owner is the top three bits of nine.
    fn shift(self) -> u32 {
        match self {
            Who::Owner => 6,
            Who::Group => 3,
            Who::Other => 0,
        }
    }
}

impl What {
    pub const ALL: [What; 3] = [What::Read, What::Write, What::Execute];

    pub fn label(self) -> &'static str {
        match self {
            What::Read => "read",
            What::Write => "write",
            What::Execute => "execute",
        }
    }

    fn bit(self) -> u32 {
        match self {
            What::Read => 0b100,
            What::Write => 0b010,
            What::Execute => 0b001,
        }
    }
}

/// Unix permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mode(u32);

impl Mode {
    /// Keep only the permission bits: the rest of `st_mode` is the file type,
    /// which is not something to be edited by a checkbox.
    pub fn from_bits(bits: u32) -> Mode {
        Mode(bits & 0o7777)
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    pub fn is_set(self, who: Who, what: What) -> bool {
        self.0 & (what.bit() << who.shift()) != 0
    }

    pub fn set(&mut self, who: Who, what: What, on: bool) {
        let bit = what.bit() << who.shift();
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    pub fn has(self, special: u32) -> bool {
        self.0 & special != 0
    }

    pub fn set_special(&mut self, special: u32, on: bool) {
        if on {
            self.0 |= special;
        } else {
            self.0 &= !special;
        }
    }

    /// `rwxr-xr-x`, the nine characters `ls` writes.
    ///
    /// The special bits are not a tenth column: they replace the execute
    /// character of the triple they belong to, in capitals when the execute
    /// bit underneath is *not* set - which is how you can tell `rws` from
    /// `rwS` and know the second one is almost certainly a mistake.
    pub fn symbolic(self) -> String {
        let mut out = String::with_capacity(9);
        for who in Who::ALL {
            out.push(if self.is_set(who, What::Read) {
                'r'
            } else {
                '-'
            });
            out.push(if self.is_set(who, What::Write) {
                'w'
            } else {
                '-'
            });

            let execute = self.is_set(who, What::Execute);
            let special = match who {
                Who::Owner => self.has(SETUID).then_some(('s', 'S')),
                Who::Group => self.has(SETGID).then_some(('s', 'S')),
                Who::Other => self.has(STICKY).then_some(('t', 'T')),
            };
            out.push(match (special, execute) {
                (Some((set, _)), true) => set,
                (Some((_, unset)), false) => unset,
                (None, true) => 'x',
                (None, false) => '-',
            });
        }
        out
    }

    /// `755`, or `4755` when any of the special bits is set.
    pub fn octal(self) -> String {
        if self.0 & 0o7000 != 0 {
            format!("{:04o}", self.0)
        } else {
            format!("{:03o}", self.0 & 0o777)
        }
    }

    /// Read `755`, `0755` or `4755`. `None` when it is not one of those.
    pub fn parse_octal(text: &str) -> Option<Mode> {
        let text = text.trim();
        if text.is_empty() || text.len() > 4 || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return None;
        }
        u32::from_str_radix(text, 8).ok().map(Mode::from_bits)
    }
}

/// The kind character `ls` puts before the permissions.
pub fn kind_char(kind: EntryKind, is_symlink: bool) -> char {
    if is_symlink {
        return 'l';
    }
    match kind {
        EntryKind::Dir | EntryKind::Parent => 'd',
        _ => '-',
    }
}

/// Everything the properties dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Properties {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub is_symlink: bool,
    /// Where a symlink points, which is the thing you opened it to find out.
    pub link_target: Option<PathBuf>,
    /// Unix only.
    pub mode: Option<Mode>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// Everywhere, and the only one Windows has that is worth a checkbox.
    pub readonly: bool,
}

impl Properties {
    /// The name to put at the top of the dialog.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Read everything about `path`.
///
/// `symlink_metadata`, so a link's own permissions and size are reported
/// rather than its target's - the dialog is about the file you selected.
pub fn read(path: &Path) -> io::Result<Properties> {
    let metadata = path.symlink_metadata()?;
    let is_symlink = metadata.file_type().is_symlink();

    let kind = if metadata.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };

    Ok(Properties {
        path: path.to_path_buf(),
        kind,
        size: metadata.len(),
        modified: metadata.modified().ok(),
        accessed: metadata.accessed().ok(),
        created: metadata.created().ok(),
        is_symlink,
        link_target: is_symlink.then(|| std::fs::read_link(path).ok()).flatten(),
        mode: mode_of(&metadata),
        owner: owner_of(&metadata),
        group: group_of(&metadata),
        readonly: metadata.permissions().readonly(),
    })
}

#[cfg(unix)]
fn mode_of(metadata: &std::fs::Metadata) -> Option<Mode> {
    use std::os::unix::fs::PermissionsExt;
    Some(Mode::from_bits(metadata.permissions().mode()))
}

#[cfg(not(unix))]
fn mode_of(_metadata: &std::fs::Metadata) -> Option<Mode> {
    None
}

#[cfg(unix)]
fn owner_of(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(name_for(metadata.uid(), "/etc/passwd"))
}

#[cfg(not(unix))]
fn owner_of(_metadata: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn group_of(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(name_for(metadata.gid(), "/etc/group"))
}

#[cfg(not(unix))]
fn group_of(_metadata: &std::fs::Metadata) -> Option<String> {
    None
}

/// A user or group name for an id, from a `passwd`-format file.
///
/// Both files have the name first and the numeric id third, which is all this
/// needs. Reading them directly means no C library call and no dependency -
/// at the price of only knowing *local* accounts: a network directory answers
/// through NSS, which this does not go through. An id with no local entry is
/// shown as the number, which is what `ls -n` would say anyway.
#[cfg(unix)]
fn name_for(id: u32, file: &str) -> String {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| name_in(&text, id))
        .unwrap_or_else(|| id.to_string())
}

/// The pure half of [`name_for`].
pub fn name_in(text: &str, id: u32) -> Option<String> {
    for line in text.lines() {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next();
        let Some(number) = fields.next() else {
            continue;
        };
        if number.trim().parse::<u32>() == Ok(id) {
            return Some(name.to_string());
        }
    }
    None
}

/// Write new permissions.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: Mode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode.bits()))
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: Mode) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system has no permission bits",
    ))
}

/// Turn the read-only flag on or off, which every platform has.
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut permissions = path.metadata()?.permissions();
    permissions.set_readonly(readonly);
    std::fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nine_bits_read_the_way_ls_writes_them() {
        assert_eq!(Mode::from_bits(0o755).symbolic(), "rwxr-xr-x");
        assert_eq!(Mode::from_bits(0o644).symbolic(), "rw-r--r--");
        assert_eq!(Mode::from_bits(0o600).symbolic(), "rw-------");
        assert_eq!(Mode::from_bits(0o777).symbolic(), "rwxrwxrwx");
        assert_eq!(Mode::from_bits(0o000).symbolic(), "---------");
    }

    #[test]
    fn a_special_bit_replaces_an_execute_character_rather_than_adding_one() {
        // setuid with execute, and without - the capital is how you tell that
        // the second one is almost certainly a mistake.
        assert_eq!(Mode::from_bits(0o4755).symbolic(), "rwsr-xr-x");
        assert_eq!(Mode::from_bits(0o4644).symbolic(), "rwSr--r--");
        assert_eq!(Mode::from_bits(0o2755).symbolic(), "rwxr-sr-x");
        assert_eq!(Mode::from_bits(0o2644).symbolic(), "rw-r-Sr--");
        // /tmp, which is the sticky bit's whole reason for existing.
        assert_eq!(Mode::from_bits(0o1777).symbolic(), "rwxrwxrwt");
        assert_eq!(Mode::from_bits(0o1666).symbolic(), "rw-rw-rwT");
        // ...and all three at once still has nine characters.
        assert_eq!(Mode::from_bits(0o7777).symbolic().len(), 9);
    }

    #[test]
    fn the_octal_grows_a_digit_only_when_it_has_to() {
        assert_eq!(Mode::from_bits(0o755).octal(), "755");
        assert_eq!(Mode::from_bits(0o007).octal(), "007");
        assert_eq!(Mode::from_bits(0o4755).octal(), "4755");
        assert_eq!(Mode::from_bits(0o1777).octal(), "1777");
    }

    #[test]
    fn the_octal_reads_back_what_it_wrote() {
        for bits in [0o755, 0o644, 0o000, 0o777, 0o4755, 0o2750, 0o1777, 0o7777] {
            let mode = Mode::from_bits(bits);
            assert_eq!(Mode::parse_octal(&mode.octal()), Some(mode), "{bits:o}");
        }
        // Leading zero, as anyone types it.
        assert_eq!(Mode::parse_octal("0755"), Some(Mode::from_bits(0o755)));
    }

    #[test]
    fn what_is_not_an_octal_mode_is_refused() {
        for text in ["", "  ", "8", "99", "75x", "12345", "-1", "0o755"] {
            assert_eq!(Mode::parse_octal(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_bit_can_be_read_set_and_cleared() {
        let mut mode = Mode::from_bits(0o644);
        assert!(mode.is_set(Who::Owner, What::Read));
        assert!(mode.is_set(Who::Owner, What::Write));
        assert!(!mode.is_set(Who::Owner, What::Execute));
        assert!(!mode.is_set(Who::Other, What::Write));

        mode.set(Who::Owner, What::Execute, true);
        assert_eq!(mode.octal(), "744");
        mode.set(Who::Other, What::Read, false);
        assert_eq!(mode.octal(), "740");
        // Setting what is already set changes nothing.
        mode.set(Who::Owner, What::Execute, true);
        assert_eq!(mode.octal(), "740");
    }

    #[test]
    fn every_bit_of_the_grid_is_its_own() {
        // Nine checkboxes, nine bits, no two the same.
        let mut seen = Vec::new();
        for who in Who::ALL {
            for what in What::ALL {
                let mut mode = Mode::default();
                mode.set(who, what, true);
                assert!(!seen.contains(&mode.bits()), "{who:?}/{what:?} collided");
                seen.push(mode.bits());
            }
        }
        assert_eq!(seen.len(), 9);
        assert_eq!(seen.iter().fold(0, |a, b| a | b), 0o777);
    }

    #[test]
    fn the_special_bits_are_their_own_too() {
        let mut mode = Mode::from_bits(0o755);
        assert!(!mode.has(SETUID));
        mode.set_special(SETUID, true);
        assert_eq!(mode.octal(), "4755");
        mode.set_special(STICKY, true);
        assert_eq!(mode.octal(), "5755");
        mode.set_special(SETUID, false);
        assert_eq!(mode.octal(), "1755");
    }

    #[test]
    fn the_file_type_is_not_part_of_the_permissions() {
        // st_mode carries the type in its high bits; editing those with a
        // checkbox would turn a file into something else.
        let regular = 0o100_644; // S_IFREG | 0644
        assert_eq!(Mode::from_bits(regular).octal(), "644");
        let directory = 0o040_755; // S_IFDIR | 0755
        assert_eq!(Mode::from_bits(directory).octal(), "755");
    }

    #[test]
    fn the_kind_character_is_the_one_ls_uses() {
        assert_eq!(kind_char(EntryKind::File, false), '-');
        assert_eq!(kind_char(EntryKind::Dir, false), 'd');
        // A link is a link whatever it points at.
        assert_eq!(kind_char(EntryKind::File, true), 'l');
        assert_eq!(kind_char(EntryKind::Dir, true), 'l');
    }

    #[test]
    fn a_name_is_looked_up_by_its_number() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
                      user:x:1000:1000:A User:/home/user:/bin/bash\n";
        assert_eq!(name_in(passwd, 0).as_deref(), Some("root"));
        assert_eq!(name_in(passwd, 1000).as_deref(), Some("user"));
        // An id with no local entry has no name here - the caller shows the
        // number, which is what `ls -n` would say anyway.
        assert_eq!(name_in(passwd, 4242), None);
    }

    #[test]
    fn a_malformed_passwd_line_is_stepped_over() {
        let passwd = "\n\
                      not-a-line\n\
                      broken:x\n\
                      user:x:1000:1000::/home/user:/bin/sh\n";
        assert_eq!(name_in(passwd, 1000).as_deref(), Some("user"));
        assert_eq!(name_in("", 0), None);
    }

    #[cfg(unix)]
    #[test]
    fn properties_are_read_from_the_file_and_written_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "hello").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();

        let properties = read(&file).unwrap();
        assert_eq!(properties.name(), "notes.txt");
        assert_eq!(properties.size, 5);
        assert_eq!(properties.kind, EntryKind::File);
        assert!(!properties.is_symlink);
        assert_eq!(properties.mode.unwrap().octal(), "640");
        assert!(properties.owner.is_some());
        assert!(properties.modified.is_some());

        set_mode(&file, Mode::from_bits(0o600)).unwrap();
        assert_eq!(read(&file).unwrap().mode.unwrap().octal(), "600");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_reports_itself_and_where_it_points() {
        // Not its target: the dialog is about the file that was selected.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, "a longer file than the link").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let properties = read(&link).unwrap();
        assert!(properties.is_symlink);
        assert_eq!(properties.link_target.as_deref(), Some(target.as_path()));
        assert_ne!(properties.size, 27, "it reported the target's size");
    }

    #[test]
    fn reading_something_that_is_not_there_is_an_error() {
        assert!(read(Path::new("/no/such/file")).is_err());
    }

    #[test]
    fn the_read_only_flag_can_be_turned_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        set_readonly(&file, true).unwrap();
        assert!(read(&file).unwrap().readonly);
        set_readonly(&file, false).unwrap();
        assert!(!read(&file).unwrap().readonly);
    }
}
