// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Archives, read as folders.
//!
//! An archive is a directory that happens to be one file. Everything a panel
//! needs from a real directory - a list of names, sizes, dates, and the
//! ability to read one of them - an archive can answer too, so it may as well
//! be walked into.
//!
//! # What is here, and what is deliberately not
//!
//! Reading only. Listing an archive and pulling one member out of it, which is
//! what almost all of using an archive in a file manager is. Changing one is a
//! different problem with different hazards - a repack that silently drops the
//! permissions and symlinks a writer does not understand is data loss from a
//! rename - and it is not attempted here.
//!
//! # Adding a format
//!
//! One module implementing [`Reader`], and one entry in [`FORMATS`]. Nothing
//! outside this directory knows which formats exist, which is the point: the
//! panel asks "is this an archive" and "what is in it", and the answers do not
//! change shape when a format is added.
//!
//! # Why pure-Rust decoders
//!
//! Every dependency here is pure Rust. A file manager that will not build
//! because a system compression library is missing is worse than one that
//! cannot read that format, and decompression is not where the last few per
//! cent of speed matters.

use std::io;
use std::path::Path;
use std::time::SystemTime;

mod lha;
mod sevenz;
mod tarball;
mod zip;

/// One thing inside an archive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Member {
    /// Where it sits inside the archive: `/` separated, no leading slash, and
    /// no trailing one even on a directory.
    ///
    /// Normalised on the way out of every reader, because the formats do not
    /// agree: zip writes `a/b/`, tar writes `./a/b`, and a Windows-made
    /// archive may well write `a\b`.
    pub path: String,
    pub size: u64,
    /// What it occupies inside the archive, where the format says.
    pub packed: Option<u64>,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
    /// The Unix permission bits, where the format carries them.
    pub mode: Option<u32>,
    /// Whether reading this one needs a password.
    ///
    /// Listed all the same: a zip keeps its names in the clear, so an
    /// encrypted archive can be looked through without being opened, and a
    /// listing that quietly left those entries out would say the archive is
    /// nearly empty when it is not.
    pub encrypted: bool,
}

impl Member {
    /// The name on its own, without the directories above it.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The directory it sits in, `""` for the top level.
    pub fn parent(&self) -> &str {
        match self.path.rfind('/') {
            Some(at) => &self.path[..at],
            None => "",
        }
    }
}

/// What a format has to be able to do.
///
/// Both calls take the archive's path rather than a handle, so a reader holds
/// no state and a panel that reloads is not holding a file open on a disc
/// somebody wants to eject.
pub trait Reader: Send + Sync {
    /// Everything inside, in whatever order the archive stores it.
    ///
    /// A password is offered because some formats encrypt the index itself -
    /// a 7z or rar written with header encryption cannot even be listed
    /// without it - while a zip never does, and ignores it here.
    fn list(&self, archive: &Path, password: Option<&str>) -> io::Result<Vec<Member>>;

    /// One member's contents.
    fn read(&self, archive: &Path, member: &str, password: Option<&str>) -> io::Result<Vec<u8>>;
}

/// The two answers about a password that a caller has to be able to tell
/// apart from ordinary failure, and from each other.
///
/// "Locked" means ask; "wrong" means ask again and say why. A single opaque
/// error would leave the difference between "I have not been asked yet" and
/// "what you gave me is not it" to be guessed from a message string.
pub const NEEDS_PASSWORD: &str = "this needs a password";
pub const WRONG_PASSWORD: &str = "that password does not open it";

pub fn needs_password() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, NEEDS_PASSWORD)
}

pub fn wrong_password() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, WRONG_PASSWORD)
}

/// Whether an error is one a password would fix.
pub fn is_locked(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        && (error.to_string().contains(NEEDS_PASSWORD)
            || error.to_string().contains(WRONG_PASSWORD))
}

/// Whether the password offered was refused, as opposed to never asked for.
pub fn was_refused(error: &io::Error) -> bool {
    error.to_string().contains(WRONG_PASSWORD)
}

/// Bytes that identify a format, and where to look for them.
#[derive(Debug, Clone, Copy)]
pub struct Magic {
    pub at: usize,
    pub bytes: &'static [u8],
}

/// A format this program can read.
pub struct Format {
    /// What to call it.
    pub name: &'static str,
    /// The endings that suggest it. Longest first, so `.tar.gz` is tried
    /// before `.gz`.
    pub extensions: &'static [&'static str],
    /// How to recognise it from the file itself. Empty where the format has
    /// no usable signature.
    pub magic: &'static [Magic],
    reader: fn() -> Box<dyn Reader>,
}

impl Format {
    pub fn reader(&self) -> Box<dyn Reader> {
        (self.reader)()
    }
}

impl std::fmt::Debug for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Format").field("name", &self.name).finish()
    }
}

/// Every format, in the order they are tried.
///
/// Compound endings come before their parts: a `.tar.gz` is a tar in a gzip
/// wrapper and listing it as "one member called archive.tar" would be
/// technically true and useless.
pub const FORMATS: &[Format] = &[
    Format {
        name: "zip",
        extensions: &[
            ".zip", ".jar", ".war", ".ear", ".odt", ".ods", ".odp", ".epub", ".xpi",
        ],
        // "PK\x03\x04" for a normal one; the other two are an empty archive
        // and a spanned one, both of which the reader still opens.
        magic: &[
            Magic {
                at: 0,
                bytes: b"PK\x03\x04",
            },
            Magic {
                at: 0,
                bytes: b"PK\x05\x06",
            },
            Magic {
                at: 0,
                bytes: b"PK\x07\x08",
            },
        ],
        reader: || Box::new(zip::Zip),
    },
    Format {
        name: "tar.gz",
        extensions: &[".tar.gz", ".tgz", ".taz"],
        magic: &[],
        reader: || Box::new(tarball::Tarball(tarball::Wrapper::Gzip)),
    },
    Format {
        name: "tar.xz",
        extensions: &[".tar.xz", ".txz"],
        magic: &[],
        reader: || Box::new(tarball::Tarball(tarball::Wrapper::Xz)),
    },
    Format {
        name: "tar.bz2",
        extensions: &[".tar.bz2", ".tbz", ".tbz2", ".tb2"],
        magic: &[],
        reader: || Box::new(tarball::Tarball(tarball::Wrapper::Bzip2)),
    },
    Format {
        name: "tar",
        extensions: &[".tar"],
        // "ustar" sits 257 bytes in, past the first header's name field.
        magic: &[
            Magic {
                at: 257,
                bytes: b"ustar\0",
            },
            Magic {
                at: 257,
                bytes: b"ustar  \0",
            },
        ],
        reader: || Box::new(tarball::Tarball(tarball::Wrapper::None)),
    },
    Format {
        name: "7z",
        extensions: &[".7z"],
        magic: &[Magic {
            at: 0,
            bytes: b"7z\xBC\xAF\x27\x1C",
        }],
        reader: || Box::new(sevenz::SevenZ),
    },
    Format {
        name: "lha",
        extensions: &[".lha", ".lzh", ".lzs"],
        // The header's own marker, five bytes in: "-lh0-" through "-lh7-",
        // "-lz4-" and friends. Only the fixed part is matched.
        magic: &[Magic {
            at: 2,
            bytes: b"-l",
        }],
        reader: || Box::new(lha::Lha),
    },
];

/// How much of a file is worth reading to recognise it.
const SNIFF: usize = 512;

/// Which format this file is, if any.
///
/// The contents are asked first and the name second, because a name is a
/// claim and the bytes are the fact - `.zip` on a JPEG is a mistake worth
/// seeing through, and an archive with no extension at all is still an
/// archive. Where the bytes say nothing definite, the ending decides.
pub fn identify(path: &Path) -> Option<&'static Format> {
    let head = head_of(path).unwrap_or_default();
    by_magic(&head).or_else(|| by_name(path))
}

fn head_of(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; SNIFF];
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

/// Recognise by signature.
pub fn by_magic(head: &[u8]) -> Option<&'static Format> {
    FORMATS.iter().find(|format| {
        format.magic.iter().any(|magic| {
            head.len() >= magic.at + magic.bytes.len()
                && &head[magic.at..magic.at + magic.bytes.len()] == magic.bytes
        })
    })
}

/// Recognise by the end of the name.
///
/// Case-insensitively: `.ZIP` off a Windows machine is a zip.
pub fn by_name(path: &Path) -> Option<&'static Format> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    FORMATS
        .iter()
        .find(|format| format.extensions.iter().any(|end| name.ends_with(end)))
}

/// Whether this file is one this program can walk into.
pub fn is_archive(path: &Path) -> bool {
    path.is_file() && identify(path).is_some()
}

/// Everything inside an archive, with the format that read it.
#[derive(Debug)]
pub struct Listing {
    pub format: &'static str,
    pub members: Vec<Member>,
}

impl Listing {
    /// Whether anything in here needs a password to read.
    pub fn any_locked(&self) -> bool {
        self.members.iter().any(|member| member.encrypted)
    }
}

/// Read an archive's index.
pub fn list(path: &Path) -> io::Result<Listing> {
    list_with(path, None)
}

/// Read an archive's index, with a password for the formats that need one to
/// do even that.
pub fn list_with(path: &Path, password: Option<&str>) -> io::Result<Listing> {
    let format = identify(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "not an archive this program can read",
        )
    })?;
    let mut members = format.reader().list(path, password)?;
    for member in members.iter_mut() {
        member.path = normalise(&member.path);
    }
    members.retain(|member| !member.path.is_empty());
    Ok(Listing {
        format: format.name,
        members,
    })
}

/// Pull one member out.
pub fn read(path: &Path, member: &str) -> io::Result<Vec<u8>> {
    read_with(path, member, None)
}

/// Pull one member out of an archive that wants a password.
pub fn read_with(path: &Path, member: &str, password: Option<&str>) -> io::Result<Vec<u8>> {
    let format = identify(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "not an archive this program can read",
        )
    })?;
    format.reader().read(path, member, password)
}

/// A member path as this program stores it.
///
/// The formats do not agree: zip marks a directory with a trailing slash, tar
/// writes `./a/b`, and an archive made on Windows may well use backslashes.
/// One shape here means the level-walking below has one case to think about.
pub fn normalise(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for part in path.replace('\\', "/").split('/') {
        // `.` is noise and `..` in an archive is either a mistake or an
        // attack; neither belongs in a path shown to anybody.
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

/// One line of a listing at one level inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub name: String,
    /// The full member path, for reading it back out.
    pub path: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
    pub mode: Option<u32>,
}

/// What a panel should show at one level inside an archive.
///
/// Archives store a flat list, and many store no directory entries at all - a
/// zip of `docs/a.txt` and `docs/b.txt` need not mention `docs` anywhere. So
/// the directories at each level are worked out from the members below them
/// rather than trusted to exist, and a directory entry that *is* present is
/// not shown twice.
pub fn at(members: &[Member], inside: &str) -> Vec<Level> {
    let inside = normalise(inside);
    let prefix = match inside.is_empty() {
        true => String::new(),
        false => format!("{inside}/"),
    };

    let mut out: Vec<Level> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for member in members {
        let Some(rest) = member.path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            // Deeper down: it contributes a directory at this level, and the
            // directory may well not be a member of its own.
            Some((directory, _)) => {
                if seen.insert(directory.to_string()) {
                    out.push(Level {
                        name: directory.to_string(),
                        path: format!("{prefix}{directory}"),
                        size: 0,
                        modified: None,
                        is_dir: true,
                        mode: None,
                    });
                }
            }
            // Here.
            None => {
                if !seen.insert(rest.to_string()) {
                    // Already stood up as a directory by something below it;
                    // fill in what the real entry knows.
                    if let Some(existing) = out.iter_mut().find(|level| level.name == rest) {
                        if existing.modified.is_none() {
                            existing.modified = member.modified;
                            existing.mode = member.mode;
                        }
                    }
                    continue;
                }
                out.push(Level {
                    name: rest.to_string(),
                    path: member.path.clone(),
                    size: member.size,
                    modified: member.modified,
                    is_dir: member.is_dir,
                    mode: member.mode,
                });
            }
        }
    }
    out
}

/// Whether anything inside the archive sits at this level.
///
/// A path typed or remembered from last time can point at a directory that no
/// longer exists, and walking into nothing should say so rather than showing
/// an empty listing that looks like an empty directory.
pub fn holds(members: &[Member], inside: &str) -> bool {
    let inside = normalise(inside);
    if inside.is_empty() {
        return true;
    }
    let prefix = format!("{inside}/");
    members
        .iter()
        .any(|member| member.path == inside || member.path.starts_with(&prefix))
}

/// Everything at or below a level, for extracting a whole directory.
pub fn under<'a>(members: &'a [Member], inside: &str) -> Vec<&'a Member> {
    let inside = normalise(inside);
    if inside.is_empty() {
        return members.iter().collect();
    }
    let prefix = format!("{inside}/");
    members
        .iter()
        .filter(|member| member.path == inside || member.path.starts_with(&prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str, size: u64, is_dir: bool) -> Member {
        Member {
            path: path.to_string(),
            size,
            is_dir,
            ..Member::default()
        }
    }

    #[test]
    fn a_member_path_is_one_shape_whatever_the_format_wrote() {
        // zip marks a directory with a trailing slash, tar writes ./a/b, and
        // an archive made on Windows uses backslashes.
        assert_eq!(normalise("docs/"), "docs");
        assert_eq!(normalise("./docs/a.txt"), "docs/a.txt");
        assert_eq!(normalise("docs\\a.txt"), "docs/a.txt");
        assert_eq!(normalise("/docs//a.txt"), "docs/a.txt");
        // `..` inside an archive is a mistake at best and an escape attempt
        // at worst. It never survives.
        assert_eq!(normalise("../../etc/passwd"), "etc/passwd");
        assert_eq!(normalise("a/../../b"), "a/b");
        assert_eq!(normalise(""), "");
    }

    #[test]
    fn a_member_knows_its_name_and_where_it_sits() {
        let one = member("docs/notes/a.txt", 10, false);
        assert_eq!(one.name(), "a.txt");
        assert_eq!(one.parent(), "docs/notes");

        let top = member("readme", 1, false);
        assert_eq!(top.name(), "readme");
        assert_eq!(top.parent(), "");
    }

    #[test]
    fn the_top_level_shows_files_and_the_directories_above_the_rest() {
        let members = vec![
            member("readme.txt", 12, false),
            member("docs/a.txt", 20, false),
            member("docs/b.txt", 30, false),
            member("docs/deep/c.txt", 40, false),
        ];
        let top = at(&members, "");
        let names: Vec<&str> = top.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["readme.txt", "docs"]);
        assert!(top[1].is_dir, "docs is a directory");
        assert_eq!(top[1].path, "docs");
        assert_eq!(top[0].size, 12);
    }

    #[test]
    fn a_directory_nobody_wrote_down_is_still_a_directory() {
        // A zip of docs/a.txt need not contain an entry for `docs` at all.
        // Trusting the archive to have one would leave the file unreachable.
        let members = vec![member("docs/deep/deeper/a.txt", 5, false)];
        assert_eq!(at(&members, "").len(), 1);
        assert!(at(&members, "")[0].is_dir);
        assert_eq!(at(&members, "docs")[0].name, "deep");
        assert_eq!(at(&members, "docs/deep")[0].name, "deeper");
        assert_eq!(at(&members, "docs/deep/deeper")[0].name, "a.txt");
        assert!(!at(&members, "docs/deep/deeper")[0].is_dir);
    }

    #[test]
    fn a_directory_that_is_written_down_is_not_shown_twice() {
        let members = vec![
            Member {
                path: "docs".to_string(),
                is_dir: true,
                mode: Some(0o755),
                ..Member::default()
            },
            member("docs/a.txt", 5, false),
        ];
        let top = at(&members, "");
        assert_eq!(top.len(), 1, "got {top:?}");
        assert!(top[0].is_dir);
        // And what the real entry knew is kept.
        assert_eq!(top[0].mode, Some(0o755));
    }

    #[test]
    fn walking_into_nothing_can_be_told_from_an_empty_directory() {
        let members = vec![member("docs/a.txt", 5, false)];
        assert!(holds(&members, ""));
        assert!(holds(&members, "docs"));
        assert!(!holds(&members, "nowhere"));
        // A prefix that is not a whole name does not count: `doc` is not
        // `docs`.
        assert!(!holds(&members, "doc"));
    }

    #[test]
    fn everything_under_a_level_comes_back_for_extracting_it() {
        let members = vec![
            member("readme.txt", 1, false),
            member("docs/a.txt", 2, false),
            member("docs/deep/b.txt", 3, false),
        ];
        let all: Vec<&str> = under(&members, "docs")
            .into_iter()
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(all, vec!["docs/a.txt", "docs/deep/b.txt"]);
        assert_eq!(under(&members, "").len(), 3);
    }

    #[test]
    fn a_name_is_a_claim_and_the_bytes_are_the_fact() {
        // A zip renamed to .jpg is still a zip, and a JPEG renamed to .zip is
        // still not one. The signature is asked first for exactly this.
        assert_eq!(by_magic(b"PK\x03\x04rest").map(|f| f.name), Some("zip"));
        assert_eq!(
            by_magic(b"7z\xBC\xAF\x27\x1Crest").map(|f| f.name),
            Some("7z")
        );
        assert!(by_magic(b"\xFF\xD8\xFFno").is_none(), "a JPEG is not one");

        // The ending is the fallback, and case does not matter.
        assert_eq!(by_name(Path::new("/a/b.ZIP")).map(|f| f.name), Some("zip"));
        assert_eq!(
            by_name(Path::new("/a/b.tar.gz")).map(|f| f.name),
            Some("tar.gz"),
            "the compound ending wins over .gz alone"
        );
        assert_eq!(
            by_name(Path::new("/a/b.tgz")).map(|f| f.name),
            Some("tar.gz")
        );
        assert!(by_name(Path::new("/a/b.txt")).is_none());
    }

    #[test]
    fn a_tar_is_recognised_by_the_marker_past_its_first_name() {
        let mut head = vec![0u8; 512];
        head[257..263].copy_from_slice(b"ustar\0");
        assert_eq!(by_magic(&head).map(|f| f.name), Some("tar"));
        // Too short to hold the marker is not a match rather than a panic.
        assert!(by_magic(&head[..100]).is_none());
    }
}

/// Tests that read archives made by the system's own tools.
///
/// Kept apart from the pure ones above because they need those tools
/// installed, and because what they prove is different: not that the code
/// agrees with itself, but that it agrees with `zip` and `tar`.
#[cfg(test)]
mod against_real_archives {
    use super::*;
    use std::process::Command;

    /// A directory with a known shape, for an archiver to be pointed at.
    ///
    /// Deliberately awkward: a nested directory, a file with a space in its
    /// name, and one deep enough to need two levels synthesised.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("readme.txt"), "at the top\n").unwrap();
        std::fs::create_dir_all(root.join("docs/deep")).unwrap();
        std::fs::write(root.join("docs/notes.txt"), "in a folder\n").unwrap();
        std::fs::write(root.join("docs/a name with spaces.txt"), "spaced\n").unwrap();
        std::fs::write(root.join("docs/deep/buried.txt"), "two down\n").unwrap();
        dir
    }

    fn have(program: &str) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {program}"))
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// Run an archiver in `cwd`, and hand back where it was told to write.
    fn make(cwd: &std::path::Path, into: &std::path::Path, line: &str) -> bool {
        let status = Command::new("sh")
            .arg("-c")
            .arg(line)
            .current_dir(cwd)
            .status();
        matches!(status, Ok(s) if s.success()) && into.exists()
    }

    /// The checks every format has to pass, whatever made the archive.
    fn behaves(archive: &std::path::Path, format: &str) {
        assert_eq!(
            identify(archive).map(|f| f.name),
            Some(format),
            "{} was not recognised as {format}",
            archive.display()
        );

        let listing = list(archive).expect("listing");
        assert_eq!(listing.format, format);

        // The top level: one file, and a directory that may or may not have
        // been written down as an entry of its own.
        let top = at(&listing.members, "");
        let names: Vec<&str> = top.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"readme.txt"), "top level was {names:?}");
        assert!(names.contains(&"docs"), "top level was {names:?}");
        assert!(
            top.iter().find(|l| l.name == "docs").unwrap().is_dir,
            "docs should be a directory"
        );

        // One level down, including the name with a space in it.
        let level = at(&listing.members, "docs");
        let inside: Vec<&str> = level.iter().map(|l| l.name.as_str()).collect();
        assert!(inside.contains(&"notes.txt"), "docs held {inside:?}");
        assert!(
            inside.contains(&"a name with spaces.txt"),
            "docs held {inside:?}"
        );
        assert!(inside.contains(&"deep"), "docs held {inside:?}");

        // Two down, which needs the level walking to be right.
        let below = at(&listing.members, "docs/deep");
        let deep: Vec<&str> = below.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(deep, vec!["buried.txt"]);

        // And the contents come back byte for byte.
        assert_eq!(
            read(archive, "readme.txt").unwrap(),
            b"at the top\n".to_vec()
        );
        assert_eq!(
            read(archive, "docs/deep/buried.txt").unwrap(),
            b"two down\n".to_vec()
        );
        assert_eq!(
            read(archive, "docs/a name with spaces.txt").unwrap(),
            b"spaced\n".to_vec()
        );

        // Asking for something that is not there is an error, not a panic or
        // an empty file that looks like a real one.
        assert!(read(archive, "nowhere.txt").is_err());

        // A size the archive knows about.
        let readme = listing
            .members
            .iter()
            .find(|m| m.path == "readme.txt")
            .expect("readme is a member");
        assert_eq!(readme.size, 11);
    }

    #[test]
    fn a_zip_made_by_zip() {
        if !have("zip") {
            eprintln!("no zip on this machine - skipped");
            return;
        }
        let dir = tree();
        let archive = dir.path().join("made.zip");
        assert!(make(
            dir.path(),
            &archive,
            "zip -qr made.zip readme.txt docs"
        ));
        behaves(&archive, "zip");
    }

    #[test]
    fn a_tar_and_the_three_streams_it_arrives_in() {
        if !have("tar") {
            eprintln!("no tar on this machine - skipped");
            return;
        }
        for (name, flag, format) in [
            ("made.tar", "", "tar"),
            ("made.tar.gz", "z", "tar.gz"),
            ("made.tar.xz", "J", "tar.xz"),
            ("made.tar.bz2", "j", "tar.bz2"),
        ] {
            let dir = tree();
            let archive = dir.path().join(name);
            let line = format!("tar -c{flag}f {name} readme.txt docs");
            if !make(dir.path(), &archive, &line) {
                eprintln!("could not make {name} - skipped");
                continue;
            }
            behaves(&archive, format);
        }
    }

    #[test]
    fn a_zip_whose_name_lies_is_still_a_zip() {
        // The signature is asked before the name for exactly this: an archive
        // saved with the wrong extension, or none at all, is still readable.
        if !have("zip") {
            eprintln!("no zip on this machine - skipped");
            return;
        }
        let dir = tree();
        assert!(make(
            dir.path(),
            &dir.path().join("made.zip"),
            "zip -qr made.zip readme.txt docs"
        ));
        let lying = dir.path().join("holiday.jpg");
        std::fs::rename(dir.path().join("made.zip"), &lying).unwrap();

        assert_eq!(identify(&lying).map(|f| f.name), Some("zip"));
        assert_eq!(
            read(&lying, "readme.txt").unwrap(),
            b"at the top\n".to_vec()
        );
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_not_one() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "just text, at some length\n").unwrap();
        assert!(identify(&plain).is_none());
        assert!(!is_archive(&plain));
        assert!(list(&plain).is_err());

        // Nor is a directory, however it is named.
        let named = dir.path().join("assets.zip");
        std::fs::create_dir(&named).unwrap();
        assert!(!is_archive(&named), "a directory is not an archive");
    }

    #[test]
    fn a_truncated_archive_gives_back_what_it_can() {
        // Half a download should show its front rather than nothing at all.
        if !have("tar") {
            eprintln!("no tar on this machine - skipped");
            return;
        }
        let dir = tree();
        let archive = dir.path().join("made.tar");
        assert!(make(
            dir.path(),
            &archive,
            "tar -cf made.tar readme.txt docs"
        ));

        let whole = std::fs::read(&archive).unwrap();
        let cut = dir.path().join("cut.tar");
        std::fs::write(&cut, &whole[..whole.len() / 2]).unwrap();

        // Not an error, and not a panic: a listing of what survived.
        let listing = list(&cut).expect("a truncated tar still lists");
        assert!(
            !listing.members.is_empty(),
            "the front of it should still be readable"
        );
    }
}

/// Tests about archives that want a password.
///
/// The interesting case, and the one most easily got wrong: a zip keeps its
/// *names* in the clear however well its contents are locked, so an encrypted
/// archive must still list in full. A listing that quietly dropped what it
/// could not decrypt would report an archive as nearly empty, which is the
/// worst kind of wrong - it looks like an answer.
#[cfg(test)]
mod locked_archives {
    use super::*;
    use std::process::Command;

    fn have(program: &str) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {program}"))
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// A directory to lock up.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "at the top\n").unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/notes.txt"), "in a folder\n").unwrap();
        dir
    }

    fn run(cwd: &Path, line: &str) -> bool {
        matches!(
            Command::new("sh").arg("-c").arg(line).current_dir(cwd).status(),
            Ok(status) if status.success()
        )
    }

    /// Everything a locked archive has to do, whichever scheme locked it.
    fn behaves_when_locked(archive: &Path, password: &str) {
        // Listed in full. This is the whole point: the names are not secret,
        // and pretending they are loses the user their own file list.
        let listing = list(archive).expect("a locked zip still lists");
        let names: Vec<&str> = listing.members.iter().map(|m| m.path.as_str()).collect();
        assert!(
            names.contains(&"readme.txt"),
            "the locked entries went missing: {names:?}"
        );
        assert!(
            names.contains(&"docs/notes.txt"),
            "the locked entries went missing: {names:?}"
        );
        assert!(listing.any_locked(), "and it says they are locked");

        // Walking it works without any password at all.
        let top = at(&listing.members, "");
        assert!(top.iter().any(|l| l.name == "docs" && l.is_dir));

        // Reading without one is refused in a way the caller can act on -
        // "ask for a password", not "no such file", which would be a lie.
        let refused = read(archive, "readme.txt").expect_err("should need a password");
        assert!(is_locked(&refused), "got {refused} ({:?})", refused.kind());
        assert!(!was_refused(&refused), "nothing has been tried yet");

        // The wrong one is refused differently, so the caller can say why.
        let wrong = read_with(archive, "readme.txt", Some("not it"))
            .expect_err("the wrong password should not open it");
        assert!(is_locked(&wrong), "got {wrong}");
        assert!(was_refused(&wrong), "got {wrong}");

        // And the right one works.
        assert_eq!(
            read_with(archive, "readme.txt", Some(password)).unwrap(),
            b"at the top\n".to_vec()
        );
        assert_eq!(
            read_with(archive, "docs/notes.txt", Some(password)).unwrap(),
            b"in a folder\n".to_vec()
        );
    }

    #[test]
    fn a_zip_locked_the_old_way() {
        // ZipCrypto, which is what `zip -P` writes and what decades of
        // archives in the wild use.
        if !have("zip") {
            eprintln!("no zip on this machine - skipped");
            return;
        }
        let dir = tree();
        let archive = dir.path().join("locked.zip");
        assert!(run(
            dir.path(),
            "zip -q -P opensesame -r locked.zip readme.txt docs"
        ));
        behaves_when_locked(&archive, "opensesame");
    }

    #[test]
    fn a_zip_locked_the_way_current_tools_lock_them() {
        // WinZip AES-256, which every modern archiver writes by default and
        // which the old scheme's support does nothing for.
        if !have("python3") {
            eprintln!("no python3 on this machine - skipped");
            return;
        }
        let dir = tree();
        let archive = dir.path().join("aes.zip");
        let script = r#"python3 - <<'PY'
import sys
try:
    import pyzipper
except ImportError:
    sys.exit(7)
with pyzipper.AESZipFile('aes.zip', 'w', compression=pyzipper.ZIP_DEFLATED,
                         encryption=pyzipper.WZ_AES) as z:
    z.setpassword(b'opensesame')
    z.write('readme.txt')
    z.write('docs/notes.txt')
PY"#;
        if !run(dir.path(), script) {
            eprintln!("no pyzipper to write an AES zip - skipped");
            return;
        }
        behaves_when_locked(&archive, "opensesame");
    }

    #[test]
    fn an_unlocked_archive_says_nothing_about_passwords() {
        if !have("zip") {
            eprintln!("no zip on this machine - skipped");
            return;
        }
        let dir = tree();
        assert!(run(dir.path(), "zip -q -r open.zip readme.txt docs"));
        let archive = dir.path().join("open.zip");

        let listing = list(&archive).unwrap();
        assert!(!listing.any_locked());
        assert!(listing.members.iter().all(|member| !member.encrypted));
        // And a password offered where none is wanted is simply ignored.
        assert_eq!(
            read_with(&archive, "readme.txt", Some("pointless")).unwrap(),
            b"at the top\n".to_vec()
        );
    }

    #[test]
    fn the_two_answers_about_a_password_are_told_apart() {
        // A caller has to be able to tell "I have not been asked" from "what
        // you gave me is not it", or it cannot say which of the two happened.
        assert!(is_locked(&needs_password()));
        assert!(is_locked(&wrong_password()));
        assert!(!was_refused(&needs_password()));
        assert!(was_refused(&wrong_password()));

        // And neither is confused with ordinary failure.
        let missing = io::Error::new(io::ErrorKind::NotFound, "nowhere.txt is not in here");
        assert!(!is_locked(&missing));
        assert!(!is_locked(&io::Error::other("truncated")));
    }
}
