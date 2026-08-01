//! Zip, and the many things that are a zip wearing another extension.
//!
//! The cheap format to read: the index lives in a central directory at the
//! end, so listing one is a seek and a parse however big it is, and reading
//! one member inflates that member alone.

use std::io;
use std::path::Path;

use super::{Member, Reader};

pub struct Zip;

impl Reader for Zip {
    /// A zip never encrypts its index, so a password is not wanted here -
    /// which is why an archive nobody can open can still be looked through.
    fn list(&self, archive: &Path, _password: Option<&str>) -> io::Result<Vec<Member>> {
        let file = std::fs::File::open(archive)?;
        let mut zip = ::zip::ZipArchive::new(file).map_err(other)?;

        let mut members = Vec::with_capacity(zip.len());
        for index in 0..zip.len() {
            // `by_index_raw` and not `by_index`: the latter refuses an
            // encrypted entry outright, so listing with it drops every
            // protected file and reports an archive as nearly empty. The raw
            // form reads the header without touching the contents, which is
            // all a listing needs and all that is possible without the
            // password. A zip keeps its names in the clear either way.
            let Ok(entry) = zip.by_index_raw(index) else {
                continue;
            };
            members.push(Member {
                path: entry.name().to_string(),
                size: entry.size(),
                packed: Some(entry.compressed_size()),
                modified: entry.last_modified().and_then(from_zip_time),
                is_dir: entry.is_dir(),
                mode: entry.unix_mode(),
                encrypted: entry.encrypted(),
            });
        }
        Ok(members)
    }

    fn read(&self, archive: &Path, member: &str, password: Option<&str>) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let file = std::fs::File::open(archive)?;
        let mut zip = ::zip::ZipArchive::new(file).map_err(other)?;

        // The stored name may differ from the normalised one this program
        // shows - a trailing slash, a `./`, backslashes - so the match is on
        // the normalised form rather than the raw string. Raw again, so that
        // finding an encrypted entry does not depend on being able to open
        // it.
        let wanted = super::normalise(member);
        let mut found = None;
        for index in 0..zip.len() {
            if let Ok(entry) = zip.by_index_raw(index) {
                if super::normalise(entry.name()) == wanted {
                    found = Some((index, entry.encrypted()));
                    break;
                }
            }
        }
        let (index, encrypted) = found.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{member} is not in here"))
        })?;

        let mut out = Vec::new();
        match (encrypted, password) {
            (true, None) => return Err(super::needs_password()),
            (true, Some(password)) => {
                let mut entry = zip
                    .by_index_decrypt(index, password.as_bytes())
                    .map_err(|e| match e {
                        ::zip::result::ZipError::InvalidPassword => super::wrong_password(),
                        other_error => other(other_error),
                    })?;
                entry.read_to_end(&mut out)?;
            }
            (false, _) => {
                let mut entry = zip.by_index(index).map_err(other)?;
                out.reserve(entry.size() as usize);
                entry.read_to_end(&mut out)?;
            }
        }
        Ok(out)
    }
}

fn other(e: ::zip::result::ZipError) -> io::Error {
    io::Error::other(e.to_string())
}

/// A zip date, which is local time with no zone, as a system time.
///
/// Zip stores DOS timestamps: two-second resolution, no time zone, and no
/// year before 1980. Treating them as local time is what every other tool
/// does, and being an hour out twice a year is better than refusing a date.
fn from_zip_time(when: ::zip::DateTime) -> Option<std::time::SystemTime> {
    use chrono::{Local, NaiveDate, TimeZone};
    let date = NaiveDate::from_ymd_opt(
        i32::from(when.year()),
        u32::from(when.month()),
        u32::from(when.day()),
    )?;
    let stamp = date.and_hms_opt(
        u32::from(when.hour()),
        u32::from(when.minute()),
        u32::from(when.second()),
    )?;
    let local = Local.from_local_datetime(&stamp).single()?;
    let seconds = local.timestamp();
    match seconds >= 0 {
        true => Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64)),
        false => None,
    }
}
