# Changelog

Notable changes, newest first. Versions follow [semantic versioning]; until
1.0 the minor number is where breaking changes go.

[semantic versioning]: https://semver.org/

## Unreleased

### Changed

- **The window is four sectors and two seams.** The places list moved to the
  right, and the shell now sits directly under the panes and exactly as wide
  as them - it was a panel of its own running the full width, so it was wider
  than the panes by exactly the sidebar, and the two things that belong
  together were the two that did not line up. What was run here moved out of
  the shell drawer and under the places list, to that list's width. One seam
  runs the whole height and one the whole width, and both are remembered;
  before, each panel had an edge of its own. The second pane splits the
  top-left sector and nothing else, and a tree splits a pane inside that, so
  no arrangement of the panes moves the shell.

- **It opens with one panel now**, in both front-ends, which is XTree's
  arrangement rather than Norton's: a tree and its files with the whole width
  to show them in. Two panels is the right shape for a copy and the wrong one
  for the rest of the time. `Tab` asks for the second — it used to do nothing
  when there was only one — and `F12` folds it away, leaving whichever panel
  you were reading.
- **`F5` and `F6` in the graphical view ask where, in a field**, prefilled
  with the other pane's directory when there is one. It used to copy straight
  to wherever the other pane happened to be, with nothing shown and nothing to
  change; with one pane that would have been a copy into a directory the
  reader never saw. The terminal view has always asked. Extracting from an
  archive goes through the same field.
- **A folder comparison opens the second panel** rather than reading one that
  is not on screen. It changes nothing on disk, so it does not ask first.

### Added

- **A row of function keys under the panes in the graphical view**, as in every
  commander since Norton. It had none, which left `F5` a secret kept from
  anybody who had not read the key list. What it says is read out of the same
  table the keyboard uses, so it cannot drift: `F9` is *Select* in this
  front-end rather than *Sort*, and the bar says so because it is asking
  rather than remembering. The toolbar folds it away.
- **Folder history, in the other pane** (`Alt-H`): what was copied in, moved
  out, renamed, deleted or created here, newest first, with failures kept -
  "why is this file not here" is answered by the attempt that failed as often
  as by the one that worked. The journal screen answers "what did I do today";
  this answers "why does this folder look like this", which is the question
  you have while looking at it.
- **What was run here, beside the shell in the graphical view.** The terminal
  view has had this column since it got a shell screen; the drawer now has it
  too, on by default and off with `hist`. Clicking a line puts it on the
  command line without running it.

### Changed

- **Quick view and folder history came off the panes' own view switch.** That
  switch is list / grid / tree - three ways of drawing *this* pane's own
  directory. The other two are about the folder in the *other* pane, so
  choosing one from a pane's own header put an answer in the pane you were
  standing in about somewhere you were not. They are `F3` and `Alt-H`, both
  act on the opposite pane, and both are on the toolbar's view menu. A pane
  showing either now names the folder it is showing and drops the switch,
  which would otherwise offer to redraw a directory that pane is not showing.

- **The shell screen lists what was run in this directory**, beside it, with
  the one `Alt-P` is offering marked. A shell's own history is one list with
  no idea where you were; this is the half that is about here.
- **`Alt-P` and `Alt-N` walk what has been run**, offering what was run here
  before what was run anywhere else, and saying where when it differs. This
  works with any shell — the line is known before it is handed over, so it
  needs no hook, which is what makes it the first shell feature that is not
  poorer on `cmd`.
- **A tab says who opened it, by colour.** One the program opened on your
  behalf is drawn in the colour marks use, in both front-ends — so finding
  three of them later, you can tell which you asked for.
- **`Alt-Enter` opens where an entry happened**, as new tabs rather than by
  moving the panels — so looking at what a command did costs you nothing. A
  copy opens *both* ends, which is the question anybody asks afterwards: what
  do A and B look like now? Anything that has since been deleted or is on an
  unmounted disk is named rather than silently skipped.
- **A command can be taken back out of the account.** `Enter` on a command in
  the journal puts it on the command line, with a note saying where it
  originally ran if that is not where you are. It is not run: a line
  remembered from a week ago, in another directory, is exactly where an `rm`
  goes wrong.

- **Both front-ends can find the machine's drives.** The graphical one lists
  them in the sidebar; the terminal one has a third tab in `Ctrl-B`, beside
  Saved and Recent. Drives say how much room is left.
- **The sidebar shows what the machine has**: drives and volumes, then Home,
  Desktop, Documents, Downloads, Pictures, Music and Videos. Without them the
  only route to a second drive was typing its letter — and on Windows there is
  no root to walk up to that would reveal one, since `C:` and `D:` are two
  trees rather than two directories.

- **A shell that stays running under the terminal view.** One shell for the
  session rather than one per command, so `cd` means something: the directory
  it leaves you in is where the next command runs. `Ctrl-O` swaps between the
  panels and the shell's own screen, and keys go straight to it while it is
  showing.
- **A Windows directory reported by a shell is usable.** A `file://` URI
  always begins its path with a slash, so `C:\src` arrived as `/C:/src` —
  which looks like a path, is not one, and fails by silently not existing.
  PowerShell reports exactly that, so the panel refused to follow it and said
  nothing. Found by driving a real PowerShell rather than by reading code.
- **The terminal view honours the configured shell.** `shell` in
  `settings.toml` was read and then ignored there, so the setting the
  graphical view respects did nothing. It matters on Windows, where the
  machine's own answer is `cmd` and `cmd` has no seam for the hook.
- **A shell that cannot be asked where it is gets told instead.** `cmd` and
  `dash` have no seam to hook, so each command is now preceded by a `cd` to
  the directory the panel is showing — Far Manager's model on Windows, and
  for the same reason: half-sharing a directory is worse than not sharing
  one. A hooked shell is left alone, since sending it back would undo a `cd`
  the reader meant.
- **The graphical view's shell and panes follow each other.** A `cd` in the
  visible shell moves the active pane; moving a pane, or switching to the
  other one, sends the shell after it. Previously `cd here` was the only
  connection and it pointed one way.
- **A terminal tab can be pinned**, with the checkbox beside `cd here`. A
  pinned shell is left where it is: it stops following the panes and they
  stop following it — for a build running in one directory while you work in
  another. Without it, coupling the two means a shell you cannot keep still.
- **The graphical view follows its shell too.** A `cd` in the visible shell
  moves the active pane, which it never did before — it had `cd here` and
  nothing the other way.
- **`Ctrl-O` opens onto a line** naming the shell and saying whether it
  shares the directory and is recorded, or does neither.
- **`Alt-O` picks the shell**, and says which of them can be recorded. It
  matters most on Windows, where the machine's answer is `cmd` and `cmd` has
  no seam to hook — so without a way to choose, the shared directory was
  unreachable and unexplained.
- **`cd` is quoted the way each shell quotes.** `cd 'C:\src'` is an error in
  `cmd`, which has no single quotes at all, and `cmd` will not cross drives
  without `/d`. PowerShell doubles a quote inside a name where a POSIX shell
  escapes it.
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
