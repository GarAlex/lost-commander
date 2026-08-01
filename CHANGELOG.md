# Changelog

Notable changes, newest first. Versions follow [semantic versioning]; until
1.0 the minor number is where breaking changes go.

[semantic versioning]: https://semver.org/

## Unreleased

Nothing yet.

## 0.1.0 — 2026-08-01

First release.

### The program

Two front-ends over one engine, running unchanged on Linux, macOS and
Windows:

- **`lostc`** — a terminal UI: two panels, function keys, and a command line
  under them that runs in the directory being shown.
- **`lostc-gui`** — a graphical view: a sidebar of places, a breadcrumb trail,
  per-pane views, and a real shell in a drawer.

### What it does

- Copy, move, delete and compare, with live progress and a cancel
- Reading `zip`, `tar` (gz/xz/bz2), `7z` and `lha` archives as directories
- Folder compare and synchronise; a duplicate finder
- Find by name and by contents
- Quick view of text, markdown, images and raw bytes, and a byte editor
- The directory tree above the files, XTree's arrangement, with tags that
  survive walking to another directory
- Tabs, bookmarks, network locations, permissions, trash
- A journal of what was done, including commands run
- Named colour schemes, among them Norton Commander and XTree Gold

### For people building on it

- `lost-commander-core` is the engine with no user interface attached: the
  crate boundary is what guarantees it, since it depends on no windowing
  library.
- `lost-commander-ffi` is a C ABI for front-ends that are not written in
  Rust. Values cross as JSON, the caller polls, nothing unwinds into C, and
  every string and handle has one owner and one way back.

### Known gaps

- The terminal front-end has no shell of its own beyond the command line:
  no `Ctrl-O` to the shell's screen, and no shared current directory. That is
  a gap rather than a decision — it is what Norton and Midnight Commander do.
- Markdown renders in the graphical view and shows as markup in the terminal
  one. The parse is in the engine, so what is missing is drawing.
- Archives are read-only.
