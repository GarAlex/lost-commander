# lost-commander

A file manager in Rust, in the Norton Commander tradition, with two front-ends
over one engine. It runs unchanged on **Linux, macOS and Windows**.

It opens with **one pane**, which is XTree's arrangement rather than Norton's:
a tree and its files with the whole width to show them in. The second pane is
for the things that are actually about two places at once — a copy, a move, a
comparison, a preview — and every one of those brings it up by itself. `Tab`
asks for it, `F12` folds it away.

- **`lostc`** — a terminal UI. Function keys, no mouse required.
- **`lostc-gui`** — a graphical view laid out for a pointer rather than a
  teletype: four sectors with two seams. The panes are top left, the places
  list top right, the shell directly under the panes and exactly as wide as
  them, and what was run in that directory under the places list and to its
  width. One seam runs the whole height and one the whole width, so the
  things that belong together stay the same size as each other.

Everything underneath — listing, sorting, marking, copy/move/delete with
progress and cancel, archives, bookmarks, network locations, the directory
tree, the account of what was done — lives in one library that neither front-
end owns.

## The terminal view

One panel, a status line, the command line, and the function keys. This is
generated from the real drawing code by a test, which fails if this picture
falls behind the program - a README's screenshot is otherwise a photograph of
whatever it looked like the day somebody typed it.

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

`Tab` opens a second panel beside this one, and `F12` puts it away again.

The third row from the bottom is the command line: what you type goes there
and Enter runs it in the directory being shown, exactly as in Norton
Commander. `Ctrl-O` puts the panels away and shows what the shell has
printed.

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

`Tab` opens the second panel when only one is showing, so it is never a key
that does nothing. `F12` folds it away again, leaving whichever panel you were
reading — not the left one by default, which would move you somewhere else.

There is more than one way to quit on purpose, and the key bar shows both.

**If `F10` does nothing, your terminal is eating it.** GTK binds F10 to
"open the menubar" for every application, so GNOME Terminal, Terminator and
Tilix all take it by default — some setups bind it to close the window
instead. The keystroke is consumed by the emulator one layer above the
pseudo-terminal, so nothing is sent and no program running inside can see it.
Turn it off in GNOME Terminal under Preferences → General → *Enable the menu
accelerator key*, or just use `Ctrl-Q`, which nothing intercepts.

Worth knowing what happens next if you do press it: the menubar opens — it
does so even when hidden — and the terminal is then in menu navigation, where
plain letters are mnemonics rather than input. A `q` at that point is *File →
Quit*, which closes the whole terminal application and every tab in it. One
reader lost all their tabs that way, and the bar telling them "10 Quit" is
what suggested the `q`. `Ctrl-C` cancels a running copy, or clears the command line, or
quits when there is neither, which is what the keystroke means everywhere
else in a terminal. Letters go to the command line, so `q` does not quit.

`--list` is the whole listing pipeline with no terminal attached, which makes
it useful for a quick check that a build works, and for scripting.

## Running the graphical front-end

```sh
cargo run --bin lostc-gui                        # current directory
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
width, so a shell is exactly as wide as the panes it belongs to, and the list
of what was run here is exactly as wide as the places list above it. Both
seams remember where you left them.

**A tab is a window, not a directory.** Down the left is the rail of
workspaces: each carries its own two panes, how they are arranged, what each
is drawing, which one you were in, and the shell that was standing there.
Switching workspaces puts all of it back, down to the cursor, the marks and
the scroll position — they are meant to feel like separate windows rather
than separate folders. `Ctrl-T` forks the workspace you are in, since what
you have set up is usually what you were about to set up again. The rail's
`>` widens it from a name to the whole path and the shell.

*Save workspaces* on the view menu writes them down, and the next start with
no arguments opens them again. A shell is not saved — the process is gone and
its scrollback with it — but its directory is, and the account already holds
what was run there. A saved window whose directory has since gone is named
rather than opened.

Either row can have the window to itself: `Ctrl-O` gives it to the shell and
`Ctrl-Shift-O` to the panes, and the same key hands it back. (`Ctrl-Alt-O` is
the third of that family: whether the bottom is a shell that stays or a line
that runs one command.) They are on the view menu too, so nothing here is
keyboard-only.

While only one half is up the two are **independent** — a `cd` in a shell
nobody can see has no business moving a pane nobody is looking at, and the
same in reverse. They fall back into step when both return, and the half that
had the window is the one that wins: it is where the work just happened.
Typing a command, walking the history or sending a name to the prompt brings
the shell back on its own, because answering into a shell that is not on
screen looks exactly like a swallowed keystroke.

The second pane splits the top-left sector and nothing else; a tree splits a
pane the other way, inside it. That is what keeps the arrangement stable: no
matter how the panes are divided, the shell under them does not move.

`Tab` opens the second pane and `F12` folds it away. A folder comparison
opens it, since it has two directories to show.

Each pane carries its own view switch in its header — a dense detail list, a
grid of large icons, or the directory tree. Those are three ways of drawing
*this* pane's own directory, which is why they are the only three there.

Two other things can be put on a pane, and neither is a way of drawing its own
directory: **quick view** (`F3`) and **folder history** (`Alt-H`). Both are
about the folder you are standing in, so both are drawn in the pane you are
*not* — the one you asked from keeps the cursor. A pane showing either says
whose folder it is showing in its header and drops the list/grid/tree switch,
which would otherwise be three buttons offering to redraw a directory that
pane is not showing. They are on the toolbar's view menu as well as on the
keys, and a pane opened only to answer folds away again when it stops.

Folder history is the account read from where you are standing rather than by
day: what was copied in, moved out, renamed, deleted or created here, newest
first, with failures kept — because "why is this file not here" is answered by
the attempt that failed as often as by the one that worked. `Ctrl-J` still
opens the whole journal, by day.

Under the panes is the row of function keys, as in every commander since
Norton. It is read out of the same table the keyboard uses, so it cannot come
to disagree with what the keys actually do — `F9` is *Select* in this
front-end, not *Sort*, and the bar says so. The toolbar can fold it away.

There is one more argument, and it is how the view gets checked without a
human at the screen:

```sh
cargo run --bin lostc-gui -- --screenshot shot.png ~/src ~/documents
```

It renders a few frames, writes a PNG and exits. The picture above was made
with it.

**Where a copy goes.** `F5` and `F6` ask, in a field, with the other pane's
directory already in it when there is one — one Enter, as in every commander
since Norton. With a single pane there is nothing to guess at, so the field
offers the current directory instead: a starting point to edit rather than an
Enter to lean on. Nothing is ever copied into a directory that is not on
screen.

## What each front-end does

Both drive the same engine, so listing, sorting, marking, copy/move/delete
with progress and cancel, archives, folder compare and sync, the duplicate
finder, find-by-name-and-contents, permissions, tabs, bookmarks, network
locations, the directory tree, the trash and the journal work the same in
either.

The terminal one is not a cut-down version of the graphical one - it came
first, and it is the portable one. What it does not do is mostly what a
terminal cannot, plus one thing not yet built:

| | `lostc` | `lostc-gui` | why |
|---|---|---|---|
| File-type icons, grid of large icons | — | yes | there is no grid of icons in a terminal |
| Image viewing, and crop/rotate/resize | — | yes | same |
| Built-in text editor | — | yes | `lostc` hands the file to `$EDITOR`, which already knows your settings |
| Running commands | a command line, `Ctrl-O` for the output | a shell in a drawer, output in the window | `lostc` hands each command the real terminal; `lostc-gui` has no terminal to hand over, so it keeps one |
| A shell that stays put | yes | yes | one per session in the terminal view, one per tab in the drawer |
| The shell and the panel sharing a directory | yes, both ways | yes, both ways, and a tab can be pinned out of it | a `cd` either side moves the other; switching panes takes the shell along |
| Session recording (`rec`) | — | yes | it records that shell, so it needs one |
| Named colour schemes | one | several | the terminal view uses the classic blue/cyan palette, and a terminal has its own colours anyway |
| Function keys along the bottom | yes | yes | the graphical one had none, which left `F5` a secret |
| What was run here, beside the shell | yes | yes | `Alt-P`/`Alt-N` walk it in the terminal; the window shows the list |
| What was done in *this folder* | the journal, by day (`Ctrl-J`) | that, and a pane beside it (`Alt-H`) | a window has room to show the answer next to the question |
| Markdown rendered rather than shown as markup | — | yes | not built for the terminal yet; the parse is in the engine, so it is drawing that is missing, not logic |

Reading bytes as hex, the directory tree, tabs and the journal are in both.

**History** — in both front-ends — sits beside the shell: what was run,
newest first. It opens on `here`, meaning the commands from the directory the
shell is standing in, which is the half of a history worth having in front of
you; `all` is the rest of it, this directory first, with the ones from
somewhere else drawn dimmer and naming their folder when you hover. Clicking
a line puts it on the command line without running it.

In the window it is the bottom-right sector, under the places list; `hist`
turns it off, and hiding the places list takes the column with it, since that
column is what it is drawn in. In the terminal view it is beside the shell
screen, and `Alt-P` / `Alt-N` walk the same list.
A shell's own history is one list with no idea where you were standing, and
the half that is about here is the half worth having in front of you. Clicking
a line puts it on the command line without running it; the terminal view walks
the same list with `Alt-P` and `Alt-N`.

**On the shell.** `lostc` keeps one shell running underneath the panels for
the whole session, the way Midnight Commander does. Typing goes to the
command line and Enter hands the line to that shell; `Ctrl-O` swaps between
the panels and the shell's own screen, where you can work in it directly.

Because it is one shell rather than one per command, `cd` means something:
the directory it leaves you in is where the next command runs. It is shared
both ways — a `cd` in the shell moves the panel when you come back to it, and
moving the panel `cd`s the shell. The shell reports where it is through the
same `OSC 133` hook the graphical view uses, so this is reading an answer
rather than guessing at one.

A shell with no seam to hook — `cmd`, `dash` — cannot be asked, so the panel
becomes the answer instead: each command is preceded by a `cd` to the
directory the panel is showing, which is what Far Manager does on Windows and
for the same reason. Half-sharing a directory is worse than not sharing one.
`Ctrl-O` opens onto a line saying which of the two you have, and `Alt-O`
changes it.

## Testing

```sh
cargo test                     # all 1094, from the workspace root
```

From the root that is everything, because the root is a virtual manifest and
cargo defaults to every member of one. From *inside* a crate directory it is
that crate alone, which is a fast loop but easy to mistake for a full run —
say `cargo test --workspace` there if you meant all of it.

Per crate, when you want a fast loop:

```sh
cargo test -p lost-commander-core    # 649 - the engine, seconds to build
cargo test -p lost-commander-egui    # 217 - the graphical view
cargo test -p lost-commander-tui     # 122 - the terminal view
cargo test -p lost-commander-ffi     # 106 - the C ABI
```

The engine's tests are worth singling out: they cover the whole of the file
manager's behaviour with no user interface attached, and they build in a
fraction of the time the graphical crate does, because `core` depends on no
windowing library and the crate boundary is what guarantees it.

Some tests want real programs and skip rather than fail without them — zsh,
fish and dash for the shell-integration tests, the system's own `zip` and
`tar` for some archive fixtures. A machine with more of those installed runs
more tests.

### Driving the real thing

The tests do not substitute for running it, and most real bugs here were found
by running it. `harness/` holds two working harnesses with their own README:
`gui_archives.sh` drives the graphical binary under Xvfb with xdotool and
scrot, and `tui_journal.py` drives the terminal binary through a pty with
pyte.

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

Six things are kept, and each is its own file, because they change at wildly
different rates and mean different things:

| | | |
|---|---|---|
| `settings.toml` | how you like it | when you change something |
| `bookmarks.toml` | places you saved | when you save or remove one |
| `recent.toml` | places you have been | every time a pane moves |
| `workspaces.toml` | the windows you had open | when you ask it to |
| `journal/shell-*.jsonl` | what you ran | as each command is run |
| `journal/files-*.jsonl` | what was done to files | as each thing happens |

The two histories are appended a line at a time, so nothing is lost to a
crash. Recent locations were kept inside `bookmarks.toml` until they got a
file of their own: walking into a directory rewrote the file holding the
things you had deliberately saved, which is a lot of writing to risk somebody's
bookmarks on. An old `bookmarks.toml` with recents inside it still gives them
up, so nothing disappears on the upgrade.

## License

Mozilla Public License 2.0 — see [LICENSE](LICENSE).

Copyleft, per file. Changes to *these* files must be published under the same
licence, so improvements to the engine come back rather than disappearing into
a fork. Linking this code from a larger work of your own is explicitly allowed
and puts no licence on that work (§3.3) — which is the reason for choosing MPL
over the GPL family, since this engine is designed to be driven through a C
ABI by front-ends it knows nothing about.

### Contribution

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work shall be licensed as above, without any additional
terms or conditions.
