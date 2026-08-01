# lost-commander

A dual-pane file manager in Rust, in the Norton Commander tradition, with two
front-ends over one engine. It runs unchanged on **Linux, macOS and Windows**.

- **`rcmd`** — a terminal UI. Two panes, function keys, no mouse required.
- **`rcmd-gui`** — a graphical view laid out for a pointer rather than a
  teletype: a sidebar, a breadcrumb trail, per-pane list/grid/tree views, and
  a real shell in a drawer along the bottom.

Everything underneath — listing, sorting, marking, copy/move/delete with
progress and cancel, archives, bookmarks, network locations, the directory
tree, the account of what was done — lives in one library that neither front-
end owns.

## The terminal view

```
┌───────── /home/you/lost-commander/core ──────────┐┌─────────── /home/you/lost-commander ─────────────┐
│Name                           Size Modified      ││Name                           Size Modified      │
│ ..                            <UP>               ││ ..                            <UP>               │
│ src                          <DIR> 25.07.26 04:43││ core                         <DIR> 25.07.26 04:43│
│*entry.rs                     5.52K 25.07.26 04:36││ egui                         <DIR> 25.07.26 04:42│
│*fsops.rs                     9.07K 25.07.26 04:38││ ffi                          <DIR> 25.07.26 04:42│
│ panel.rs                     5.66K 25.07.26 04:42││ Cargo.toml                     730 25.07.26 04:36│
└──────────────────────────────────────────────────┘└──────────────────────────────────────────────────┘
panel.rs  5.66K  25.07.26 04:42  [2 marked, 14.6K]  (sort: name)
1Help   2Rename 3View   4Edit   5Copy   6Move   7MkDir  8Delete 9Sort   10Quit
```

## The graphical view

![rcmd-gui](docs/rcmd-gui.png)

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
cargo build --release -p lost-commander-tui    # rcmd alone
cargo build --release -p lost-commander-egui   # rcmd-gui alone
```

## Running the terminal front-end

```sh
cargo run --bin rcmd                      # the current directory in both panes
cargo run --bin rcmd -- ~/src ~/documents # explicit left and right
cargo run --bin rcmd -- --list ~/src      # print a listing and exit
cargo run --bin rcmd -- --help
```

```
Tab   switch panel     Enter  open           Backspace  parent
F1    help             F2     rename         F3         view
F4    edit             F5     copy           F6         move
F7    mkdir            F8     delete         F9         sort
F10   quit             Space  mark           Ctrl-H     hidden files
```

`--list` is the whole listing pipeline with no terminal attached, which makes
it useful for a quick check that a build works, and for scripting.

## Running the graphical front-end

```sh
cargo run --bin rcmd-gui                        # current directory
cargo run --bin rcmd-gui -- ~/src ~/documents   # explicit left and right
cargo run --bin rcmd-gui -- --grid              # start both panes in the icon grid
cargo run --bin rcmd-gui -- --tree              # tree above the files, XTree's arrangement
cargo run --bin rcmd-gui -- --preview           # right pane follows the left, as F3 does
cargo run --bin rcmd-gui -- --help
```

Each pane carries its own view switch in its header — a dense detail list, a
grid of large icons, the directory tree, or a preview of whatever the *other*
pane is pointing at. The function keys do what they do in the terminal view.

There is one more argument, and it is how the view gets checked without a
human at the screen:

```sh
cargo run --bin rcmd-gui -- --screenshot shot.png ~/src ~/documents
```

It renders a few frames, writes a PNG and exits. The picture above was made
with it.

## What each front-end does

Both drive the same engine, so listing, sorting, marking, copy/move/delete
with progress and cancel, archives, folder compare and sync, the duplicate
finder, find-by-name-and-contents, permissions, tabs, bookmarks, network
locations, the directory tree, the trash and the journal work the same in
either.

The terminal one is not a cut-down version of the graphical one - it came
first, and it is the portable one. What it does not do is mostly what a
terminal cannot, plus one thing not yet built:

| | `rcmd` | `rcmd-gui` | why |
|---|---|---|---|
| File-type icons, grid of large icons | — | yes | there is no grid of icons in a terminal |
| Image viewing, and crop/rotate/resize | — | yes | same |
| Built-in text editor | — | yes | `rcmd` hands the file to `$EDITOR`, which already knows your settings |
| A shell, with its commands recorded | — | yes | not built for the terminal yet — see below |
| Session recording (`rec`) | — | yes | it records that shell, so it needs one |
| Named colour schemes | one | several | the terminal view uses the classic blue/cyan palette, and a terminal has its own colours anyway |
| Markdown rendered rather than shown as markup | — | yes | not built for the terminal yet; the parse is in the engine, so it is drawing that is missing, not logic |

Reading bytes as hex, the directory tree, tabs and the journal are in both.

**On the shell**, one honest note. It is tempting to say a terminal file
manager needs no shell of its own because it is already running in one. That
is not what Norton Commander or Midnight Commander do: they put a command
line along the bottom of the panels, keep a real shell running underneath,
and toggle to its screen with `Ctrl-O` - with the current directory shared
both ways, so `cd` on the command line moves the panel and moving the panel
`cd`s the shell. That is a different integration from the graphical view's
drawer, and a better one for a terminal. `rcmd` has neither, and that is a
gap rather than a decision.

## Testing

```sh
cargo test                     # all 986, from the workspace root
```

From the root that is everything, because the root is a virtual manifest and
cargo defaults to every member of one. From *inside* a crate directory it is
that crate alone, which is a fast loop but easy to mistake for a full run —
say `cargo test --workspace` there if you meant all of it.

Per crate, when you want a fast loop:

```sh
cargo test -p lost-commander-core    # 607 - the engine, seconds to build
cargo test -p lost-commander-egui    # 181 - the graphical view
cargo test -p lost-commander-tui     #  92 - the terminal view
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
tui/     rcmd      — ratatui + crossterm
egui/    rcmd-gui  — eframe/egui
ffi/     a C ABI, for front-ends that are not written in Rust
```

`ffi/` exists because a front-end need not be a Rust one. Values cross as
JSON, the caller polls and nothing calls back, no panic unwinds into C, and
every string and handle has one owner and one way back. A native Windows
front-end written in C# is built on it.

## Where things are kept

Settings, saved locations and the journal live in one directory per platform:

| | |
|---|---|
| Linux | `~/.config/lost-commander/` |
| macOS | `~/Library/Application Support/lost-commander/` |
| Windows | `%APPDATA%\lost-commander\` |

## License

Licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. This is the usual arrangement for Rust projects, and the
choice is the user's rather than mine: MIT is the shorter one most people
already know, and Apache-2.0 carries an explicit patent grant that some
organisations' policies look for.

### Contribution

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.

That sentence is the point of offering both. Under Apache-2.0 a contributor
licenses the patent claims their contribution reads on — including any their
employer holds — to everyone downstream. MIT says nothing about patents, so
without this a contribution could be accepted in good faith and still leave a
problem for the people using it.
