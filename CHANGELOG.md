# Changelog

Notable changes, newest first. Versions follow [semantic versioning]; until
1.0 the minor number is where breaking changes go.

[semantic versioning]: https://semver.org/

## Unreleased

### Added

- **A shell that stays running under the terminal view.** One shell for the
  session rather than one per command, so `cd` means something: the directory
  it leaves you in is where the next command runs. `Ctrl-O` swaps between the
  panels and the shell's own screen, and keys go straight to it while it is
  showing.
- **The directory is shared both ways.** A `cd` in the shell moves the panel;
  moving the panel `cd`s the shell. The shell reports where it is through the
  `OSC 133` hook the graphical view already used — read rather than guessed
  at. A shell with no seam to hook still runs; the two simply stop following
  each other.

## 0.1.0 — 2026-08-01

First release.

### Licence

MPL-2.0. Changes to these files stay open, so improvements to the engine come
back; linking the code from a larger work of your own is explicitly allowed
and puts no licence on that work, which matters because the engine is meant to
be driven through a C ABI by front-ends it knows nothing about.

### Fixed before release

- **The status line shows what the program says.** `info` wrote to a field
  that only errors were ever read from, so roughly forty messages — including
  the opening line naming the help and quit keys — were composed and drawn
  nowhere.
- **The key bar reads `F1`, not `1`.** The bare number is the Commander
  convention and everybody who grew up with it reads it as the function key,
  but that was knowledge the bar assumed rather than supplied.
- **There is more than one way to quit the terminal view.** `F10` is the
  Commander key and stays, but a terminal may keep it for its own menu and
  never pass it on — GNOME Terminal does — which left a reader with no way
  out at all. `Ctrl-Q` always works.
- **`Ctrl-C` does something.** In raw mode it arrives as a keystroke rather
  than a signal, so it was being swallowed. It now cancels a running copy, or
  clears the command line, or quits when there is neither.
- **`Ctrl-Z` suspends** on Unix, giving the terminal back before it stops so
  the shell is usable afterwards. `fg` resumes. Windows has no job control.
- The manual said `F10` "is the only way to quit". It was, and that was the
  bug.

### The program

Two front-ends over one engine, running unchanged on Linux, macOS and
Windows:

- **`lostc`** — a terminal UI: two panels, function keys, and a command line
  under them that runs in the directory being shown. `Ctrl-O` hides the
  panels and shows what the shell has printed, with the terminal's own
  scrollback — the panels are drawn on the alternate screen, so nothing has
  to be captured for that to work.
- **`lostc-gui`** — a graphical view: a sidebar of places, a breadcrumb trail,
  per-pane views, and a real shell in a drawer.

### What it does

- Copy, move, delete and compare, with live progress and a cancel
- Reading `zip`, `tar` (gz/xz/bz2), `7z` and `lha` archives as directories
- Folder compare and synchronise; a duplicate finder
- Find by name and by contents
- Quick view of text, markdown, images and raw bytes, and a byte editor
- The directory tree above the files, XTree's arrangement, with tags that
  survive walking to another directory. Both front-ends show the row the
  arrows move and, separately, the directory the listing belongs to, plus a
  count of what is tagged out of sight
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
