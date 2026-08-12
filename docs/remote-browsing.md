# Browsing what cannot be mounted

A plan for reaching FTP, SFTP and WebDAV from all three front-ends.

Nothing here is built yet. This is the design and the order to build it
in, written down before any of it, because the two expensive decisions -
how a listing is parsed and where a password lives - are the ones that
are painful to change once there is data in the field.

## The hole

`mount::connect` answers one question: *what local path can this be
listed at?* That is the right question for SMB, AFP and NFS, which the
platform can genuinely attach, and it is the wrong question for FTP.
Windows cannot make an FTP site into a path without a filesystem driver,
so `plan_for` returns `Unsupported` and the address is refused.

But "the platform cannot turn this into a path" is not the same as "this
program cannot show it to you". `MapPlan` has three arms:

```rust
Direct(PathBuf)                  // already browsable
Command { program, args, hint }  // run this, then find the mount
Unsupported { reason }
```

FTP lands in the third only because there is no way to say *not a path,
but I can read it myself*. That is a missing arm, not a missing
capability.

## The precedent, which is already shipping

A ZIP is not a filesystem either, and panes have listed inside archives
since early on. `core/src/archive/` is one module per format behind a
two-method trait:

```rust
pub trait Reader: Send + Sync {
    fn list(&self, archive: &Path, password: Option<&str>) -> io::Result<Vec<Member>>;
    fn read(&self, archive: &Path, member: &str, password: Option<&str>) -> io::Result<Vec<u8>>;
}
```

and the front-ends have a pane mode - `InArchive` in the Windows one -
where rows come from the engine rather than from `std::fs`. Everything
that mode needed already exists: a listing of things that are not paths,
a fetch-to-temp for opening one, and a transfer that crosses in and out.

A remote is the same shape with latency and credentials added. This is
why the work is worth doing as an adapter rather than as a mount, even
on the platforms where mounting is possible: one implementation covers
all three front-ends and all three operating systems, with no keychain,
no admin rights, and no ten-second block while the OS decides.

## The seam

A fourth arm, and a trait beside `archive::Reader`:

```rust
pub enum MapPlan {
    Direct(PathBuf),
    Command { program: String, args: Vec<String>, hint: Option<PathBuf> },
    /// Not a path on this machine, and does not need to be: the engine
    /// can list and fetch it directly.
    Browse(Protocol),
    Unsupported { reason: String },
}
```

```rust
/// A place whose contents are not reachable through the filesystem.
pub trait Remote: Send + Sync {
    fn list(&mut self, at: &str) -> io::Result<Vec<Member>>;
    fn read(&mut self, path: &str) -> io::Result<Vec<u8>>;
    fn write(&mut self, path: &str, bytes: &[u8]) -> io::Result<()>;
    fn remove(&mut self, path: &str) -> io::Result<()>;
    fn mkdir(&mut self, path: &str) -> io::Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()>;
}
```

Three differences from `Reader`, each with a reason:

- **`&mut self`, and the connection is held.** An archive is opened per
  call because opening it is cheap and local. A control connection is
  neither, and an FTP server will drop an idle one. The session lives in
  a pool keyed by host and user, and reconnects when it finds itself
  dropped rather than failing the listing.
- **It writes.** An archive is read-only here; F5 into a remote is the
  point of having one.
- **`Member` is reused rather than reinvented.** It already carries the
  fields a listing needs - a `/`-separated path normalised on the way
  out, size, modified, `is_dir`, optional mode - and reusing it means
  the pane code that already renders archive members renders these
  without knowing what they are. `packed` and `encrypted` stay `None`
  and `false`. If a field is genuinely needed that `Member` lacks, add
  it to `Member` rather than forking the type.

`connect()` keeps its signature. A `Browse` plan returns the URL rather
than a path, and the caller can tell the two apart because it asked for
one thing and can see which it got; this is the one place where the ABI
shape below matters more than the Rust shape.

## The part that will actually cost the time

Not the protocol. `LIST` parsing.

FTP has no standard listing format. A server may answer in Unix `ls -l`
style, in DOS style, or in either with a locale applied to the month
name. A year is shown for old files and a clock for recent ones, so a
file's timestamp changes format when it turns six months old. `MLSD`
(RFC 3659) fixes all of this and most modern servers offer it - and the
ones worth supporting FTP for at all are often the ones that do not.

So: `MLSD` when `FEAT` advertises it, and a `LIST` parser behind it with
a fixture per format. This is where the tests go, and they are cheap -
a listing is a string, and the parser is pure. Collect real output from
several servers first and commit the fixtures; do not write the parser
from the RFC and hope.

**Guess badly and it is invisible.** A misparsed line becomes a file
with the wrong size or the wrong date, not an error. That is the failure
mode this design is most exposed to, and the reason a listing that
cannot be parsed must be reported as a listing failure rather than
silently skipped - a directory that shows nine of its twelve files is
worse than one that refuses to show any.

## Credentials, which is a decision and not a detail

Three options, in order of how much I would recommend them:

1. **Ask each session, hold in memory.** Nothing is stored, nothing
   leaks, and it costs a prompt per connection per run. This is what
   archive passwords already do, so there is a precedent in the codebase
   and in the reader's expectations.
2. **The platform's own store** - Credential Manager, Keychain, Secret
   Service. Correct, and three implementations plus three failure modes,
   one of which (a headless Linux with no Secret Service running) has no
   good answer.
3. **A file under the config directory.** Do not. A file manager that
   writes passwords to disk in a format its own users will find has
   earned every bad thing said about it afterwards.

**Recommendation: (1) now, with the seam shaped so (2) can be added
later without changing callers.** Anonymous FTP needs none of this, so
the first working version needs no decision at all.

Note the interaction with the Store: the packaged Windows build is
sandboxed, so whatever is chosen has to work inside an MSIX container.
Credential Manager does; a file under `%LOCALAPPDATA%` is redirected.

## The ABI

Mirrors the archive calls, which the front-ends already know how to use:

```
rcmd_remote_list(url, at)             -> { entries: [...] } | { error }
rcmd_remote_fetch(url, path, to)      -> { path } | { error }
rcmd_remote_put(url, path, from)      -> { ok } | { error }
rcmd_remote_delete(url, path)         -> { ok } | { error }
rcmd_remote_mkdir(url, path)          -> { ok } | { error }
rcmd_remote_rename(url, from, to)     -> { ok } | { error }
rcmd_remote_close(url)                -> { ok }
```

Everything is blocking and named by URL rather than by a handle, because
a handle across a C ABI means the front-end owns a lifetime it cannot
see, and every front-end here has already shown it will forget to close
one. The pool inside the engine holds the connection; `close` is a hint,
not a requirement, and a pool entry idle past a timeout is dropped
anyway.

Transfers of any size go through the existing `FileJob` and its progress
channel rather than these calls, so a slow download is cancellable and
shows a bar. `fetch` and `put` above are for the small internal cases -
F3 opening a file, a preview - which is exactly how archives already
work.

## Front-end work

**Engine first, and all of the above lands before any front-end
changes.** Each front-end then gets roughly what it already has for
archives:

- **tui** - already has a Connections screen; it gains the browse arm.
- **egui** - a pane mode, and the same fetch-to-temp for F3/F4.
- **WinUI** - `InRemote` beside `InArchive`. Most of the work is
  already-written code being told about a second case: `Chosen()`,
  `PaneDragStarting`, the F5/F6/F8 routing, and the path bar.

The sidebar needs nothing. A network location is already pinned by
address and already clicked to connect; a `Browse` plan changes what
happens next, not what the row is.

## Order, and where to stop and look

1. `MapPlan::Browse`, the `Remote` trait, the pool. No protocol yet.
   Tests: a plan for every protocol on every platform.
2. FTP listing: `MLSD`, then `LIST` with fixtures per server style.
   **Stop here and check the fixtures against real servers.**
3. FTP read and write, wired to `FileJob` for progress and cancel.
4. The ABI, and the WinUI pane mode. **Stop here: this is the first
   point where it is usable, and worth living with for a while.**
5. SFTP behind the same trait. Cheaper than FTP - one listing format,
   real metadata, and a mature crate - and the reason the trait is
   shaped for more than one protocol.
6. WebDAV, if it is still wanted by then.

Steps 1-4 are the plan. Steps 5-6 are what the plan is *for*, and
neither should be started until 4 has been used in anger.

## Not in scope

- Mounting FTP as a drive letter. It needs a filesystem driver, which is
  a different program with a different signing story.
- Explorer's `ftp://` shell namespace. It browses, but as a shell folder
  with no Win32 path, so nothing here can list it. It looks like a
  solution and is not.
- FTPS and FTP-over-TLS beyond what the client crate gives for free.
- Resume of interrupted transfers. Wanted eventually; not before 4.

## HTTP and HTTPS, which is three features wearing one name

Asked for as: connect to a URL, show the page as a file, its referenced
resources as files beside it, and its links as folders. It is a good idea
and it is three ideas, with very different value and very different risk.
Only the third is the one it sounds like.

**1. WebDAV.** Already step 6 above, and worth saying out loud that this
*is* HTTP-as-a-filesystem, done by servers that agreed to it. Real
directories, real sizes, real dates, `PROPFIND` returns a listing that
needs no guessing. Everything below is what you do when the server never
agreed to anything.

**2. Directory indexes.** An Apache or nginx autoindex page is a listing
that happens to be wearing HTML, and parsing it is closer to the `LIST`
problem above than to the web: a handful of layouts, all tabular, sizes
and dates usually present. High value - this is how most public file
trees are served - and the ambiguity is bounded. Detect it (the server's
own generator comment, or a table whose rows are all links with sizes),
and fall back to (3) when it is not one.

**3. A page as a folder.** The document itself as one entry, its
subresources - `src`, `href`, `srcset` - as files beside it, and its
links as directories. This is genuinely useful for reading a page's
sources and pulling its assets down with F5, and it needs rules, because
the web is not a tree:

- **One request per listing.** A link becomes a directory row; it is not
  followed until the reader steps into it. Nothing recurses, ever. A
  file manager that crawls is a crawler, and somebody will point one at
  a site that does not want it.
- **A cap, and a stated one.** Some pages have ten thousand links. Show
  the first N and say that is what happened, rather than showing an
  unusable pane or a truncated one that looks complete.
- **Sizes are unknown, not zero.** `Member.size` is a `u64` and a
  listing of resources has no sizes without a `HEAD` each, which is one
  request per row. Off by default; a column of confident zeroes is worse
  than a column of blanks. This is the one place `Member` may need a
  field rather than a convention.
- **No cookies, no scripts, no forms.** What is fetched is what `curl`
  would fetch.

**The honest limitation, which belongs in the UI and not in a footnote:**
a page whose content is assembled by JavaScript has almost nothing in
its HTML. On a modern application this listing is a document, three
bundles and a favicon - technically correct and useless. That is not a
bug to be fixed later; it is what the format is. So an empty or nearly
empty listing must say *why* - "this page builds itself in the browser;
there is nothing to list" - rather than drawing an empty pane the reader
will read as a failure.

F3 needs nothing new. The viewers already route by what a file is and
already detect encoding; HTML and JS are text, and `fetch`-to-temp is
the same path archives use. F4 should refuse: editing a copy of a
resource fetched over HTTP has nowhere to save to.

Two platform notes, both learned the expensive way:

- The macOS Store build needs `com.apple.security.network.client` before
  any of this works, and its absence fails silently rather than loudly.
- Plain `http://` is a cleartext request, which the App Store asks about;
  default to `https://`, and make a plain one a thing the reader typed
  on purpose rather than a thing the program chose.

**Order: (1) as planned, (2) with it, (3) after both.** The first two are
listings that a server intended to be read; the third is inference, and
inference is what should be built last and behind a rule that stops it
running away.

## Open questions

- **Which FTP crate.** `suppaftp` is maintained and has TLS; check its
  listing support before relying on it, since a crate that hands back
  parsed entries is worth more here than one that hands back lines.
- **Whether SFTP should come first.** It is easier, more used, and would
  prove the trait with less of the listing risk. The argument against is
  that FTP is the one that is currently refused out loud, and so the one
  a reader has already been told about.
