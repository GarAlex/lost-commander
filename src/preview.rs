//! What quick view should show for a file, and how to get the system to help.
//!
//! Two halves. The first is ours: text is read through a line index (see
//! [`crate::textindex`], which is what lets a gigabyte-long log be scrolled
//! without holding it), and the image formats the `image` crate can decode
//! are decoded directly. The second is
//! the operating system's, because every desktop already knows how to render a
//! PDF, a RAW photo or a video's poster frame, and reimplementing that would
//! be absurd.
//!
//! All three platforms expose the same shape of thing - "give me a picture of
//! this file" - so the seam here is a command that writes a PNG:
//!
//! * **Linux** has the freedesktop thumbnailer spec: `*.thumbnailer` files
//!   declaring a MIME type and a command line. This is the one the tests
//!   exercise, since it is data on disk and can be faked.
//! * **macOS** has Quick Look. `qlmanage -t` is the same idea from the command
//!   line, and covers everything Finder's preview does.
//! * **Windows** has `IThumbnailProvider` behind `IShellItemImageFactory`,
//!   which is COM rather than a command, so it does not fit this seam and is
//!   not wired up: Windows falls back to the built-in decoders.
//!
//! Nothing here spawns anything or touches the GUI. Deciding *what* to do is
//! separated from doing it so the decisions can be tested.

use std::path::{Path, PathBuf};

use crate::mount::Platform;

/// What to show for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    /// Read it and show the text.
    Text,
    /// Decode it ourselves.
    Image,
    /// Ask the system for a picture; failing that, show the facts.
    System,
}

/// Extensions the `image` crate is built with here. Anything else that is
/// still a picture - RAW, HEIC, PSD - goes to the system, which usually knows.
const DECODABLE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff",
];

/// Extensions worth showing as text without sniffing the bytes.
const TEXTUAL: &[&str] = &[
    "txt",
    "md",
    "rst",
    "log",
    "csv",
    "tsv",
    "ini",
    "cfg",
    "conf",
    "toml",
    "json",
    "yaml",
    "yml",
    "xml",
    "html",
    "htm",
    "css",
    "js",
    "ts",
    "rs",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "py",
    "rb",
    "go",
    "java",
    "kt",
    "swift",
    "sh",
    "bash",
    "zsh",
    "fish",
    "pl",
    "lua",
    "sql",
    "diff",
    "patch",
    "gitignore",
    "lock",
    "manifest",
    "makefile",
    "dockerfile",
];

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Decide what quick view should do with `path`.
///
/// `is_dir` is passed in rather than probed so this stays a pure decision -
/// the caller already knows, having just listed the directory.
pub fn classify(path: &Path, is_dir: bool) -> Kind {
    if is_dir {
        return Kind::Directory;
    }
    let extension = extension(path);
    if DECODABLE.contains(&extension.as_str()) {
        return Kind::Image;
    }
    if TEXTUAL.contains(&extension.as_str()) {
        return Kind::Text;
    }
    // A file with no extension at all is far more often a script or a README
    // than something the system can draw, so sniffing it is worth the read.
    if extension.is_empty() {
        return Kind::Text;
    }
    Kind::System
}

/// Whether a chunk of a file looks like text rather than a binary.
///
/// The NUL byte is the giveaway, and the test every tool from `grep` to `file`
/// uses: no text encoding this program will meet puts one in the first few
/// kilobytes, and every binary format does.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if bytes.contains(&0) {
        return false;
    }
    // A high proportion of control characters says binary even without a NUL.
    let odd = bytes
        .iter()
        .filter(|&&b| b < 0x09 || (0x0e..0x20).contains(&b))
        .count();
    odd * 100 / bytes.len() < 5
}

/// A MIME type from the extension.
///
/// Two callers, and the second is why the text types are here: thumbnailers
/// are never registered for `text/plain` - `kind` sends text down its own
/// path long before one is asked for - but "Open with" ranks by this, and a
/// chooser that could not tell which applications open a `.txt` would be
/// getting the commonest case wrong.
///
/// Still not a database. It is the types a desktop actually registers
/// applications for, which is a much shorter list than every type there is.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    Some(match extension(path).as_str() {
        // ---- text, which is what most files worth opening are
        "txt" | "text" | "log" | "rst" | "asc" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "xml" => "text/xml",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "sh" | "bash" => "application/x-shellscript",
        "py" => "text/x-python",
        "rs" => "text/rust",
        "c" | "h" => "text/x-csrc",
        "cpp" | "cc" | "hpp" => "text/x-c++src",
        "js" => "text/javascript",
        "ini" | "cfg" | "conf" => "text/plain",
        // ---- archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" | "tgz" => "application/gzip",
        "bz2" => "application/x-bzip",
        "xz" => "application/x-xz",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "svg" => "image/svg+xml",
        "svgz" => "image/svg+xml-compressed",
        "pdf" => "application/pdf",
        "ps" => "application/postscript",
        "epub" => "application/epub+zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "psd" => "image/vnd.adobe.photoshop",
        "cr2" | "nef" | "arw" | "dng" | "raf" | "orf" => "image/x-dcraw",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => return None,
    })
}

/// One entry from a freedesktop `*.thumbnailer` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnailer {
    /// The command line, still holding its `%s` `%u` `%i` `%o` placeholders.
    pub exec: String,
    /// The binary to check for before believing any of it.
    pub try_exec: Option<String>,
    pub mime_types: Vec<String>,
}

impl Thumbnailer {
    pub fn handles(&self, mime: &str) -> bool {
        self.mime_types.iter().any(|m| m == mime)
    }

    /// Whether the program it needs is actually installed.
    ///
    /// `TryExec` is in the spec for exactly this, and it is not a formality:
    /// distributions ship the `.thumbnailer` file and the binary in separate
    /// packages, so a registered thumbnailer is routinely not a working one.
    /// This container is such a machine - it declares librsvg's thumbnailer
    /// and does not have `gdk-pixbuf-thumbnailer`.
    pub fn installed(&self, exists: &dyn Fn(&Path) -> bool) -> bool {
        match &self.try_exec {
            Some(program) => program_exists(program, exists),
            None => true,
        }
    }
}

/// Resolve a `TryExec` value: a path if it looks like one, otherwise a name
/// to look for along `PATH`, as the desktop-entry spec says.
pub fn program_exists(program: &str, exists: &dyn Fn(&Path) -> bool) -> bool {
    if program.contains('/') || program.contains('\\') {
        return exists(Path::new(program));
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| exists(&dir.join(program)))
}

/// The predicate to hand the functions above in real use.
pub fn on_disk(path: &Path) -> bool {
    path.exists()
}

/// Parse a `.thumbnailer` file. Returns `None` if it is not one.
///
/// The format is a desktop-entry file, but only three keys matter, so this
/// reads them directly rather than pulling in an INI parser for it.
pub fn parse_thumbnailer(text: &str) -> Option<Thumbnailer> {
    let mut in_section = false;
    let mut exec = None;
    let mut try_exec = None;
    let mut mime_types = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[Thumbnailer Entry]";
            continue;
        }
        if !in_section || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Exec" => exec = Some(value.trim().to_string()),
            "TryExec" => try_exec = Some(value.trim().to_string()),
            "MimeType" => {
                mime_types = value
                    .split(';')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    let exec = exec?;
    if mime_types.is_empty() {
        return None;
    }
    Some(Thumbnailer {
        exec,
        try_exec,
        mime_types,
    })
}

/// Where thumbnailers are declared, most specific first.
pub fn thumbnailer_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("thumbnailers"));
    }
    dirs.push(PathBuf::from("/usr/local/share/thumbnailers"));
    dirs.push(PathBuf::from("/usr/share/thumbnailers"));
    dirs
}

/// Read every thumbnailer declared in `dirs`.
pub fn load_thumbnailers(dirs: &[PathBuf]) -> Vec<Thumbnailer> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("thumbnailer") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(thumbnailer) = parse_thumbnailer(&text) {
                found.push(thumbnailer);
            }
        }
    }
    found
}

/// A command that will write a picture of a file to `output`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Where the picture will be, once the command has run.
    pub output: PathBuf,
}

/// Fill in a freedesktop `Exec` line.
///
/// `%i` is the input path, `%u` its URI, `%o` the output file and `%s` the
/// size. A line with no `%o` cannot be used, since there would be nowhere for
/// the result to go.
pub fn expand_exec(exec: &str, input: &Path, output: &Path, size: u32) -> Option<ThumbnailCommand> {
    let mut words = exec.split_whitespace();
    let program = words.next()?.to_string();

    let uri = file_uri(input);
    let mut saw_output = false;
    let mut args = Vec::new();
    for word in words {
        let filled = word
            .replace("%i", &input.to_string_lossy())
            .replace("%u", &uri)
            .replace("%o", &output.to_string_lossy())
            .replace("%s", &size.to_string());
        if word.contains("%o") {
            saw_output = true;
        }
        args.push(filled);
    }
    saw_output.then_some(ThumbnailCommand {
        program,
        args,
        output: output.to_path_buf(),
    })
}

/// `file://` URI for a path, with the characters that need escaping escaped.
pub fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The command that would render `path`, for a given platform.
///
/// `thumbnailers` is only consulted on Linux; the other platforms have one
/// system-wide answer. Passing them in - rather than reading the disk here -
/// is what lets the whole decision be tested.
pub fn thumbnail_command(
    platform: Platform,
    thumbnailers: &[Thumbnailer],
    path: &Path,
    output_dir: &Path,
    size: u32,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<ThumbnailCommand> {
    match platform {
        Platform::Linux => {
            let mime = mime_for(path)?;
            let thumbnailer = thumbnailers
                .iter()
                .find(|t| t.handles(mime) && t.installed(exists))?;
            let output = output_dir.join("thumb.png");
            expand_exec(&thumbnailer.exec, path, &output, size)
        }
        Platform::MacOs => {
            // Quick Look, the same engine Finder's preview uses. It names the
            // result after the file rather than taking an output path.
            let name = path.file_name()?.to_string_lossy().to_string();
            Some(ThumbnailCommand {
                program: "qlmanage".to_string(),
                args: vec![
                    "-t".to_string(),
                    "-s".to_string(),
                    size.to_string(),
                    "-o".to_string(),
                    output_dir.to_string_lossy().to_string(),
                    path.to_string_lossy().to_string(),
                ],
                output: output_dir.join(format!("{name}.png")),
            })
        }
        // Windows' thumbnails come from IShellItemImageFactory, which is COM
        // and not a command, so it does not fit this seam. Built-in decoders
        // only, until that is wired up properly.
        Platform::Windows => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_choose_who_renders_what() {
        assert_eq!(classify(Path::new("/x"), true), Kind::Directory);
        // Ours to decode.
        assert_eq!(classify(Path::new("a.png"), false), Kind::Image);
        assert_eq!(classify(Path::new("a.JPEG"), false), Kind::Image);
        // Ours to read.
        assert_eq!(classify(Path::new("a.txt"), false), Kind::Text);
        assert_eq!(classify(Path::new("main.rs"), false), Kind::Text);
        // No extension is far more often a script or a README than something
        // the system can draw.
        assert_eq!(classify(Path::new("Makefile"), false), Kind::Text);
        // The system's job: it can draw these and we cannot.
        assert_eq!(classify(Path::new("a.pdf"), false), Kind::System);
        assert_eq!(classify(Path::new("a.svg"), false), Kind::System);
        assert_eq!(classify(Path::new("photo.CR2"), false), Kind::System);
    }

    #[test]
    fn binaries_are_told_from_text_by_their_nul_bytes() {
        assert!(looks_like_text(b"hello\nworld\n"));
        assert!(looks_like_text("café ✓ — em dash".as_bytes()));
        assert!(looks_like_text(b""), "an empty file is not a binary");

        assert!(!looks_like_text(b"\x7fELF\x02\x01\x01\0\0\0"));
        assert!(!looks_like_text(&[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x00
        ]));
        // Control-character soup, even with no NUL.
        assert!(!looks_like_text(&[
            0x01, 0x02, 0x03, 0x04, 0x05, b'a', b'b'
        ]));
    }

    #[test]
    fn a_thumbnailer_file_is_read_for_its_three_useful_keys() {
        // The real one shipped with librsvg.
        let parsed = parse_thumbnailer(
            "[Thumbnailer Entry]\n\
             TryExec=/usr/bin/gdk-pixbuf-thumbnailer\n\
             Exec=/usr/bin/gdk-pixbuf-thumbnailer -s %s %u %o\n\
             MimeType=image/svg+xml;image/svg+xml-compressed;\n",
        )
        .expect("a thumbnailer");

        assert_eq!(parsed.exec, "/usr/bin/gdk-pixbuf-thumbnailer -s %s %u %o");
        assert_eq!(
            parsed.try_exec.as_deref(),
            Some("/usr/bin/gdk-pixbuf-thumbnailer")
        );
        assert!(parsed.handles("image/svg+xml"));
        assert!(!parsed.handles("application/pdf"));

        // Keys outside the section do not count.
        assert!(parse_thumbnailer("[Desktop Entry]\nExec=x\nMimeType=a/b;\n").is_none());
        // Neither does a file with no MIME types to match on.
        assert!(parse_thumbnailer("[Thumbnailer Entry]\nExec=x %o\n").is_none());
    }

    #[test]
    fn the_exec_line_placeholders_are_filled_in() {
        let command = expand_exec(
            "/usr/bin/gdk-pixbuf-thumbnailer -s %s %u %o",
            Path::new("/tmp/a b.svg"),
            Path::new("/tmp/out/thumb.png"),
            256,
        )
        .expect("a command");

        assert_eq!(command.program, "/usr/bin/gdk-pixbuf-thumbnailer");
        assert_eq!(
            command.args,
            ["-s", "256", "file:///tmp/a%20b.svg", "/tmp/out/thumb.png"]
        );
        assert_eq!(command.output, Path::new("/tmp/out/thumb.png"));

        // %i is the plain path, where a thumbnailer asks for one.
        let command = expand_exec("t %i %o", Path::new("/tmp/a.pdf"), Path::new("/o.png"), 128)
            .expect("a command");
        assert_eq!(command.args, ["/tmp/a.pdf", "/o.png"]);

        // With nowhere to write, there is no usable command.
        assert!(expand_exec("t %i", Path::new("/a"), Path::new("/o.png"), 128).is_none());
    }

    #[test]
    fn a_uri_escapes_what_a_shell_would_otherwise_mangle() {
        assert_eq!(
            file_uri(Path::new("/tmp/plain.svg")),
            "file:///tmp/plain.svg"
        );
        assert_eq!(
            file_uri(Path::new("/tmp/my holiday.svg")),
            "file:///tmp/my%20holiday.svg"
        );
        assert_eq!(
            file_uri(Path::new("/tmp/100%.svg")),
            "file:///tmp/100%25.svg"
        );
    }

    #[test]
    fn each_platform_asks_its_own_system() {
        let thumbnailers = vec![Thumbnailer {
            exec: "render -s %s %u %o".to_string(),
            try_exec: None,
            mime_types: vec!["application/pdf".to_string()],
        }];
        let out = Path::new("/tmp/out");
        let anything: &dyn Fn(&Path) -> bool = &|_| true;

        // Linux: whichever thumbnailer claims the MIME type.
        let command = thumbnail_command(
            Platform::Linux,
            &thumbnailers,
            Path::new("/a.pdf"),
            out,
            256,
            anything,
        )
        .expect("a command");
        assert_eq!(command.program, "render");
        assert_eq!(command.output, out.join("thumb.png"));
        // Nothing registered for it means nothing to run.
        assert!(thumbnail_command(
            Platform::Linux,
            &thumbnailers,
            Path::new("/a.svg"),
            out,
            256,
            anything
        )
        .is_none());

        // macOS: Quick Look, which names the result after the file.
        let command = thumbnail_command(
            Platform::MacOs,
            &[],
            Path::new("/tmp/a.pdf"),
            out,
            256,
            anything,
        )
        .expect("a command");
        assert_eq!(command.program, "qlmanage");
        assert_eq!(command.output, out.join("a.pdf.png"));
        assert!(command.args.contains(&"-t".to_string()));

        // Windows' thumbnails are COM, not a command, so there is nothing to
        // run and the built-in decoders have to do.
        assert!(thumbnail_command(
            Platform::Windows,
            &[],
            Path::new("C:\\a.pdf"),
            out,
            256,
            anything
        )
        .is_none());
    }

    #[test]
    fn a_registered_thumbnailer_whose_binary_is_missing_is_not_used() {
        // Not a formality: this very container declares librsvg's thumbnailer
        // and does not ship gdk-pixbuf-thumbnailer, so believing the
        // declaration means running a program that is not there.
        let declared = Thumbnailer {
            exec: "/usr/bin/render %u %o".to_string(),
            try_exec: Some("/usr/bin/render".to_string()),
            mime_types: vec!["application/pdf".to_string()],
        };
        let out = Path::new("/tmp/out");

        let missing: &dyn Fn(&Path) -> bool = &|_| false;
        let present: &dyn Fn(&Path) -> bool = &|_| true;

        assert!(!declared.installed(missing));
        assert!(declared.installed(present));
        assert!(thumbnail_command(
            Platform::Linux,
            std::slice::from_ref(&declared),
            Path::new("/a.pdf"),
            out,
            256,
            missing
        )
        .is_none());
        assert!(thumbnail_command(
            Platform::Linux,
            std::slice::from_ref(&declared),
            Path::new("/a.pdf"),
            out,
            256,
            present
        )
        .is_some());

        // No TryExec at all means nothing to check, so it is taken on trust.
        let unchecked = Thumbnailer {
            try_exec: None,
            ..declared.clone()
        };
        assert!(unchecked.installed(missing));
    }

    #[test]
    fn a_bare_try_exec_name_is_looked_for_along_the_path() {
        // The spec allows either form, and both appear in the wild.
        let seen = std::sync::Mutex::new(Vec::new());
        let record: &dyn Fn(&Path) -> bool = &|p| {
            seen.lock().unwrap().push(p.to_path_buf());
            false
        };
        assert!(!program_exists("some-thumbnailer", record));
        let looked = seen.lock().unwrap();
        assert!(
            looked.iter().all(|p| p.ends_with("some-thumbnailer")),
            "should have joined the name onto PATH entries"
        );
        assert!(looked.len() > 1, "PATH has more than one directory");

        // An absolute path is checked directly, not searched for.
        let checked = std::sync::Mutex::new(Vec::new());
        let record: &dyn Fn(&Path) -> bool = &|p| {
            checked.lock().unwrap().push(p.to_path_buf());
            true
        };
        assert!(program_exists("/opt/thumb/render", record));
        assert_eq!(
            checked.lock().unwrap().as_slice(),
            [PathBuf::from("/opt/thumb/render")]
        );
    }

    #[test]
    fn the_thumbnailers_this_machine_has_are_readable() {
        // Not asserting on the contents - a container may have none - but the
        // parse must survive whatever is really there.
        let found = load_thumbnailers(&thumbnailer_dirs());
        for thumbnailer in &found {
            assert!(!thumbnailer.exec.is_empty());
            assert!(!thumbnailer.mime_types.is_empty());
        }
    }
}
