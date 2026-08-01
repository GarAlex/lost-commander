//! Tar, and tar inside a compressed stream.
//!
//! The opposite of zip in the one way that matters here: there is no index.
//! A tar is a sequence of headers and payloads read front to back, and when
//! it sits inside a gzip, xz or bzip2 stream the whole thing must be
//! decompressed to reach the last header. Listing one is therefore a full
//! read, and so is pulling out a member near the end.
//!
//! That is a property of the format rather than of this code, and it is why
//! callers cache what they get back.

use std::io::{self, Read};
use std::path::Path;

use super::{Member, Reader};

/// What the tar is wrapped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapper {
    None,
    Gzip,
    Xz,
    Bzip2,
}

pub struct Tarball(pub Wrapper);

impl Tarball {
    /// The tar bytes, with whatever wrapper taken off.
    ///
    /// Decompressed whole rather than streamed because two of the three
    /// decoders here only offer that, and because every caller goes on to
    /// read the entire archive anyway.
    fn unwrapped(&self, archive: &Path) -> io::Result<Vec<u8>> {
        let file = std::fs::File::open(archive)?;
        let mut reader = io::BufReader::new(file);
        let mut out = Vec::new();
        match self.0 {
            Wrapper::None => {
                reader.read_to_end(&mut out)?;
            }
            Wrapper::Gzip => {
                flate2::read::GzDecoder::new(reader).read_to_end(&mut out)?;
            }
            Wrapper::Xz => {
                lzma_rs::xz_decompress(&mut reader, &mut out)
                    .map_err(|e| io::Error::other(format!("{e:?}")))?;
            }
            Wrapper::Bzip2 => {
                bzip2_rs::DecoderReader::new(reader).read_to_end(&mut out)?;
            }
        }
        Ok(out)
    }
}

impl Reader for Tarball {
    /// Neither tar nor the streams it arrives in have any notion of a
    /// password - an encrypted tarball is a `.tar.gz.gpg`, which is a
    /// different file with a different problem - so one offered here is
    /// ignored rather than pretended about.
    fn list(&self, archive: &Path, _password: Option<&str>) -> io::Result<Vec<Member>> {
        let bytes = self.unwrapped(archive)?;
        let mut tar = tar::Archive::new(io::Cursor::new(bytes));

        let mut members = Vec::new();
        for entry in tar.entries()? {
            // One unreadable header should cost that entry, not the listing:
            // a truncated archive is still worth seeing the front of.
            let Ok(entry) = entry else { break };
            let Ok(path) = entry.path() else { continue };
            let header = entry.header();
            members.push(Member {
                path: path.to_string_lossy().to_string(),
                size: entry.size(),
                packed: None,
                modified: header
                    .mtime()
                    .ok()
                    .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
                is_dir: header.entry_type().is_dir(),
                mode: header.mode().ok(),
                encrypted: false,
            });
        }
        Ok(members)
    }

    fn read(&self, archive: &Path, member: &str, _password: Option<&str>) -> io::Result<Vec<u8>> {
        let bytes = self.unwrapped(archive)?;
        let mut tar = tar::Archive::new(io::Cursor::new(bytes));
        let wanted = super::normalise(member);

        for entry in tar.entries()? {
            let Ok(mut entry) = entry else { break };
            let Ok(path) = entry.path() else { continue };
            if super::normalise(&path.to_string_lossy()) != wanted {
                continue;
            }
            let mut out = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut out)?;
            return Ok(out);
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{member} is not in here"),
        ))
    }
}
