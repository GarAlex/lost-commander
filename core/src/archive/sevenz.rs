// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! 7z, read-only.
//!
//! Indexed like a zip - the header carries every name and size - so listing
//! is cheap. Reading is not always: 7z compresses in *blocks* that may hold
//! several files together, so pulling one member out can mean decompressing
//! the block it shares with its neighbours. Nothing here can avoid that; it
//! is what the format buys its compression ratio with.
//!
//! # Two kinds of locked
//!
//! 7z can encrypt the *contents* alone, which leaves the names readable and
//! behaves like a locked zip. It can also encrypt the **header** (`-mhe=on`),
//! and then there is nothing to list: the archive is one opaque block until
//! the password arrives. Both are reported as needing a password, because
//! from the outside they are the same request.

use std::io;
use std::path::Path;

use super::{Member, Reader};

pub struct SevenZ;

/// The password to try, as the library wants it.
fn password_of(password: Option<&str>) -> sevenz_rust2::Password {
    match password {
        Some(text) => sevenz_rust2::Password::from(text),
        None => sevenz_rust2::Password::empty(),
    }
}

impl Reader for SevenZ {
    fn list(&self, archive: &Path, password: Option<&str>) -> io::Result<Vec<Member>> {
        let listing = sevenz_rust2::Archive::open_with_password(archive, &password_of(password))
            .map_err(|e| locked_or(e, password))?;
        Ok(listing
            .files
            .iter()
            .map(|entry| Member {
                path: entry.name.clone(),
                size: entry.size,
                packed: Some(entry.compressed_size),
                modified: match entry.has_last_modified_date {
                    true => from_windows_time(entry.last_modified_date.to_raw()),
                    false => None,
                },
                is_dir: entry.is_directory,
                mode: None,
                // A 7z with an encrypted header has already refused to list
                // by this point, so anything reached here has readable names;
                // what needs the password is the content, which the entry's
                // own methods say. The library does not expose that per
                // entry, so the honest answer is taken from whether opening
                // it needed one.
                encrypted: password.is_some(),
            })
            .collect())
    }

    fn read(&self, archive: &Path, member: &str, password: Option<&str>) -> io::Result<Vec<u8>> {
        let mut reader = sevenz_rust2::SevenZReader::open(archive, password_of(password))
            .map_err(|e| locked_or(e, password))?;
        let wanted = super::normalise(member);

        // The name as stored may differ from the normalised one shown, so the
        // whole archive is walked comparing normalised names rather than
        // asking for a string the archive may not hold verbatim.
        let mut found: Option<Vec<u8>> = None;
        reader
            .for_each_entries(|entry, rest| {
                if super::normalise(&entry.name) != wanted {
                    return Ok(true);
                }
                let mut out = Vec::with_capacity(entry.size as usize);
                std::io::Read::read_to_end(rest, &mut out)?;
                found = Some(out);
                // Stop: the rest of the archive is of no interest, and a
                // large one would be decompressed for nothing.
                Ok(false)
            })
            .map_err(|e| locked_or(e, password))?;

        found.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{member} is not in here"))
        })
    }
}

/// Turn the library's error into ours, keeping "needs a password" and "that
/// password is wrong" apart from everything else and from each other.
fn locked_or(e: sevenz_rust2::Error, tried: Option<&str>) -> io::Error {
    match e {
        sevenz_rust2::Error::PasswordRequired => super::needs_password(),
        // The library cannot always tell a wrong password from a corrupt
        // block - the symptom is the same, a checksum that does not match -
        // so it says "maybe". Having just been given one, wrong is much the
        // likelier of the two, and is the one the user can do something
        // about.
        sevenz_rust2::Error::MaybeBadPassword(_) if tried.is_some() => super::wrong_password(),
        sevenz_rust2::Error::MaybeBadPassword(_) => super::needs_password(),
        other => io::Error::other(other.to_string()),
    }
}

/// Windows file time - 100ns ticks since 1601 - as a system time.
fn from_windows_time(ticks: u64) -> Option<std::time::SystemTime> {
    /// Seconds between 1601-01-01 and 1970-01-01.
    const TO_UNIX: u64 = 11_644_473_600;
    let seconds = ticks / 10_000_000;
    seconds
        .checked_sub(TO_UNIX)
        .map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s))
}
