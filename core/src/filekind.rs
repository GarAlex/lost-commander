// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What kind of thing a file is, by the look of its name.
//!
//! This is a guess made from the extension and nothing else - it opens no
//! files and reads no bytes. That is the right trade for a listing, where the
//! answer is wanted for every row of a directory that may hold ten thousand,
//! and where being wrong costs the wrong icon rather than the wrong result.
//! Where it matters what a file *is* rather than what it is called - deciding
//! whether something is an archive worth stepping into, or which decoder a
//! preview needs - the engine sniffs the bytes instead, and says so.
//!
//! It lives here, and not beside the code that draws icons, because the two
//! front-ends draw them completely differently and neither owns the question.
//! The graphical one paints vector shapes from a palette; a native Windows one
//! asks the shell for the icon the rest of the desktop already uses. What they
//! agree on is only which of these ten buckets a name falls into, so that is
//! what the engine decides and each front-end renders however it likes.

use crate::entry::Entry;

/// The buckets a name can fall into.
///
/// Ten, chosen to be the distinctions worth making at a glance in a list -
/// not a taxonomy. Anything unrecognised is [`Kind::Plain`] rather than being
/// forced into the nearest match, because a wrong icon is more misleading than
/// a neutral one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Parent,
    Folder,
    Image,
    Code,
    Archive,
    Audio,
    Video,
    Document,
    Binary,
    Plain,
}

impl Kind {
    /// A stable name for this kind, for crossing a boundary or writing down.
    ///
    /// Deliberately not the `Debug` spelling: that changes if the variant is
    /// ever renamed, and anything reading it over the C ABI would quietly stop
    /// matching.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Parent => "parent",
            Kind::Folder => "folder",
            Kind::Image => "image",
            Kind::Code => "code",
            Kind::Archive => "archive",
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Document => "document",
            Kind::Binary => "binary",
            Kind::Plain => "plain",
        }
    }
}

/// Which bucket this entry falls into.
pub fn classify(entry: &Entry) -> Kind {
    if entry.is_parent() {
        return Kind::Parent;
    }
    if entry.is_dir() {
        return Kind::Folder;
    }
    of_name(&entry.name)
}

/// Which bucket a bare name falls into.
///
/// Separate from [`classify`] so a name can be asked about without an `Entry`
/// to hand - which is what anything working from a list of names, rather than
/// from a directory listing, has got.
pub fn of_name(name: &str) -> Kind {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tiff" => Kind::Image,
        "rs" | "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "go" | "java" | "rb" | "sh"
        | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "css" | "lock" => Kind::Code,
        "zip" | "gz" | "bz2" | "xz" | "7z" | "rar" | "tar" | "zst" => Kind::Archive,
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" => Kind::Audio,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "m4v" => Kind::Video,
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "epub" | "md" | "txt" => Kind::Document,
        "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "a" => Kind::Binary,
        _ => Kind::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_read_by_its_extension() {
        assert_eq!(of_name("holiday.jpg"), Kind::Image);
        assert_eq!(of_name("main.rs"), Kind::Code);
        // A lock file is source in every sense that matters here: it is text,
        // it lives in a repository, and it is read far more often than edited.
        assert_eq!(of_name("Cargo.lock"), Kind::Code);
        assert_eq!(of_name("backup.tar"), Kind::Archive);
        assert_eq!(of_name("song.flac"), Kind::Audio);
        assert_eq!(of_name("clip.mkv"), Kind::Video);
        assert_eq!(of_name("report.pdf"), Kind::Document);
        assert_eq!(of_name("notes.md"), Kind::Document);
        assert_eq!(of_name("rcmd.exe"), Kind::Binary);
        assert_eq!(of_name("libfoo.so"), Kind::Binary);
    }

    #[test]
    fn an_extension_in_capitals_is_the_same_extension() {
        // Windows names are routinely shouted, and a file called PHOTO.JPG is
        // not a different kind of thing from photo.jpg.
        assert_eq!(of_name("PHOTO.JPG"), Kind::Image);
        assert_eq!(of_name("Report.PDF"), Kind::Document);
        assert_eq!(of_name("SETUP.EXE"), Kind::Binary);
    }

    #[test]
    fn anything_unrecognised_is_plain_rather_than_a_guess() {
        // A wrong icon is more misleading than a neutral one: it says the
        // program knows something about the file that it does not.
        assert_eq!(of_name("notes"), Kind::Plain);
        assert_eq!(of_name("data.qqq"), Kind::Plain);
        assert_eq!(of_name(""), Kind::Plain);
        assert_eq!(of_name(".gitignore"), Kind::Plain);
    }

    #[test]
    fn only_the_last_extension_counts() {
        // archive.tar.gz is a gzip, and notes.txt.bak is not a document -
        // whatever it once was, what it is now is a backup nobody can open.
        assert_eq!(of_name("archive.tar.gz"), Kind::Archive);
        assert_eq!(of_name("notes.txt.bak"), Kind::Plain);
    }

    #[test]
    fn the_labels_are_stable_names_and_not_the_debug_spelling() {
        // Anything reading these over the C ABI matches on them, so they are
        // fixed here rather than following the variant names around.
        assert_eq!(Kind::Parent.label(), "parent");
        assert_eq!(Kind::Folder.label(), "folder");
        assert_eq!(Kind::Plain.label(), "plain");
    }

    #[test]
    fn what_the_filesystem_says_beats_what_the_name_suggests() {
        use crate::entry::EntryKind;
        use std::path::PathBuf;

        let entry = |name: &str, kind: EntryKind| Entry {
            name: name.to_string(),
            path: PathBuf::from(name),
            kind,
            size: 0,
            modified: None,
            is_symlink: false,
            mode: None,
            marked: false,
        };

        // A directory called assets.zip is a directory, not an archive - and
        // getting this the wrong way round would offer to open it as a file.
        // The look of the name only gets a say once the filesystem has none.
        assert_eq!(classify(&entry("assets.zip", EntryKind::Dir)), Kind::Folder);
        assert_eq!(
            classify(&entry("pictures.jpg", EntryKind::Dir)),
            Kind::Folder
        );
        assert_eq!(classify(&entry("..", EntryKind::Parent)), Kind::Parent);
        // And a file still goes by its name.
        assert_eq!(
            classify(&entry("assets.zip", EntryKind::File)),
            Kind::Archive
        );
    }
}
