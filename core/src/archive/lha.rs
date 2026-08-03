// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! LHA / LZH, read-only.
//!
//! An old format, and the reason this module is worth having as an example:
//! it took one file. Adding another format is a [`Reader`] and a line in
//! [`super::FORMATS`], and nothing outside this directory changes.
//!
//! Like tar it has no index - headers and payloads front to back - so a
//! listing is a walk through the whole file. They are small archives by
//! modern standards, so that costs nothing worth optimising.

use std::io;
use std::path::Path;

use super::{Member, Reader};

pub struct Lha;

impl Reader for Lha {
    /// LHA has no encryption of its own, so a password is not consulted.
    fn list(&self, archive: &Path, _password: Option<&str>) -> io::Result<Vec<Member>> {
        let mut reader = delharc::parse_file(archive)?;
        let mut members = Vec::new();
        loop {
            let header = reader.header();
            members.push(Member {
                path: header.parse_pathname_to_str(),
                size: header.original_size,
                packed: Some(header.compressed_size),
                // A naive timestamp is read as local time, which is what the
                // machine that wrote it meant by it.
                modified: header.parse_last_modified().to_local().and_then(|when| {
                    let seconds = when.timestamp();
                    (seconds >= 0).then(|| {
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64)
                    })
                }),
                is_dir: header.is_directory(),
                mode: None,
                encrypted: false,
            });
            // A file whose compression this build cannot decode is still
            // worth listing - seeing that it is in there beats pretending the
            // archive is empty - so the walk skips over it rather than
            // stopping.
            match reader.next_file() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => return Err(io::Error::other(e.to_string())),
            }
        }
        Ok(members)
    }

    fn read(&self, archive: &Path, member: &str, _password: Option<&str>) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut reader = delharc::parse_file(archive)?;
        let wanted = super::normalise(member);
        loop {
            let header = reader.header();
            if super::normalise(&header.parse_pathname_to_str()) == wanted {
                if !reader.is_decoder_supported() {
                    return Err(io::Error::other(
                        "this file uses a compression method that is not supported",
                    ));
                }
                let mut out = Vec::new();
                reader.read_to_end(&mut out)?;
                return Ok(out);
            }
            match reader.next_file() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => return Err(io::Error::other(e.to_string())),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{member} is not in here"),
        ))
    }
}
