# lost-commander

A file manager in Rust, in the Norton Commander tradition, with two front-ends
over one engine. It runs unchanged on **Linux, macOS and Windows**.

It opens with **one pane**, which is XTree's arrangement rather than Norton's:
a tree and its files with the whole width to show them in. `Tab` opens a
second pane and `F12` folds it away; a copy, a move, a comparison or a preview
opens it by itself.

- **`lostc`** — a terminal UI. Function keys, no mouse required.
- **`lostc-gui`** — a graphical view laid out for a pointer rather than a
  teletype: four sectors with two seams. The panes are top left, the places
  list top right, the shell directly under the panes and exactly as wide as
  them, and what was run in that directory under the places list and to its
  width.

Everything underneath — listing, sorting, marking, copy/move/delete with
progress and cancel, archives, bookmarks, network locations, the directory
tree, the account of what was done — lives in one library that neither front-
end owns.

## The terminal view

One panel, a status line, the command line, and the function keys. The picture
is generated from the real drawing code by a test, which fails if it falls
behind the program.

```
┌──────────────────────────────── ~/src/lost-commander/core ───────────────────────────────────────┐
│Name                                                                           Size Modified      │
│ ..                                                                            <UP>               │
│ archive                                                                      <DIR> 04.08.26 11:29│
│*entry.rs                                                                     5.52K 04.08.26 11:29│
│*fsops.rs                                                                     9.07K 04.08.26 11:29│
│ panel.rs                                                                     28.1K 04.08.26 11:29│
│ tree.rs                                                                      14.3K 04.08.26 11:29│
│                                                                                                  │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
F1 help   Tab opens a second panel   F10 or Ctrl-Q quits
~/src/lost-commander/core> cargo test█
F1Help   F2Rename F3View   F4Edit   F5Copy   F6Move   F7MkDir  F8Delete F9Sort   F10Quit
```

The third row from the bottom is the command line: what you type goes there
and Enter runs it in the directory being shown. `Ctrl-O` puts the panels away
and shows what the shell has printed.

On the command line, `%f` is the file under the cursor, `%s` the marked names
(or the cursor file when nothing is marked), `%d` the other pane's directory,
and `%%` a literal percent — each name quoted for the shell. While the line
holds a placeholder, the status row shows what will actually run, so Enter is
never a surprise. The same placeholders work on the graphical view's command
line.

## The graphical view

![lostc-gui](docs/lostc-gui.png)

## Installing

Nothing is packaged yet; this is the first release. From source:

```sh
cargo install lost-commander-tui     # lostc, the terminal one
cargo install lost-commander-egui    # lostc-gui, the graphical one
```

`packaging/` holds what a distribution package needs: man pages, completions
for bash, zsh and fish, a desktop entry, AppStream metainfo and an icon. A
package should install them as:

```
packaging/lostc.1                        -> /usr/share/man/man1/
packaging/lostc-gui.1                    -> /usr/share/man/man1/
packaging/completions/lostc.bash         -> /usr/share/bash-completion/completions/lostc
packaging/completions/_lostc             -> /usr/share/zsh/site-functions/
packaging/completions/lostc.fish         -> /usr/share/fish/vendor_completions.d/
packaging/lostc-gui.desktop              -> /usr/share/applications/
packaging/io.github.garalex.lost-commander.metainfo.xml -> /usr/share/metainfo/
packaging/io.github.garalex.lost-commander.svg          -> /usr/share/icons/hicolor/scalable/apps/
```

The desktop entry, the metainfo id and the icon filename all have to keep
agreeing, or a software centre shows an entry with no icon.

## Building

Needs a Rust toolchain (1.74 or newer). Nothing else on Windows or macOS; on
Linux the graphical front-end needs the usual X11 development packages that
any windowed Rust program does.

```sh
cargo build --release          # both binaries, into target/release/
```

The two front-ends are separate crates, so you can build one without paying
for the other. The terminal binary pulls in no windowing or GPU dependency at
all:

```sh
cargo build --release -p lost-commander-tui    # lostc alone
cargo build --release -p lost-commander-egui   # lostc-gui alone
```

## Running the terminal front-end

```sh
cargo run --bin lostc                      # the current directory
cargo run --bin lostc -- ~/src ~/documents # left, and the second when it opens
cargo run --bin lostc -- --list ~/src      # print a listing and exit
cargo run --bin lostc -- --help
```

```
Tab   other panel      Enter  open           Backspace  parent
F1    help             F2     rename         F3         view
F4    edit             F5     copy           F6         move
F7    mkdir            F8     delete         F9         sort
F10   quit             Space  mark           Ctrl-H     hidden files
F11   saved places     F12    second panel   Alt-T      tree
Ctrl-Q quit            Ctrl-C cancel or quit  Ctrl-Z    suspend
Ctrl-O shell screen    Escape back to the tree
```

`Tab` opens the second panel when only one is showing. `F12` folds it away
again, leaving whichever panel you were reading.

**If `F10` does nothing, your terminal is eating it.** GTK binds F10 to
"open the menubar" for every application, so GNOME Terminal, Terminator and
Tilix all take it by default — some setups bind it to close the window
instead. The keystroke is consumed by the emulator one layer above the
pseudo-terminal, so no program running inside can see it. Turn it off in GNOME
Terminal under Preferences → General → *Enable the menu accelerator key*, or
use `Ctrl-Q`, which nothing intercepts.

If you do press it, the menubar opens and the terminal is then in menu
navigation, where plain letters are mnemonics rather than input — a `q` there
is *File → Quit*, which closes the whole terminal application and every tab in
it. Inside `lostc`, `Ctrl-C` cancels a running copy, clears the command line,
or quits when there is neither; letters go to the command line, so `q` does
not quit.

`--list` is the whole listing pipeline with no terminal attached, which makes
it useful for a quick check that a build works, and for scripting.

## Running the graphical front-end

```sh
cargo run --bin lostc-gui                        # current directory, or the saved windows
cargo run --bin lostc-gui -- ~/src ~/documents   # left, and the second when it opens
cargo run --bin lostc-gui -- --grid              # start both panes in the icon grid
cargo run --bin lostc-gui -- --tree              # tree above the files, XTree's arrangement
cargo run --bin lostc-gui -- --preview           # second pane follows the first, as F3 does
cargo run --bin lostc-gui -- --history           # second pane shows what was done in the first's folder
cargo run --bin lostc-gui -- --help
```

The window is four sectors and two seams:

```
+------------------------+---------------+
|  panes                 |  places       |
|  (the second pane and  |  drives,      |
|   a tree split this    |  folders,     |
|   sector, and only     |  bookmarks    |
|   this one)            |               |
+------------------------+---------------+
|  F1 Help  F2 Rename ...                |
+------------------------+---------------+
|  shell                 |  history      |
+------------------------+---------------+
```

The vertical seam runs the whole height and the horizontal one the whole
width, so the shell is exactly as wide as the panes above it and the history
exactly as wide as the places list above it. Both seams remember where you
left them. Down the left is the rail of workspaces.

**A tab is a window, not a directory.** Each workspace carries its own two
panes, how they are arranged, what each is drawing, which one you were in,
which halves of the window are showing, and the shell that was standing there.
Switching workspaces puts all of it back, down to the cursor, the marks and
the scroll position. `Ctrl-T` forks the workspace you are in. The rail's `>`
widens it from a name to the whole path and the shell.

`Alt-1` through `Alt-9` jump to a workspace by position, `Alt-0` bounces to
the one that had the window before this one, and `Ctrl-Tab` walks them in
order. Double-click a rail entry to name it — two workspaces on the same
folder are otherwise indistinguishable — and drag one to reorder; the wide
rail has a `×` per row, and middle-click closes in either width.

Workspaces are written down as they change — opened, closed, switched,
rearranged — and the next start with no arguments opens them again. The shell
process is not saved; its kind and its directory are, and a fresh one of the
same kind opens there when the window next comes up. A saved window whose
directory has since gone is named rather than opened.

**One shell per workspace.** The drawer has no `+`: the two things you can do
to a shell are *replace* it — the picker names what is running, choosing
another kind swaps it, and choosing the same kind again restarts it — and
*clean* it, which wipes the screen and the scrollback. Closing a workspace
ends its shell.

Either row can have the window to itself: `Ctrl-O` gives it to the shell and
`Ctrl-Shift-O` to the panes, and the same key hands it back. `Ctrl-Alt-O`
chooses whether the bottom is a shell that stays or a line that runs one
command. All three are on the view menu.

While only one half is showing, the panes and the shell are **independent** —
neither follows the other. They fall back into step when both return, and the
half that had the window is the one that wins. Typing a command, walking the
history or sending a name to the prompt brings the shell back on its own.

Each pane carries its own view switch in its header: a dense detail list, a
grid of large icons, or the directory tree — three ways of drawing that pane's
own directory.

Two other things can be put on a pane: **quick view** (`F3`) and **folder
history** (`Alt-H`). Both are about the folder you are standing in, so both
are drawn in the pane you are *not*, and the one you asked from keeps the
cursor. A pane showing either names the folder it is showing and drops the
list/grid/tree switch. Both are on the toolbar's view menu as well as on the
keys, and a pane opened only to answer folds away again when it stops.

Folder history is the account read from where you are standing rather than by
day: what was copied in, moved out, renamed, deleted or created here, newest
first, with failures kept. `Ctrl-J` opens the whole journal, by day.

Under the panes is the row of function keys, read out of the same table the
keyboard uses. The toolbar can fold it away.

`Ctrl-Z` undoes the last file operation, read out of the account: the last
copy's products are removed, the last move or rename goes back, a made
directory is removed if empty. The plan is shown in full before anything
moves — every step, and every item that cannot be reversed with its reason.
A changed copy is refused rather than taken; a permanent delete is named
irreversible; a trashed file is restored from the trash. The reversal is
itself recorded, so undoing an undo is just `Ctrl-Z` again.

`Alt-B` opens the trash — also a row in the places column: everything
deletion kept, each with where it came from and when. Restore puts a thing
back (refusing if something else now sits where it came from, and remaking
its home if that has gone); purge and *Empty the trash* are named not
reversible, because they are not.

`Ctrl-Shift-P` opens the palette: every action, searchable by name or key,
Enter runs the highlighted one. It reads the same table as the key bar and
the keyboard, so it can neither offer what a key cannot do nor miss what one
can — an action added without a palette name refuses to compile.

There is one more argument, and it is how the view gets checked without a
human at the screen:

```sh
cargo run --bin lostc-gui -- --screenshot shot.png ~/src ~/documents
```

It renders a few frames, writes a PNG and exits. The picture above was made
with it.

**Drag and drop.** Rows drag between the panes — a copy, with the
destination visibly under the pointer; `Shift` at release makes it a move. A
marked row drags the whole marked set. Rows dropped on a rail workspace copy
to its folder, and files dropped in from outside land in the pane they were
dropped on.

**Where a copy goes.** `F5` and `F6` ask, in a field, with the other pane's
directory already in it when there is one — one Enter. With a single pane the
field offers the current directory, to be edited. Nothing is copied into a
directory that is not on screen.

## What each front-end does

Both drive the same engine, so listing, sorting, marking, copy/move/delete
with progress and cancel, archives, folder compare and sync, the duplicate
finder, find-by-name-and-contents, permissions, tabs, bookmarks, network
locations, the directory tree, the trash and the journal work the same in
either.

The terminal one is not a cut-down version of the graphical one — it is the
portable one. What it does not do is mostly what a terminal cannot:

| | `lostc` | `lostc-gui` |
|---|---|---|
| File-type icons, grid of large icons | — | yes |
| Image viewing, and crop/rotate/resize | — | yes |
| Built-in text editor | hands the file to `$EDITOR` | yes |
| Running commands | a command line, `Ctrl-O` for the output | a shell in a drawer, output in the window |
| A shell that stays put | one per session | one per workspace |
| The shell and the panel sharing a directory | yes, both ways | yes, both ways, and a shell can be pinned out of it |
| Session recording (`rec`) | — | yes |
| Named colour schemes | one | several |
| Function keys along the bottom | yes | yes |
| What was run here, beside the shell | yes | yes |
| What was done in *this folder* | `Alt-H`, in the viewer | `Alt-H`, in the other pane |
| Undo the last operation | `Alt-Z` | `Ctrl-Z` |
| Workspaces | tabs per pane | a rail of windows |
| Markdown rendered rather than shown as markup | yes — structure by characters | yes |

Reading bytes as hex, the directory tree, tabs and the journal are in both.

**History** — in both front-ends — sits beside the shell: what was run, newest
first. It opens on `here`, the commands from the directory the shell is
standing in; `all` is the rest, this directory first, with the ones from
somewhere else drawn dimmer and naming their folder on hover. Clicking a line
puts it on the command line without running it.

**The quick filter** (`Alt-F`) narrows the listing as you type — `Ctrl-F`
finds on the disk; this filters what is already on screen. Enter keeps it,
Escape takes it off, and it shows in the pane's heading while it is on. What
the F-keys act on is what the filter leaves visible, so filter plus `Ctrl-A`
is "mark all the `.log` files" — and a mark made before the filter went on
survives being hidden by it. Changing directory takes the filter off, since a
filter is about the folder it was typed in.

**Pinned commands.** `pin cargo test` on the command line puts that line on
top of this folder's history, permanently; `pin` the same line again takes it
off. The line is kept as typed, so a pinned `%f` template stays a template.
In the window, right-clicking a history row pins it and right-clicking a
pinned row unpins; the shelf sits above the history in both front-ends,
marked. Pins live in `pinned.toml`. A plain click on any history or pinned
row puts the line on the command line — the safe verb — and `Ctrl`-click runs
it, expanding a pinned template against the cursor first.

On Windows the shell picker also offers WSL's default distribution when
`wsl.exe` is present, and the panel-to-shell `cd` speaks WSL's language for
it: `C:\src` becomes `/mnt/c/src`.

In the window it is the bottom-right sector, under the places list; a filter
box under its header narrows it as you type, and `Alt-R` puts the keyboard
there, bringing the column up if it is hidden. `hist` turns the column off,
and hiding the places list takes it along. In the terminal view it is beside
the shell screen; `Alt-P` / `Alt-N` walk the same list, and `Alt-R` is a
shell-style reverse search over the whole account, here first — something no
shell's own `Ctrl-R` can do, because no shell knows where you were standing.
`Alt-R` again steps to the next match, Enter takes the offer onto the command
line without running it, Escape gives back exactly what was typed.

**On the shell.** `lostc` keeps one shell running underneath the panels for
the whole session, the way Midnight Commander does. Typing goes to the command
line and Enter hands the line to that shell; `Ctrl-O` swaps between the panels
and the shell's own screen, where you can work in it directly.

Because it is one shell rather than one per command, `cd` means something: the
directory it leaves you in is where the next command runs. It is shared both
ways — a `cd` in the shell moves the panel, and moving the panel `cd`s the
shell. The shell reports where it is through an `OSC 133` hook.

A shell with no seam to hook — `cmd`, `dash` — cannot be asked, so each
command is preceded by a `cd` to the directory the panel is showing. `Ctrl-O`
opens onto a line saying which of the two you have, and `Alt-O` changes it.

## Testing

```sh
cargo test                     # all 1155, from the workspace root
```

From the root that is everything, because the root is a virtual manifest and
cargo defaults to every member of one. From *inside* a crate directory it is
that crate alone, which is a fast loop but easy to mistake for a full run —
say `cargo test --workspace` there if you meant all of it.

Per crate, when you want a fast loop:

```sh
cargo test -p lost-commander-core    # 682 - the engine, seconds to build
cargo test -p lost-commander-egui    # 237 - the graphical view
cargo test -p lost-commander-tui     # 130 - the terminal view
cargo test -p lost-commander-ffi     # 106 - the C ABI
```

The engine's tests cover the whole of the file manager's behaviour with no
user interface attached, and they build in a fraction of the time the
graphical crate does: `core` depends on no windowing library, and the crate
boundary is what guarantees it.

Some tests want real programs and skip rather than fail without them — zsh,
fish and dash for the shell-integration tests, the system's own `zip` and
`tar` for some archive fixtures. A machine with more of those installed runs
more tests.

### Driving the real thing

The tests do not substitute for running it. `harness/` holds two working
harnesses with their own README: `gui_archives.sh` drives the graphical binary
under Xvfb with xdotool and scrot, and `tui_journal.py` drives the terminal
binary through a pty with pyte.

## Layout

Four crates in one workspace. Each front-end depends on `core`; `core` depends
on none of them, and the compiler is what keeps it that way.

```
core/    the engine — filesystem and state, no drawing
tui/     lostc      — ratatui + crossterm
egui/    lostc-gui  — eframe/egui
ffi/     a C ABI, for front-ends that are not written in Rust
```

`ffi/` exists because a front-end need not be a Rust one. Values cross as
JSON, the caller polls and nothing calls back, no panic unwinds into C, and
every string and handle has one owner and one way back. A native Windows
front-end written in C# is built on it.

## Where things are kept

One directory per platform:

| | |
|---|---|
| Linux | `~/.config/lost-commander/` |
| macOS | `~/Library/Application Support/lost-commander/` |
| Windows | `%APPDATA%\lost-commander\` |

Seven things are kept, each in its own file:

| | | |
|---|---|---|
| `settings.toml` | how you like it | when you change something |
| `bookmarks.toml` | places you saved | when you save or remove one |
| `recent.toml` | places you have been | every time a pane moves |
| `workspaces.toml` | the windows you had open | as they change |
| `pinned.toml` | commands kept on top, per folder | when you pin or unpin |
| `journal/shell-*.jsonl` | what you ran | as each command is run |
| `journal/files-*.jsonl` | what was done to files | as each thing happens |

The two histories are one file per day — the `*` is a date, as in
`shell-2026-07-28.jsonl` — appended a line at a time. Keeping thirty days is
deleting the files older than thirty. They are plain JSON lines, so `grep` and
`jq` read them.

## License

Mozilla Public License 2.0 — see [LICENSE](LICENSE).

Copyleft, per file: changes to *these* files must be published under the same
licence. Linking this code from a larger work of your own is explicitly
allowed and puts no licence on that work (§3.3).

### Contribution

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work shall be licensed as above, without any additional
terms or conditions.
