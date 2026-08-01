# rust-commander

A dual-pane file manager in Rust, in two front-ends over one engine, built to
run unchanged on **Linux, macOS and Windows**:

* **`rcmd`** - a Norton Commander-style terminal UI.
* **`rcmd-gui`** - a graphical view designed for a pointer, not a teletype.

Everything underneath - listing, sorting, marking, copy/move/delete with
progress, bookmarks, network locations, the directory tree - lives in a shared
library (`src/lib.rs`) that neither front-end owns.

```
┌───────── /home/user/rust-commander/src ──────────┐┌─────────── /home/user/rust-commander ────────────┐
│Name                           Size Modified      ││Name                           Size Modified      │
│ ..                            <UP>               ││ ..                            <UP>               │
│ app.rs                       28.8K 25.07.26 04:42││ src                          <DIR> 25.07.26 04:43│
│*entry.rs                     5.52K 25.07.26 04:36││ target                       <DIR> 25.07.26 04:42│
│*fsops.rs                     9.07K 25.07.26 04:38││ Cargo.lock                   45.4K 25.07.26 04:42│
│ main.rs                      5.66K 25.07.26 04:42││ Cargo.toml                     730 25.07.26 04:36│
└──────────────────────────────────────────────────┘└──────────────────────────────────────────────────┘
main.rs  5.66K  25.07.26 04:42  [2 marked, 14.6K]  (sort: name)
1Help   2Rename 3View   4Edit   5Copy   6Move   7MkDir  8Delete 9Sort   10Quit
```

## Build and run

```sh
cargo run --bin rcmd                 # terminal UI, current directory
cargo run --bin rcmd -- ~/src ~/doc  # explicit left and right directories
cargo run --bin rcmd-gui             # graphical UI
cargo run --bin rcmd-gui -- --grid   # ...starting in the icon grid

cargo build --release                # binaries in target/release/
cargo test                           # 744 unit tests
```

The graphical front-end is behind a default-on `gui` feature. Building
`--no-default-features` gives you the terminal binary with none of the
windowing or GPU dependencies:

```sh
cargo build --no-default-features --bin rcmd
```

## The graphical view

Not the terminal view with nicer characters - a different layout for a
different input device:

* **A sidebar** carrying Places and Recent.
* **A breadcrumb trail** - jumping three levels up is one click, not three
  presses of Backspace.
* **Four views, chosen per pane** from a switch in that pane's own header: a
  dense detail list, a grid of large icons for when you are looking at photos
  rather than reading names, the directory tree, or a quick view of whatever
  the other pane is pointing at.

  The tree is not a separate place in the window - it is the pane, showing the
  same directory from further out. It opens already expanded down to where the
  pane is, with that row highlighted and scrolled into view, so it answers
  "where am I" at a glance. Clicking a row moves the pane there and the tree
  stays, which makes jumping across the filesystem one click instead of a walk
  up and back down. The disclosure triangle expands without moving anywhere.
  One pane can be a tree while the other stays a listing.
* **Vector file-type icons**, not glyphs and not an image atlas. They stay
  crisp at any size and DPI, ship no assets, and cannot degrade into tofu
  boxes when a font is missing - which is exactly what happened to the first
  draft of the toolbar when it used `←`, `↻` and `▦` as characters.
* **Pointer selection**: click to focus, ctrl/cmd-click to toggle one,
  **shift-click to take everything from the last click to this one**, and
  double-click to open. Ctrl-A marks the lot. Marked entries are a filled
  amber highlight, not an outline - a box drawn round every marked row reads
  as a grid rather than as a selection. The cursor stays blue, so the two
  never argue: amber is what you have chosen, blue is where you are, and a row
  that is both is a brighter amber. A mark keeps its colour when its pane
  loses focus, because a mark is a fact about the file rather than about the
  window.
* **Bulk selection** from the toolbar's select menu: all, none, invert, or by
  pattern - `*.jpg` in the box and Select, or Deselect to take a subset back
  out. That is the Commander grey-plus gesture, and it is how you mark two
  hundred files without two hundred clicks.
* **A second pane that folds away**, from the toolbar, giving the pane you are
  on the whole width. The hidden one is only hidden: its directory, cursor and
  marks are all still there when it comes back.
* **Copy, move and delete**, each acting on the whole selection - see below.
* **A command line** under the panes, as the original had - see below.
* **A status strip** that becomes a live progress bar with a Cancel button
  while a copy runs.

Every part of the layout is fluid: the panes take whatever the window, sidebar
and console leave them, and the console can be dragged taller or collapsed to
just its prompt.

Both seams are draggable. The one **between the panes** sets how the width is
shared - double-click it to go back to even - and the one **above the shell
panel** sets how tall that panel is. Neither pane can be dragged away to
nothing, since a pane with no width looks like a bug and cannot be grabbed
back; folding one away entirely is the toolbar toggle's job. The grab target
is wider than the line it draws, because a three-pixel target is a target you
miss.

A pane dragged narrow drops its columns from the right rather than painting
over them: the permissions go first, then the date, then the size, and below
that the name has the row to itself and is elided if it still does not fit.

### Opening a file

`Enter` on a directory goes into it. `Enter` on a **file hands it to whatever
application the desktop has registered for it** - the same thing a double-click
does in Explorer or Finder, because it is the same call underneath. A
double-click here does it too, and `F3` is still the built-in quick view for
when reading it in place was what you wanted.

It is one call per platform, and none of them is quite the obvious one:

| | |
| --- | --- |
| **Linux** | `xdg-open`, falling back through `gio open`, `kde-open5`, `gnome-open`, `exo-open` |
| **macOS** | `open` |
| **Windows** | `rundll32 shell32.dll,ShellExec_RunDLL` |

The Linux fallback exists because `xdg-open` is a shell script from
`xdg-utils` and a slim container or a minimal desktop may simply not have it -
the container this was developed in has only `gio`. The first opener actually
installed wins, and with none of them the error names the package to install
rather than failing silently.

Windows is the interesting one. The obvious `cmd /C start "" <path>` is wrong
twice over: `start` reads a leading quoted token as the *window title*, which
is what that empty pair is working around, and `cmd` re-parses its command
line, so `&`, `^`, `|` or `%` in a file name breaks out of it. Rust quotes
arguments by the C runtime's rules, which `cmd` does not use, so there is no
quoting that makes an arbitrary path safe through it. `rundll32` reaches
`ShellExecuteEx` - what Explorer itself calls - with the path arriving through
argv untouched.

**Opening is not executing**, and the two get confused under one word. Nothing
here ever runs the selected file; it runs the platform's opener and hands it a
path, so which application starts stays the desktop's decision. Where that
delegation would *amount* to execution - a `.exe` or `.bat` on Windows, a
`.desktop` anywhere, a script or a bare binary carrying the execute bit - it
asks first. Not a refusal, since a file manager whose Enter key cannot start a
program is missing something the original had; one keystroke of distance
between the cursor landing on `setup.exe` and `setup.exe` running.

The execute bit alone is deliberately not enough. Everything on a FAT stick or
an SMB share reports as `0777`, holiday photos included, and a confirmation
box on every one of them is a box people learn to click through. The bit
counts when the name agrees - `.sh`, `.py`, `.appimage` - or says nothing at
all, which is what a compiled binary looks like.

Like every other operation here it is plural: `Enter` on five marked photos
opens five photos. Past five it asks, because opening is one window per file
and a stray `Ctrl-A` is how you get two hundred of them. A mixed set says what
will actually happen to it - *"Open these 8? build.sh is a program and will be
run."* - since a question that describes only part of what follows is worse
than no question, because it gets answered anyway.

Three details that are each a bug if they go the other way: stdio is closed, or
a handler that inherited the terminal would draw over the panels while the TUI
is in raw mode; the wait happens on a thread, since `xdg-open` can live as
long as the application it started and nothing may block a frame; and only the
failure to *start* is reported, because whether the application then liked the
file is between it and the user.

### Open with...

`Shift-Enter` asks *which* application instead of taking the default. The
question is about one file - the answer for a photo is rarely the answer for
the spreadsheet marked next to it - so this one is deliberately singular where
the rest are plural.

**On Windows the chooser is the system's own.** One call -
`rundll32 shell32.dll,OpenAs_RunDLL` - reaches the dialog that has been there
since the nineties, which offers "always use this", knows about the Store, and
is already the one the user recognises. A list of names reimplemented beside it
would be strictly worse.

Elsewhere the list is built from what the system publishes:

* **Linux** reads the `.desktop` files in `$XDG_DATA_DIRS/applications` and
  `~/.local/share/applications`. Applications claiming the file's type come
  first and are marked; everything else follows under *Other applications*,
  because the case a chooser exists for is the file whose type nothing claims -
  or claims wrongly. `NoDisplay` and `Hidden` entries are skipped, and so is
  any whose `TryExec` binary was never installed - the same split-package case
  the thumbnailers have.
* **macOS** offers the `.app` bundles from `/Applications` and friends, started
  with `open -a`. Which application *handles* a type lives in LaunchServices,
  which is a C API rather than anything on disk; the default is already one
  keystroke away on `Enter`, and this list is for the other one.

The `Exec` line is not a shell command and is not parsed as one. It has its own
quoting from the desktop entry spec, and its own field codes: `%f` `%F` `%u`
`%U` become the file, and `%i` `%c` `%k` and the deprecated ones a launcher is
required to drop. An entry with no field code at all still gets the file, since
plenty omit it and starting the application on nothing looks like a failure.

**One box does two jobs.** Type to narrow the list; when nothing matches, what
you typed *is* the command - `hexdump -C`, `wc -l`, anything. That is the case
a list built from installed applications cannot otherwise reach, and it is
discoverable by typing rather than being a mode you have to know about.

An application marked `Terminal=true` gets a **shell tab** rather than being
started with its output thrown away - the route `F4` already takes to
`$EDITOR`, and for the same reason: there are real terminals in this window, so
a terminal application has somewhere to go.

The terminal front-end has the same chooser on **`Ctrl-P`**. It cannot use
`Shift-Enter`: most terminals send a plain `Enter` for it, so the two would be
indistinguishable there.

### As administrator

Every platform's answer is a **request, never a grant**: this asks the system
to authorise and the system asks the user. Nothing here gains privilege
quietly, and nothing here needs the file manager itself to be running as root.

| | |
| --- | --- |
| **Windows** | `Start-Process -Verb RunAs`, which raises UAC |
| **macOS** | `do shell script ... with administrator privileges`, which raises the authentication panel |
| **Linux** | `pkexec` (then `kdesu`, `lxqt-sudo`), which raises PolicyKit's prompt - and `sudo` in a shell tab when none of them is installed |

The Windows script is handed over **base64-encoded UTF-16** rather than as
text. PowerShell re-joins and re-parses what it is given for `-Command`, on top
of the C runtime quoting Rust has already applied, and a file name with a quote
or a `$` in it does not survive both passes. `-EncodedCommand` leaves no
parsing to go wrong. On Linux `pkexec` deliberately starts its child with a
minimal environment, so a graphical program loses the display it was about to
draw on; `DISPLAY` and `XAUTHORITY` are passed back through `env`, which is
what the desktops' own launchers do.

**"Open as administrator" is usually the wrong tool**, and the reason is worth
stating rather than hiding:

* A root GUI application writes root-owned files into your own configuration
  directory, which then breaks that application for you afterwards. `sudo
  gedit` once is a classic way to have to `chown` your home directory back.
* On Wayland a root process generally cannot reach the display at all, and on
  macOS a `.app` bundle run as root is unsupported rather than merely
  discouraged.
* The whole program gets root - every plugin it loads, every URL it opens -
  when the need was to write one file.

So the two routes that are *right* are the ones with keys on them, and the
blunt one is a checkbox in the chooser with its edges named:

| | |
| --- | --- |
| `Shift-F4` | edit a file you do not own |
| `Ctrl-E` | a shell as administrator, here |
| `Ctrl-F` / `Alt-F7` | find files by name or contents |
| `Alt-Enter` | properties and permissions |
| the chooser's **as administrator** | open with a chosen application, elevated |

`Shift-F4` is **`sudoedit`**, not "run the editor as root". It copies the file
to a temporary one, runs *your* editor as *you*, and installs the result back
as root when you are done - so only the write is privileged, no editor plugin
runs as root, nothing root-owned lands in your home directory, and a graphical
editor still talks to your own display because it is still your process. Almost
nobody knows it exists; it is the correct tool for the commonest case.

`Ctrl-E` is what most "as administrator" clicks were actually reaching for:
somewhere privileged to work, rather than one privileged application.

A prompt needs a terminal to appear on, so anything that will ask for a
password goes to a **shell tab** rather than being spawned blind - and if the
tab is busy with a full-screen program, it gets a tab of its own instead. A
command typed at a `vim` is not run; it reaches `vim` as keystrokes, which is
how `sudo -i` ends up as a search in someone's editor. The alternate screen is
the one honest signal a pty gives for this, and it is what gets checked.

The terminal front-end has the same three: `Ctrl-P` then `Ctrl-A` for the
chooser's toggle, `Shift-F4`, and `Ctrl-E`. Its shell lines run with the TUI
suspended, the same way `F4` hands over to `$EDITOR`.

### Permissions, and what a file is

The detail list shows permissions as `ls` writes them - `-rwxr-xr-x`,
`drwxr-xr-x`, `lrwxrwxrwx` - **when the pane is wide enough**. They are the
first column to go as a pane narrows and the last to arrive as it widens,
because they are the least often what you came to a listing for; the order
after that is unchanged, so a pane that showed the date before still does.
The text is monospaced, since the point of `rwxr-xr-x` is that the same
permission is in the same place on every row, which it is not when the
characters are different widths.

`Alt-Enter` opens **Properties**: type, size in both forms (`4.20K` is what you
read, `4301 bytes` is what you check), the three dates, owner and group, where
a symlink points, and a grid of nine checkboxes with the octal beside it. The
grid and the box are two views of one number - typing `640` moves the ticks,
ticking moves the box - and only the one being edited wins, so they cannot
fight. `setuid`, `setgid` and `sticky` are there too.

Some care about what the bits mean:

* The **file type is not part of the permissions.** `st_mode` carries both, and
  a checkbox that reached the type bits would turn a file into something else,
  so only the low twelve are ever touched.
* A **special bit replaces an execute character** rather than adding a tenth -
  `rwsr-xr-x`, and a capital `S` when the execute bit underneath is *not* set,
  which is how you spot that someone meant `4755` and typed `4644`.
* A **symlink reports itself**, not its target: `symlink_metadata`, so the
  size and the mode are the link's own. The dialog is about the file you
  selected.
* **Only what changed is written.** A dialog that wrote every field back on
  Apply would touch files nobody edited. Nothing at all reaches the disk until
  Apply - not opening the dialog, not ticking a box.

Owner and group names come from reading `/etc/passwd` and `/etc/group`
directly, which needs no C library and no dependency, at the price of only
knowing *local* accounts - a network directory answers through NSS, which this
does not go through. An id with no local entry is shown as the number, which is
what `ls -n` would have said anyway.

**Windows has none of this.** It has no permission bits; its real access
control is ACLs, which a checkbox grid cannot honestly represent. So there the
dialog offers the one flag that does exist and does mean something - read-only
- rather than nine boxes that would be a lie. The parts a platform does not
have are `None` rather than zero.

The terminal view has the same dialog on `Alt-Enter`, with the arrows moving
between the nine boxes, `Space` toggling, `u`/`g`/`t` for the special bits, and
`Enter` to apply.

### The listing keeps up on its own

A file manager whose panels are only right until something else touches the
disk is one you have to remember to press `Ctrl-R` in. Both panels notice
outside changes about a second after they happen - a build writing into the
directory you are looking at, a download finishing, `git checkout` in the shell
below.

The cheap way to do this is the directory's own modification time, one `stat`,
which moves whenever an entry is created, removed or renamed. It does **not**
move when a file already listed grows, so the sizes and the newest entry time
are folded in as well - and that is the case worth having, since watching a log
file's size climb is half of why you were looking.

Reading a directory is one syscall however many entries it has; asking each
entry for its size is one syscall **each**. So the per-entry detail is only
gathered below 2000 entries, and past that the directory's own timestamp has to
be enough. Past the limit the detail is dropped rather than half-kept, or the
answer would depend on the order the entries happened to come back in.

**A refresh keeps the cursor and the marks**, because `reload` has always kept
them by name: a listing that refreshed itself by throwing away where you were
would be worse than a stale one. In the live check a file appearing *above* the
cursor left it on the same file rather than on the same row.

Nothing is re-read while an operation is running. A copy re-reads both panels
when it finishes, and a listing changing under it would be the copy's own
writes reported back as news. A pane showing the tree is left alone too - the
tree is a view of a filesystem rather than of one directory, and has its own
refresh.

`Ctrl-R` is still there for the cases polling cannot see: a network share
whose timestamps do not move, or an impatient moment.

### Find

`Alt-F7`, the Commander binding, or `Ctrl-F` for anyone who has used anything
else since. It searches under the active panel by **name** - a glob, `*` and
`?` - and optionally by **what is inside the file**.

Results arrive as they are found rather than at the end. A search over a large
tree takes as long as it takes, and a list that fills while it runs is one you
can use before it finishes - which for "where did I put that" is usually after
the first few hits. `Stop` ends it early; closing the form ends it too, rather
than leaving a thread walking a disk for a list nobody is looking at.

The name is checked **before** the file is opened, which is what keeps a
content search over a big tree bearable: `*.rs` containing `sudoedit` opens the
Rust files and nothing else. Binaries are skipped rather than searched - a
match inside one is noise, and an excerpt of it is worse - and the test for
that is the NUL byte, the same one `grep` uses.

Files are read a line at a time rather than in fixed blocks. A block boundary
would drop a match that straddled it, and the line number with the matching
line are what make a content hit worth showing: `elevate.rs:24   sudoedit
copies the file out...`.

Four details that are each a bug the other way:

* **Symlinked directories are not followed.** A link pointing at an ancestor
  makes a recursive walk infinite, and the file is still reachable by its real
  path, so nothing is lost but a duplicate.
* **Hidden directories are not walked** unless asked for. Searching a
  repository without that rule returns ten times what it should, all of it
  out of `.git`.
* **An unreadable directory does not stop the search.** A search over a home
  directory meets several.
* **The list is capped** at 5000, and says when it stopped early. A pattern of
  `*` over a home directory is not a search, it is a listing.

`Enter` in the boxes runs the search; `Down` walks out of them into the results
and `Enter` there **goes to the file** - the panel moves to its directory with
the cursor on it. Not "opens it": a search is how you find *where* something
is, and landing next to it leaves the next thing you do open, rather than
committing you to the one thing a chooser would have guessed.

Which of the two `Enter` means is decided by this code rather than by which
widget happens to hold focus. A rule that reads off focus is a rule that
changes when focus drifts - and this form has two text boxes, which is exactly
the case where it does.

### What was done

`Ctrl-J` opens an account of everything this program has done to your files.
Every file manager tells you what it is *about* to do; almost none can tell you
what it **did** - and *"which of those four hundred files did the copy actually
skip?"*, asked an hour later, otherwise has no answer at all.

Each entry names both ends. A copy records where each file came from and where
it landed, a rename records the old name and the new, a permissions change
records `644 -> 755`, an edit records the encoding it was saved in or how many
bytes were patched. Anything that failed carries why, and is drawn in the
danger colour.

**A run is one entry, not four hundred.** Copying a selection, synchronizing
two trees, deleting a directory - each opens a *heading* first and files its
individual records under it. Fold it open to see the files; leave it folded and
it is a line saying `Copy 42 item(s) to /backup`. The heading goes down
**before** the work starts, so a run killed halfway still has one over whatever
it managed.

Browse by day - the arrows walk the dates - and filter by kind, by whether it
failed, or by part of a path. `delete` and `trash` are separate kinds on
purpose: one is recoverable and the other is not, which is the distinction
anybody looking back is drawing.

#### The shape on disk

One file per day, one JSON record per line, appended and never rewritten:

```text
~/.config/rust-commander/journal/files-2026-07-28.jsonl
~/.config/rust-commander/journal/shell-2026-07-28.jsonl
```

That falls out of what it has to do. Browsing by date is opening one file.
Keeping thirty days is deleting the files older than thirty days - no
compaction, no rewriting, no chance of losing good records while pruning old
ones. Appending is the only write, so a program killed mid-copy keeps
everything it had already recorded, and a half-written last line costs that line
and nothing else.

Two rules hold it together. **Recording never fails an operation**: a read-only
home or a full disk must not stop a copy, so every write swallows its errors on
purpose. And **a record is what happened**, written after the fact, carrying the
failure when there was one, never rewritten.

#### Searching by anything on the line

The box searches **everything the row shows**, not the path. That is the rule
that makes it predictable - what is on screen is what can be found - and it is
the only rule that answers the questions actually asked of an account:

| Typed | Finds |
|---|---|
| `--release` | the command that had that flag, on a path containing no such thing |
| `zsh` | everything run in that shell rather than the other one |
| `exit 1` | what failed, by how it failed |
| `gimp` | what was opened with it |
| `Windows-1251` | the file that was converted |
| `opened` | by the word in the kind column |

A run found by its **heading** keeps all of its files: searching `backup` and
finding "Copy 42 item(s) to /backup" should show the forty-two, not hide them
because their own paths say nothing about a backup. A search that matches one
file still narrows to that file.

In the terminal front-end, `/` opens the box and Escape clears it.

Retention is set in the window itself - 7, 30, 90 days or for ever - and
**Clear** throws the lot away. Zero means for ever rather than "none", because a
retention setting whose lowest value silently deleted everything would be a
trap; turning it off entirely is `journal = false` in the settings file. The
sweep runs once at startup, which is the only moment the program is certainly
not in the middle of writing to it.

#### All, Files, Commands

Three tabs, and **All** is the one it opens on:

```text
20:39:05  Created  /tmp/mwork/afterwards
20:39:00  Command  /tmp/mwork/from   ls no-such-thing        exit 2
▫ Copy 4 item(s) to /tmp/mwork/to    20:38:55  4 file(s)
20:38:51  Command  /tmp/mwork/from   touch built.o
```

Which is the point of keeping any of it: the copy and the `make` that consumed
it are one afternoon's work, and "what was I doing at four o'clock" is not a
question about one kind of thing. A run stays folded under its heading in the
mixed view exactly as it does on its own.

Underneath, they are still **two files** - `files-2026-07-28.jsonl` and
`shell-2026-07-28.jsonl` - because commands arrive in a different order of
magnitude: a build can run twenty a minute, and interleaved on disk they would
bury the file operations they were meant to sit beside. Reading is under no
such obligation, and puts them back together in the order they happened. The
**Files** and **Commands** tabs narrow to one file each when a build is drowning
everything else out; the day being looked at survives the switch, so comparing
what was done against what was run on the same afternoon is one click rather
than setting the date again each time.

The filter row offers only the kinds the tab can actually hold - all ten under
**All**, `copy` through `edit` under **Files**, `command` and `shell` under
**Commands**. In the terminal front-end, `Tab` cycles the three.

This covers both places a command can come from: the one-shot command line,
where the program runs it and can simply watch, and the **terminal panel**,
which is a real pty running your own shell and where nothing you type passes
through this program at all. The second one is the interesting case.

#### Asking the shell rather than reading the screen

The obvious way to find out what an interactive shell ran is to read what it
printed. It does not work, and it does not fail cleanly. Here is a real
transcript, with the commands as they actually ran beside it:

| On screen | What ran |
|---|---|
| `echo the` | `echo the beta` |
| `echo alphaha-name.txt` | `echo alpha` |
| `root@vm:/tmp# rm -rf /` | nothing - a `printf` printed it |

The first is a line edited before Enter; the second is a recall with `Ctrl-R`.
Both are what the line editor *painted*, which is not the line. The third is
program output shaped exactly like a prompt, and any parser anchored on
"prompt-shaped prefix" files it as a command that was never run. Add that the
exit status is nowhere in the byte stream at all, and reading the screen gives
you three plausible, wrong entries out of eight - which for a record of what
was done is worse than no record.

So the shell is asked instead. The shell this program starts is given a small
piece of its own startup that prints an escape sequence when a command begins
and another when it ends, carrying the line and the exit status. The terminal
never shows them; an emulator drops an operating-system command it does not
recognise. Nothing else about the session changes - your own `.bashrc`,
`.zshrc` and `config.fish` are sourced first and in full, and the hook is added
on top rather than instead.

Where a shell already emits the same `OSC 133` marks - iTerm2's or VS Code's
integration - the two are told apart by a token made fresh for each session, so
someone else's marks are ignored rather than recorded as commands nobody ran.

#### Which shells, and what happens to the rest

**bash, zsh and fish** report what they run. zsh and fish hand over the line
directly (`preexec`, `fish_preexec`); bash cannot - `$BASH_COMMAND` holds one
simple command, so a pipeline would arrive as its first component and a
subshell would not arrive at all - so its line is read back out of history from
`PS0`, which is expanded once per command line whatever shape it is.

**`sh`, `dash`, `ksh`, `nu` and `cmd.exe` cannot.** POSIX `sh` has no preexec,
no `PROMPT_COMMAND` and no `DEBUG` trap; there is no seam. They are still
offered, because which shell to use is your decision and not a logging
feature's - the picker marks them `· not recorded` rather than hiding them, and
`journaled_shells_only = true` narrows the list for anyone who wants that.
Narrowing never empties it: a machine with only `sh` still gets a terminal.

What matters more is that **the account says so**. Opening a shell that cannot
be recorded writes one line saying it:

```text
20:28:13  dash   /tmp/work   no way to report what it runs - commands in this
                             session are not recorded
20:28:08  bash   /tmp/work   touch in-bash.txt                          1ms
```

Without that, a day's work in `dash` leaves an empty tab, and an empty tab
reads as "nothing was run" - the one thing a record must never say when
something was. It is also why restricting the picker is not the answer on its
own: `ssh`, `docker exec` and typing `sh` all reach an unhooked shell anyway,
so the honest move is to state the hole rather than pretend to have closed it.

#### One thing it deliberately gets wrong

`HISTCONTROL=ignoredups:ignorespace` is the default on most distributions, and
it means bash does not always add the line to history - so reading history
hands back the line *before*. Two different situations look identical from
there: the same line run again, and a line kept out of history on purpose. Only
the first matches the command actually about to execute, which is what settles
it. Where nothing settles it - a repeated subshell, since bash runs no `DEBUG`
trap for one - nothing is recorded. It will miss a line before it will invent
one.

#### Opening a file is recorded too

Handing a file to another program is the last this program sees of it. Whatever
happens next - a change, a save, nothing at all - happens outside, so an open is
the most it can honestly say about a modification made elsewhere:

```text
21:19:57  Opened  /tmp/swork/from/notes.txt   gio - whichever application it chose
21:20:14  Opened  /photos/holiday.raw        GIMP (gimp)
```

The two lines say different amounts on purpose. **Open with...** knows exactly
which application it started, and names it. A plain `Enter` hands the file to
the desktop, and the desktop's association is not something this program can
read - so it names the opener it invoked and says plainly that the choice of
application was not its own. Claiming to know would be worse than admitting not
to.

It is a record of the handover, not of a change: nothing here can tell whether
the file was actually touched.

#### Which shell, and how long

The kind column carries the **shell's name** rather than the word "Command",
which on a list of commands says nothing:

```text
20:54:00  ▫ Copy 6 item(s) to /tmp/twork/to   6 file(s)   187ms
20:53:56  bash   /tmp/twork/from   sleep 2.4                2.4s
20:52:38  bash   /tmp/mwork/from   ls no-such-thing  exit 2   2ms
20:28:13  dash   /tmp/dwork/left   no way to report what it runs
```

Which shell ran something is exactly the thing that cannot be reconstructed
later, and "why did that behave oddly" has "that was the other shell" as an
answer more often than it should.

**Durations are kept where they mean something.** A command has one, because
waiting for one is most of what using a shell is. A single file operation does
not: sixteen bytes copied in under a millisecond, recorded four hundred times,
is not a fact about anything. What is wanted there is the **total for the run**,
and that is on the heading.

The total is a separate record appended when the run ends, not a field on the
heading - the heading goes down *before* the work starts, so that a run killed
halfway still has one, and at that moment there is nothing to say about how long
it took. A run showing no total is one that never reached its end, which is
worth being able to see. Sub-millisecond reads as `<1ms` rather than `0ms`,
because that is the true claim.

#### Never the output

The line, where it ran, and how it ended. Never the output - a record of what
was run is a record, a copy of every build log is a full disk. When you *do*
want the output, `rec` is the tool for it, and starting or stopping a recording
is itself written down, so the account can point at the transcript.

### Archives are folders you can walk into

`Enter` on a `.zip`, `.tar.gz`, `.7z` or `.lzh` steps inside it. Reading only,
for now: listing, viewing and copying out, which is almost all of what an
archive is for in a file manager. Changing one is a different problem with
different hazards - a repack that silently drops the permissions and symlinks
its writer does not understand is data loss from a rename - and is not
attempted.

zip, tar, tar.gz, tar.xz, tar.bz2, 7z and lha, all with pure-Rust decoders.
That is deliberate: a file manager that will not build because a system
compression library is missing is worse than one that cannot read that format,
and decompression is not where the last few per cent of speed matters.

Adding a format is one module and one line in a table - `src/archive/lha.rs` is
seventy lines and touches nothing else.

Two things the formats themselves decide:

**The bytes are asked before the name.** A name is a claim; a signature is a
fact. A zip saved as `.jpg` still opens, and so does an archive with no
extension at all.

**Directories are worked out, not trusted.** A zip of `docs/a.txt` need not
contain any entry for `docs`, so believing the archive would leave the file
unreachable. One that *is* written down is not then shown twice.

#### Walking in, and copying out

`Enter` on an archive steps inside it. The pane shows the level you are on,
`..` walks back up and out at the top, and the entries carry the sizes, dates
and permissions the archive recorded:

```text
/tmp/awork/here/papers.zip
  ..
  docs                    <DIR>   28.07.26 22:56   drwxr-xr-x
  readme.txt                 11   28.07.26 22:56   -rw-r--r--
```

`F5` copies out into the other pane, as a normal run with a progress bar, a
cancel and an entry in the account:

```text
▫ Extract 3 item(s) from papers.zip to /tmp/awork/there   3 file(s)   4ms
    papers.zip/docs/notes.txt  →  /tmp/awork/there/notes.txt      12
```

Files land **relative to the level you are looking at**: standing in `docs` and
copying `notes.txt` out puts it in the other pane as `notes.txt`, not as
`docs/notes.txt`. Rebuilding the archive's whole path under the destination is
what nobody asked for. A directory taken from the top still keeps its shape.

It is recorded as a **copy**, not a kind of its own - an extraction is a copy
whose source happens to be inside a file, and "where did this come from" should
not need two filters to answer.

`Enter` on a file inside an archive extracts a copy to a temporary directory and
hands it to whatever the desktop uses for that type. The account says so in
those words, because the copy is where any edit will go and read-only mode
means nothing comes back:

```text
Opened  /tmp/rust-commander-4213/papers.zip/docs/notes.txt
        docs/notes.txt from /tmp/awork/here/papers.zip - a copy, changes to it
        do not go back
```

Everything that would change the archive - `F6`, `F8`, `F2`, `F7`, permissions,
the editors - says **"Archives are read-only here - F5 extracts a copy out
first"** rather than half-working. The list is by what the action does rather
than by which key it is on, so rebinding a key cannot quietly make one of them
reachable again.

#### Archives that want a password

A zip keeps its **names in the clear** however well its contents are locked. So
a password-protected archive still lists in full, and can be walked through,
sorted and searched without the password - only reading a file needs one:

```text
readme.txt      11   locked
docs/notes.txt  12   locked
```

Getting this wrong is easy and quiet. The obvious way to list a zip refuses
encrypted entries outright, so an archive of three files lists as one, and the
panel reports it as nearly empty - an answer that looks like an answer. The
listing therefore reads headers without touching contents, and every entry
survives whether it can be decrypted or not.

Both schemes a zip can use are supported: the old ZipCrypto, and the AES-256
that every current archiver writes by default. 7z too, including archives whose
*header* is encrypted - those cannot be listed at all until the password
arrives, which is a different situation reported the same way.

"Needs a password" and "that password is wrong" are separate answers, because
the caller has to be able to tell "I have not been asked yet" from "what you
gave me is not it" without reading a message string.

Passwords are held in memory for the session and **never written anywhere** -
the same rule the network locations follow, and for the same reason. They never
reach the account either: a record of what was done is not a place for secrets.

### Delete goes to the trash

`F8` and `Del` move to the system's trash, where it can be got back.
`Shift-F8` and `Shift-Del` delete for good - the split Explorer has used since
the recycle bin existed. Both still ask first, and the wording says which one
it is: "cannot be undone" used to be true of every delete here and is now only
true of one of them.

None of the three platforms means "move it to a folder called Trash", and
getting it wrong produces files the desktop shows but cannot restore:

* **Linux** follows the freedesktop.org trash specification - a `files/` and an
  `info/` directory, and a `.trashinfo` recording where the file came from and
  when it went. The record is written **first**, with `create_new`, which is
  what makes claiming a name atomic; the file is only moved once its record
  exists, and a move that fails takes the record back out with it. A file with
  no record is an orphan the desktop will not restore.
* **macOS** asks Finder, because Finder owns the "Put Back" information.
  Moving the file into `~/.Trash` by hand puts it in the right place with none
  of the metadata.
* **Windows** goes through `Microsoft.VisualBasic.FileIO.FileSystem` with
  `SendToRecycleBin`, which is the one route to the shell's own recycle call
  that needs no COM bindings - `Remove-Item` deletes outright, and there is no
  cmdlet that recycles.

Two files called `notes.txt` deleted from different directories both want the
same slot, so the second becomes `notes.2.txt` - the number goes *before* the
extension, which is what the desktops do and what keeps the file openable once
it is restored. The recorded path is percent-encoded, since the spec stores it
as a URI.

A file on another filesystem cannot be renamed into the home trash, so it goes
to a **trash on its own volume**, and its recorded path is relative to that
volume - the trash then still makes sense when the disk is mounted somewhere
else. Finding the volume means walking up until the device number changes,
which is what a mount point is.

**If trashing fails, nothing is deleted.** A trash that quietly falls back to
`rm` is worse than no trash at all, because it fails in exactly the case the
user was relying on it not to.

A whole directory goes to the trash as one move rather than file by file: the
point is to be able to put it back, and putting back half of it is not that.
A permanent delete still walks the tree, so the bar moves and a cancel stops
part way through.

### Nothing is overwritten without asking

Copying or moving onto a file that is already there **stops and asks**, showing
both sides - size and date, with the newer one marked, because that is what the
answer turns on nine times in ten. Skip, Skip all, Overwrite, Overwrite all,
Only newer, Cancel; the ones that cover the rest of the run are asked for once
and then stand.

**Only newer** is the one that is a rule rather than an answer: from there on
the file arriving overwrites the one already there when it is the newer of the
two, and is skipped when it is not - without asking again. That is a folder
copied over a backup, where what you mean is "bring it up to date", and it is
one answer instead of one per file. Two files stamped the same moment are not
newer than each other, to within the same two seconds the directory comparison
allows, so a copy run twice does not copy everything twice. A file whose date
cannot be read is never called newer: that is not evidence, and overwriting on
no evidence is how the wrong copy wins.

A run that left files alone says so - `Copied 2 item(s), left 2 alone` rather
than a total that counts the ones it did not write.

This is the fix it sounds like: `File::create` truncates and `fs::rename`
replaces, so before this an `F5` onto an existing name destroyed it silently.
The guard that did exist only compared the top-level source and destination, so
a collision nested inside a copied tree went straight through it.

Three cases are errors rather than questions, because no answer to them means
anything: a **directory** already occupying the name a file wants, a file
copied **onto itself** (`File::create` would truncate it and then copy back the
nothing that was left), and the reverse. Two **directories** of the same name
are a merge, not a collision - only the files that actually land on each other
are asked about, one at a time.

The worker sleeps on the condition variable while the question is up, so no
byte is written until it is answered - and `Escape` means Cancel rather than
"go away", since a dialog that closed without an answer would leave the copy
stopped for good. The terminal view asks the same question with
`(s)kip  s(k)ip all  (o)verwrite  overwrite (a)ll  only (n)ewer  (c)ancel`.

### Operations act on the selection

Copy, move and delete have always been plural. Each takes every marked entry,
or the row under the cursor when nothing is marked, and `..` is never a target
however it was marked. Directories go in whole: the copy walks the tree.

One operation covers the lot - the progress bar counts the total, and Cancel
stops the whole run and removes the half-written file it was on, rather than
leaving it behind. The work happens on a thread, so the window keeps drawing
however long the copy takes.

### Tabs

Each pane holds as many directories open as you like, one on show at a time.
`Ctrl-T` opens another where this one is, `Ctrl-W` closes it, and `Ctrl-Tab` or
`Ctrl-PgUp`/`Ctrl-PgDn` walks between them - the bindings every program with
tabs uses. The tree, which had `Ctrl-T`, moved to `Alt-T`.

A tab is a whole pane, not a saved path: its cursor, its marks, its sort order
and its hidden-file setting all belong to it. So diving six levels down to
check something and coming back finds the tab you were working in exactly as
you left it, two hundred files still marked.

`Shift-F6` **sends the tab to the other pane**, which is `F6` one level up - `F6`
moves a file across, `Shift-F6` moves the whole tab - and you go with it,
because the tab is what you were working in. It arrives whole, marks and all.
`Alt-W` keeps the tab on show and closes the rest of them.

A pane always shows something, so the last tab of a pane will not close and
cannot be given away; both say so rather than leaving an empty pane behind.
Switching to a tab re-reads it, which is cheaper than watching every open
directory and just as correct, since nothing that is not on show can be seen to
be stale.

The strip only appears once there are two: one tab is a pane, and a row saying
so would cost a line of listing to repeat what the header already says. The tab
on show is filled in the palette's tab colour - `Tabs` in the colour form, two
entries, resting and on show - and keeps that colour when its pane loses the
keyboard, because which directory a pane is showing should not become hard to
see just because you looked at the other one. In the graphical view, clicking a
tab shows it, middle-click closes it, right-click offers close / close the
others / move to the other pane, and a `+` at the end of the strip opens
another. Tabs that would have the same name take their parent with them, so two
`src` tabs read `alpha/src` and `beta/src`.

### Text, and what its bytes mean

Rust strings are UTF-8 and a file on disk is bytes. Most of the time those are
the same thing. The times they are not are the ones that ruin an afternoon: a
Windows-made `.txt` in CP1252 whose smart quotes come out as replacement
characters, a Cyrillic README in CP1251 that is unreadable, a UTF-16 file out
of a PowerShell redirect that looks like every other byte is a null.

[`encoding`](src/encoding.rs) handles seven: UTF-8 with and without a mark,
UTF-16 either way round, Windows-1252, Windows-1251, and Latin-1. Detection is
graded, and says which grade it reached:

| | what it means |
| --- | --- |
| **byte-order mark** | proof. Not a guess. |
| **certain** | valid UTF-8. Arbitrary bytes are overwhelmingly unlikely to be accidentally valid, so this is as near proof as sniffing gets. |
| **a guess** | one of the single-byte tables. Every one accepts every byte, so *nothing can rule any of them out* - and a guess presenting itself as an answer is worse than one that admits it. |

The subtlety in the UTF-8 test is that sniffing reads only the first 64 KB, and
for a file of Chinese or emoji that cut lands mid-character two times in three.
`from_utf8`'s error tells the cases apart: `error_len()` of `None` means the
input ran out mid-sequence, which is the window's doing and not the file's;
`Some` means a byte that cannot be there at all. Trimming bytes off the end
until it parses - the obvious way to allow for the cut - blurs the two, and
reads `caf\xE9\n` as a truncated UTF-8 `caf`.

`F4` opens the file for editing, with **two** encoding choosers rather than one:

* **Read as** re-reads the same bytes a different way. Free, reversible, and
  nothing is written - so it can be tried until the text stops being nonsense.
* **Save as** is what to put back. Changing it converts the file, and can lose
  characters the target has no room for.

One box cannot express *"this is CP1251 and I want it saved as UTF-8"*, which
is the main thing anyone opens such a window to do. What will not fit is named
**before** Save is pressed rather than reported after; saving with losses still
writes the file, because refusing would be worse, but it says so rather than
letting an afternoon's work quietly become question marks.

Line endings get the same care. Editing happens in `\n`, because every text
widget works that way, and whatever the file had is what it gets back: an
editor that silently converts CRLF to LF produces a diff in which every line
changed, which is a diff nobody can review.

The terminal view keeps a read-only viewer - `$EDITOR` is one keystroke away
there and already knows your settings - but it detects the encoding all the
same, and `e` / `E` walk through the others. Before this, `F3` on that Cyrillic
README was a screen of replacement characters with nothing to say why. In the
graphical view `Alt-E` is the route to `$EDITOR`.

### The files that are not text

`F3` on a compiled program used to pour it into the text viewer, where it came
out as a screenful of replacement characters - which looks like a bug rather
than like a binary. A file whose head is not text now opens as a **hex dump**
instead, in the layout `hexdump -C` uses because that is the one every reader
already knows:

```
00000000  7f 45 4c 46 02 01 01 00  00 00 00 00 00 00 00 00  |.ELF............|
00000010  02 00 3e 00 01 00 00 00  40 10 40 00 00 00 00 00  |..>.....@.@.....|
00000020  72 75 73 74 2d 63 6f 6d  6d 61 6e 64 65 72 00 00  |rust-commander..|
```

An offset, sixteen bytes in two groups of eight so the eye can count to eight
without counting, and the same bytes again as characters with everything
unprintable as a dot - including the bytes that make up a UTF-8 character,
because a dump is about bytes and half a character drawn in a fixed grid is a
lie about where the next one starts.

Nothing holds the file. A dump's rows are at fixed offsets - row *n* starts at
byte *n × 16* - so only the window on screen is ever read and a four-gigabyte
file opens as fast as a small one. The graphical view shows the same thing in
its quick view, for anything neither it nor the system can draw.

Two binaries handed to the file comparison get the one answer there is about
them, and it names an offset rather than shrugging: *"They first differ at byte
300 (0x12c) - F3 on either shows it."*

#### Changing them

`F4` on a binary opens the same dump for editing, in both views. One rule runs
through it and is not negotiable: **a hex editor overwrites**. It never inserts
and never deletes, because inserting one byte moves every byte after it, and
that turns a two-byte fix to a header into a rewrite of the whole file. The
length going out is the length that came in - and the write refuses to run past
the end even if the file shrank while the dump was open.

A byte is two keystrokes, one nibble at a time, because an editor that replaced
the whole byte on the first keystroke would make `4f` reachable only by typing
`04` and then `4f`. `Tab` swaps to the character column, where one keystroke is
one byte - which is how you patch a string without doing the ASCII table in
your head.

Nothing is written until it is asked for. Changes are held as a sparse map of
offset to byte and laid over the rows on the way past, so editing four bytes in
the middle of a four-gigabyte file still reads only what is on screen. Each
entry keeps what *was* there as well as what is there now, which buys two
things: `Backspace` undoes exactly, and a byte typed back to its original value
stops counting as a change rather than being a change that happens to look the
same. Editing the same byte twice keeps the value from **disk**, not from the
first edit, or one undo would restore something that was never there.

In the terminal the dump stays read-only until `F4`, so the letters are
shortcuts - `q` closes - and only then become hex digits. A dump you can type
into by accident is a file you corrupt by accident.

### Telling two directories apart

`Alt-C` marks what differs between the two panes: each side marks what it has
that the other does not, and what it has a newer copy of. No dialog and no
walk - it compares the two listings already on screen, which is what makes it
instant and what makes it stop at the top level. Once the differences are
marked they are an ordinary selection, so `F5` copies exactly them across.
Directories are left unmarked: whether two directories differ is a question
about their contents, and marking one would offer to copy the whole thing over
the answer.

`Alt-D` answers the next question, which is *how* two files differ: it puts
them side by side, lined up, with what is only on one side marked - red for
gone, green for arrived, and each file's own line numbers down its own gutter.
`Tab` walks to the next difference and `p` back to the previous one, which is
the point of the window: a file with two changes in nine hundred lines is not
one you scroll.

Which two files is the part with an opinion, and there are two ways of saying
it because they are the two ways anyone would:

- **Mark two files in one pane.** Comparing two versions sitting in the same
  directory is most of what this is for.
- **Put one under each pane's cursor.** The classic Commander gesture, and the
  pair comes back in pane order - the left pane's file on the left - whichever
  pane has the keyboard.

A single mark counts as that pane's choice, exactly as it does for every other
operation, so one file marked here and a cursor over there is a pair. Marks
that are neither one nor two are a message rather than a guess. The alignment
is Myers' algorithm, the one `git diff` uses, whose cost follows the size of
the *difference* rather than the size of the files - so ten thousand lines with
one edit in them align instantly.

Two files that turn out to be identical say so instead of opening a window of
unchanged lines, and two files that are not text get the only answer there is
about them: same bytes, or different bytes.

The graphical view also has this on `Shift-F3` - F3 shows one file, F3 with a
shift shows what two of them differ by. The terminal view cannot: the escape
sequence for Shift-F3 is `CSI 1;2R`, which is also the cursor-position report,
so it never arrives as a key at all. `Shift-F2` and `Shift-F4` are `CSI 1;2Q`
and `CSI 1;2S` and clash with nothing; F3 is the one unlucky letter of the
four, which is why `Alt-D` is the binding both front-ends share.

`Alt-S` is the recursive version. It walks both trees on a worker thread, and
the list fills while it goes:

| | |
| --- | --- |
| `->` | newer on the left, or only there |
| `<-` | newer on the right, or only there |
| nothing | either the two agree, or they differ with neither one newer |

Two files are the same when they are the same size and carry the same date to
within **two seconds** - FAT stores times to two seconds, so a file copied to a
memory stick and back is not a different file, and without that tolerance a
comparison against a backup is a wall of differences that are not there.
Ticking **compare contents** reads both files instead, which finds what a date
cannot: a file edited and then stamped back, or two files of the same length
that are not the same file.

The direction on each row starts at the obvious one and is yours to change -
click the arrow, or `Space` on the row in the terminal view - and the pairs it
cannot answer for start at nothing. Those are the ones where the two differ and
neither is newer, which means something is wrong that a rule should not guess
its way through. **Synchronize** then copies exactly what the arrows say, in
both directions in one run, making the directories it needs on the way.

A row is only offered the directions it could actually take. A file only the
right-hand side has cannot be copied *to* the right - there is nothing on the
left to copy - and cycling used to reach that direction anyway and then fail
the run on `No such file or directory`.

A thousand differences is not a thousand key presses. **All ->**, **All <-**,
**None** and **Reset** point the whole list at once, and only at what the
filter above them is showing: narrowing to *only left* and pressing **All ->**
sets the orphans without touching the rest of the tree. In the terminal view
those are `->`, `<-`, `-` and `*`. The comparison holds twenty thousand pairs
and says so when it stops there, rather than showing a list that stopped short
as though it were the whole tree.

Nothing is deleted. A synchronize that removes what the other side does not
have is the operation that eats work when a direction is misread, and it wants
a design of its own rather than a fourth value in an enum.

### The same file twice

`Alt-U` finds files under the active pane that are byte for byte identical,
however differently they are named and however deep they are.

The answer has to be exact, because what anyone does with it is delete
something - so nothing is reported that has not had both copies read. Hashing
is used to **narrow**, never to conclude, and every set is confirmed byte for
byte before it is shown. A hash collision is unlikely; deleting the wrong
photograph because of one is not a risk worth taking to save a second pass.

That said, most files are never opened at all. The walk collects names and
sizes without touching a byte, and two files of different lengths cannot be
copies - in a real directory nearly every size belongs to exactly one file,
and those are finished before anything is read. Only inside a size group does
the work begin.

Hard links are left out of it. Two names for one inode are trivially identical
and deleting one reclaims nothing, so offering them as duplicates would be
offering to do nothing and call it tidying.

A scan of six thousand files finds a thousand sets in about 50ms, and forty
thousand files takes about half a second - almost all of it reading the files
that share a size with another. Past **5000 sets** it stops and says so, which
is a memory guard rather than a judgement about the tree.

Each set says how many copies there are and what they weigh, and the line
underneath says how much of the total is the same thing over again - which is
the number anyone came for. Nothing is ticked to start with. **Keep the first**
ticks everything but one in a set, `Space` ticks a single copy, and a set will
not let go of its last copy: a duplicate finder that will delete every copy of
a file is not a duplicate finder, it is a delete key with extra steps. Deleting
goes through the ordinary delete, which means it asks first and goes to the
trash.

### Renaming a whole selection

`Shift-F2` (or `Ctrl-M`, which is the binding Total Commander uses and which a
terminal cannot tell apart from `Enter`) opens the multi-rename tool. You write
what the new names should be *made of* rather than typing each one:

| | |
| --- | --- |
| `[N]` | the name it already has, without its extension |
| `[E]` | the extension |
| `[N2-5]` | characters two to five of the name - also `[N3]`, `[N2-]`, `[N2,3]` |
| `[C]` | a counter: 1, 2, 3 |
| `[C001]` | padded to three digits - the leading zero is the request |
| `[C10+2]` | starting at ten, going up by two |
| `[Y]` `[M]` `[D]` | the file's own year, month and day |
| `[h]` `[n]` `[s]` | its hour, minute and second - `[n]` for minutes, since `[M]` is the month |

So `holiday_[C001]` over a camera dump gives `holiday_001.JPG`,
`holiday_002.JPG`, and so on, in whatever order the panel is sorted in - which
means sorting by date and numbering the selection are the same gesture. Below
the templates are a search-and-replace over the finished name and a case
conversion, and anything in brackets that is not a placeholder is left in the
name as the text it is.

Nothing is written until you say so. The list underneath shows every file as
`old -> new` while you type, and says which ones cannot be done before the
button will do any of them:

- **two files want this name** - a template with no counter in it, over a
  selection. Both are refused rather than one quietly overwriting the other.
- **already exists** - something is there that is not in the selection. A name
  freed by another file that *is* in the selection is fair game.
- **not a usable name** - empty, or containing a path separator. A rename stays
  in its directory; asking it to move somewhere is a different operation.

The ones that are fine still run, and the button says how many. The order they
run in is worked out rather than assumed: renaming `a` to `b` while `b` becomes
`c` only works one way round, and swapping two names does not work in any order
at all - so those go through a temporary and come out the other side. Nothing
is ever written over a file that has not moved yet.

### Everything from the keyboard

The graphical view answers to the Commander keys, and there is nothing in it
that needs a pointer. `F1` lists them all.

| | |
| --- | --- |
| `F1`..`F10` | help, rename, view, edit, copy, move, mkdir, trash, select menu, quit |
| `Tab` | the other pane |
| `Enter`, `Right` | open: a directory is entered, a file goes to its application |
| `Shift-Enter` | open with a chosen application |
| `Shift-F2`, `Ctrl-M` | rename the whole selection at once |
| `Ctrl-T` / `Ctrl-W` | another tab here / close this one |
| `Alt-W` | close the other tabs |
| `Ctrl-Tab`, `Ctrl-PgUp/PgDn` | walk the tabs |
| `Shift-F6` | send this tab to the other pane |
| `Alt-C` / `Alt-S` | mark what differs / synchronize |
| `Shift-F3`, `Alt-D` | compare two files, line by line |
| `Alt-U` | find files that are the same file twice |
| `Alt-T` | directory tree |
| `Shift-F4` | edit a file you do not own (`sudoedit`) |
| `Shift-F8`, `Shift-Del` | delete for good, without the trash |
| `Alt-F7`, `Ctrl-F` | find files by name or contents |
| `Alt-Enter` | properties and permissions |
| `Ctrl-E` | a shell as administrator, here |
| `Backspace`, `Left`, `Ctrl-PageUp` | parent directory |
| `Ctrl-\` | filesystem root |
| `Ctrl-U` / `Ctrl-R` | swap the panes / reload both |
| `Insert`, `Space` | mark, and step down |
| `*` | invert the marks |
| `+` / `-` | select / deselect by pattern |
| `Ctrl-A` | mark everything |
| `Ctrl-1`..`4` | list, grid, tree, quick view |
| `Alt-T` / `Ctrl-Q` | tree / quick view |
| `Ctrl-H`, `Alt-.` | show hidden files |
| `Ctrl-D` | bookmark this directory |
| `Ctrl-K` | colours |
| `F11` / `F12` | sidebar / second pane |
| `Ctrl-O` | show or hide the shell panel |
| `` Ctrl-` `` | type in the shell |
| `Shift-Esc` | leave the shell, back to the panes |

`F3` shows the file in the other pane's quick view - the built-in one, as
opposed to `Enter`, which hands it to the desktop. `F4` hands it to `$EDITOR`
in a shell tab - a file manager with real terminals in it has no business
bundling an editor, since the user already has one and it already knows their
settings.

While the shell has the keyboard, **only `Shift-Esc` is intercepted**: every
other key belongs to whatever is running in there, so `F10` closes `mc` rather
than closing this. It has to be a key no shell program wants, or leaving the
terminal would fight with what is inside it.

#### Typing, and why it never collides with a shortcut

The original's answer was that the two never overlapped. Everything printable
went to the command line at the bottom of the screen, and every panel command
was a key that types nothing - a function key, an arrow, Tab, Insert, or
something with Ctrl or Alt on it. There was nothing to disambiguate.

The exceptions were `+` `-` `*`, and they worked because a PC keyboard has
each of them twice: the panel used the **grey** ones on the numeric keypad,
which the hardware reports as different keys from the ones you type with. That
distinction is not available here - egui folds the numpad into the same key
values as the main row - so this uses the rule the original already applied to
`Enter`:

> A single-character panel command only applies while the command line is
> empty.

Empty line, and `*` inverts the marks, `Space` marks, `Enter` opens what the
cursor is on, `Backspace` goes to the parent. Start typing and all four belong
to the line: `find . -name *.rs` types the way it reads, and `Enter` runs it.
`Esc` throws the line away and hands the keys back.

So **typing with the panes focused lands on the shell's prompt**, without
clicking into it, exactly as it did in 1986 - and the arrows keep driving the
panes while a half-typed command sits there. Since the line here is a real
shell in another process, its input buffer cannot be asked what is on it; what
counts as "empty" is our own record of what has been typed since the last
`Enter`.

**The plain arrows and plain Tab stay with the panes**, deliberately. Every
Commander settled on that split - mc puts completion on `Esc-Tab` and history
on `Alt-P`/`Alt-N` for the same reason - because the workflow the layout
exists for is to start typing, walk to a file, and `Ctrl-Enter` its name onto
the line. Taking the arrows for line editing would cost exactly that.

What you get instead is a modifier for the two worth having without leaving
the panes, and a lossless way out for everything else:

| | |
| --- | --- |
| `Shift-Tab` | the shell's own completion |
| `Ctrl-Up` / `Ctrl-Down` | the shell's history |
| `` Ctrl-` `` | hand the keyboard to the shell |
| `Shift-Esc` | hand it back |

Escalating carries the line with it, because the line *is* the shell: type
`echo half` on the panes, press `` Ctrl-` ``, and you are editing that same
half-typed command with readline underneath - completion, history, `Ctrl-R`,
word movement, everything the shell has. There is nothing to reimplement and
nothing to lose on the way in.

`F8` and `Del` move to the trash and ask first; `Shift-F8` and `Shift-Del`
delete for good. They did neither at first, which for a recursive delete of
everything marked was not a risk worth taking on one keystroke.

The map itself is a pure function from key to intent, in `src/gui/keys.rs`, and
a test walks every key and modifier pair to check that every action the
application has is reachable without a mouse.

### Quick view

The fourth setting on a pane's own switch turns it into a **quick view**: it
shows whatever the *other* pane's cursor is on, and follows as that cursor
moves. That is what quick view has always meant - the pane opposite is where
you move, this one just watches. Only one pane can be a quick view at a time,
because two would have nothing to look at but each other.

| Kind | What is shown |
| --- | --- |
| text, source, config, and anything without an extension | the text itself, tabs expanded, any size |
| png, jpg, gif, bmp, webp, ico, tiff | decoded here, on a checkerboard so transparency reads as transparency |
| directory | how many entries it holds |
| anything else | a picture from the system, if it has one; otherwise the file's icon, name and size |

| Gesture | Text | Picture |
| --- | --- | --- |
| wheel | scroll down the file | zoom about the pointer |
| shift-wheel | scroll across | - |
| ctrl-wheel, pinch | resize the text | zoom |
| drag | - | pan |
| double-click | - | actual size, and back to fitted |

A picture arrives fitted to the pane and never enlarged past its own size - a
16-pixel icon blown up to fill a pane is a worse answer than a small sharp one
- and the caption says what percentage it is being shown at. Zoom and pan reset
when the cursor moves to another file, so a new picture always arrives fitted
rather than halfway off the pane; the text size does not, since that is a
preference rather than a position.

Text lines run rather than wrap, because wrapping folds a log or a table back
on itself and loses the shape that made it worth looking at.

**Size is not a limit.** There is no cap on how big a text file can be looked
at, because the file is never held: `src/textindex.rs` walks it once, notes
where every 256th line starts, and the view reads only the window it is
showing. A 65 MB, million-line log opens and scrolls to its last line, and the
index costs about 32 KB. Recording every line instead would have cost 8 MB, and
holding the file itself, 65 MB.

A file with no extension is read rather than guessed at: it is far more often
a script or a README than something to draw. If it turns out to be a binary -
which every compiled program is - the NUL bytes give it away and it is not
poured into the text view.

Loading happens on a worker thread. Walking a large file, decoding a
photograph, or waiting on a thumbnailer process are all far too slow to do
between frames, and the last of them spawns a program.

#### Reusing what the system already knows

Every desktop can already draw a PDF, a RAW photo or a video's poster frame,
and reimplementing that would be absurd. All three platforms expose the same
shape of thing - "give me a picture of this file" - so the seam here is a
command that writes a PNG.

* **Linux** has the freedesktop thumbnailer spec: `*.thumbnailer` files in
  `~/.local/share/thumbnailers` and `/usr/share/thumbnailers` declaring a MIME
  type and a command line, with `%i` `%u` `%o` `%s` filled in. Implemented and
  tested.
* **macOS** has Quick Look, the engine behind Finder's preview. `qlmanage -t`
  is the same idea from the command line. Implemented; the command it builds
  is unit-tested, but it has not been run on a Mac from here.
* **Windows** puts thumbnails behind `IShellItemImageFactory`, which is COM
  rather than a command and so does not fit this seam. Not wired up: Windows
  falls back to the built-in decoders. That is a gap, not a design choice.

A registered thumbnailer is not necessarily a working one - distributions ship
the `.thumbnailer` file and the binary in separate packages - so `TryExec` is
honoured before anything is run. The container this was built in is exactly
that case: it declares librsvg's thumbnailer and does not have
`gdk-pixbuf-thumbnailer`, so SVGs there fall back to the icon-and-facts card.

Adding a format is a line in one of two tables in `src/preview.rs`: an
extension in `DECODABLE` or `TEXTUAL` to handle it here, or a MIME type in
`mime_for` so the system gets asked.

### Turning, cropping and resizing a picture

`Alt-I`, or the **Edit** button on the quick view, opens the picture for the
four things anyone actually wants from a file manager: turn it, mirror it, take
a rectangle out of it, make it smaller.

Three rules keep it honest.

**The source is never touched.** A session holds the *operations* - an
`imageops::Edit` - and the untouched picture, never the result. Five presses of
rotate are one rotation of the original rather than five rounds of resampling,
and **Reset** drops a value rather than trying to invert one.

**The file is re-read at its own size.** The quick view downscales anything
enormous, because a panel a few hundred points wide has no use for six thousand
pixels - but saving *that* back would quietly throw away most of a photograph.
The editor opens the file again rather than borrowing what the preview has.

**Only what we decode ourselves.** A RAW or a HEIC reaches the preview as a
thumbnail the system drew, and a thumbnail is not the picture. Editing one and
saving it over the original would swap a photograph for a postage stamp, so
those are refused rather than offered.

Operations apply in a fixed order - crop, mirror, turn, resize - and the
[`imageops`](src/imageops.rs) module is the arithmetic on its own, tested
without a single decoded pixel. That is where the mistakes live:

* Two quarter-turns compose rather than accumulate, so four presses of rotate
  is where you started.
* **A flip flips what is on screen.** With a quarter turn in effect the
  screen's left-to-right is the source's top-to-bottom, so `Mirror` has to be
  *recorded* as the other one. A button that mirrors correctly until you rotate
  and wrongly after is worse than no button - and the line under the picture
  names the operation the way the button that did it was named, not the way it
  is stored.
* A crop is stored against the untouched source, so it does not jump the next
  time anything rotates. Folding a rectangle dragged over a turned, mirrored,
  already-cropped picture back onto the original is `imageops::fold_crop`, and
  the property that pins it is that dragging over *everything on screen* must
  come back as exactly the crop already in effect - checked for all sixteen
  combinations of turn and mirror.

**Save** writes over the original and asks first, inline, because a photograph
overwritten is a photograph gone. **Save as...** offers `photo-edited.jpg`
beside it.

#### Which formats, and what a save costs

Editable: **PNG, JPEG, GIF, BMP, WebP, ICO and TIFF** - the formats decoded
here. Anything else (RAW, HEIC, SVG…) reaches the quick view as a thumbnail the
system drew, and is refused rather than offered.

The **format is preserved**, in the sense that matters: `Save` writes through
the original path, the encoder is chosen from the extension, and a PNG stays a
PNG. `Save as` with a different extension converts, which is how you get out of
a format that will not hold what you want.

What is *not* preserved is everything that was never in the pixels, and the
window says so before Save rather than after:

| | |
| --- | --- |
| **an animation** | only the first frame survives. A GIF or animated WebP comes back as a still, said in the danger colour because it is impossible to undo and easy not to notice. |
| **metadata** | EXIF and its neighbours are dropped: dates, camera settings, **orientation**, any location. A decoder hands over a grid of colours and nothing else. |
| **JPEG and WebP** | re-encoded from those pixels, which costs a generation of quality even where nothing was touched. |
| **an ICO over 256px** | cannot be written at all - the format stores each dimension in one byte. Save is disabled with the reason, *before* the work, rather than failing after it. |

Whether a file is an animation is settled by decoding **two** frames and
stopping - the question is only ever yes or no, and a hundred-frame GIF is not
worth a hundred decodes. Which is exactly why the warning does not name a
number: it would be the number two whatever the file held.

### Terminals

The panel under the panes holds **real interactive shells**, each on its own
pseudo-terminal. `export` sticks, `cd` sticks, tab-completion is the shell's
own, up-arrow recalls the shell's history, and `vim`, `top` and `git rebase -i`
all work, because the shell is a genuine process talking to a terminal.

What is emulated is the *terminal device* - the VT100 whose escape sequences
every shell expects - which is the same job iTerm2 and Windows Terminal do.
`portable-pty` provides the pty (ConPTY on Windows) and `vt100` parses the
stream.

The window opens with one terminal already running on the default shell, in the
active panel's directory - present but not holding the keyboard, since the
files are what the window is for. Click it to type.

`+` opens another in the active panel's directory, so a long build can carry on
in one tab while another is used for something else; the arrow beside it starts
a specific shell. `-` closes the one on screen, and each tab has its own `x`.
Both buttons sit at the far end of the strip rather than after the last tab, so
they stay put - opening three terminals should not mean chasing a `+` that
walks right every time it is clicked.

Tabs are numbered when several run the same shell. `cd here` points the current
terminal at the active panel. A tab whose shell exits is closed automatically,
and closing the last one means closed - it does not reappear.

POSIX shells are started with `-i`, and additionally `-l` on macOS. Without
that a shell on a pty is still non-interactive: no prompt, no rc file, and none
of the user's aliases or `PATH` - and on macOS `PATH` is assembled in
login-only files. `fish` and `nu` are left alone, since they are interactive by
default and reject those flags.

Keys go to the shell whenever the terminal has focus: text as typed, and the
keys that have no character as the sequences a terminal sends - `Ctrl-C` as
`0x03`, arrows as `ESC[A`, backspace as DEL. `Ctrl-Enter` types the selected
file names into the shell instead of onto the one-shot command line.

Each session keeps 5000 lines of scrollback, so a long build's early output is
still there after it has left the screen.

| Key | |
| --- | --- |
| wheel | scroll by lines, over whichever terminal the pointer is on |
| `Shift-PageUp` / `Shift-PageDown` | back and forward a screen |
| `Shift-Home` / `Shift-End` | the oldest line / the live prompt |

Shift is what separates the two audiences, and has since xterm: a **bare**
`PageUp` belongs to whatever is running inside - `less`, an editor, `git log`'s
pager - and still goes down the pty untouched. Anything typed snaps the view
back to the prompt, since an echo landing off-screen is worse than useless, and
while the view is scrolled the panel says so and says how to get back.

### Keeping what the shell printed

`copy` puts the output on the clipboard. `save` writes it into the **active
panel's** folder - the one being looked at, not the process's own - as
`bash-20260725-121413.log`, named for the shell and the moment. Existing files
are never overwritten: a clash appends `-2`, because a saved log is not worth
losing to the one time the second-resolution stamp collides. The pane it lands
in is re-read, so the file appears straight away.

Both buttons serve whichever surface is showing - the terminal or the one-shot
command line - since "copy the output" means the same thing either way, and a
button that vanished when the panel was toggled would be silly.

What comes out is what was on screen, not the raw byte stream: escape sequences
are resolved, so a progress bar that rewrote its own line with carriage returns
appears once, in its final state.

`rec` is the other half. `save` can only ever offer what is still in the
scrollback; a recording starts a file and writes to it for as long as it is
left on, so a build that prints a hundred thousand lines is all there. Stopping
and starting again is always a new file - nothing appends, so one file is one
session's worth of output and its name says when it began. The button carries
the running line count, and the file is flushed as it goes, so `tail -f` on it
works.

The tap is on the **pty**, not on the view: every byte read from the terminal
device is recorded before the emulator sees it. That is what makes it catch
things the screen cannot - output past the scrollback, a background job that
inherited the descriptor, anything else writing to that terminal - and it is
why `rec` is only on the terminal, since the one-shot command line has no
stream to tap.

A recording and the account answer different questions - "what did it print"
against "what was run" - and neither substitutes for the other. A recording has
everything but is a text file you have to read; the account has one structured
line per command, filterable by day and kind, and no output at all. They are
wired together at one point: starting or stopping a recording is written to the
account, naming the file, so browsing what was done leads you to the transcript
of it.

### The one-shot command line

Under the panes sits a prompt running a **real shell** - not an emulation of
one. The command line offers a picker of the shells the machine actually has
(read from `/etc/shells` on Unix, the known install paths on Windows), and the
choice is remembered in `settings.toml`. With no explicit choice it follows
`$SHELL` / `%ComSpec%`, falling back to `/bin/sh` or `cmd.exe`.

The flag follows the *program*, not the platform: `cmd` wants `/C`,
PowerShell wants `-Command`, and a Git-Bash `bash.exe` on Windows still wants
`-c`. Entries that are the same binary by different paths are collapsed
(`/bin` is a symlink to `/usr/bin` on most Linux systems), but `rbash` and `sh`
survive alongside `bash` and `dash`, because a shell inspects `argv[0]` and
behaves differently under those names. Commands run in the active panel's directory, on a worker thread so
a slow build never freezes the window, and both panels are re-read afterwards
because a command may well have created or removed files.

The picker also says which shells can report what is run in them - the ones
that cannot are marked `· not recorded` rather than left out, since that is a
consequence worth knowing and not a reason to take the choice away. See
[the account](#asking-the-shell-rather-than-reading-the-screen).

| Key | Action |
| --- | --- |
| `Enter` | run the command |
| `Ctrl-Enter` | insert the selected file name(s) at the prompt |
| `Ctrl-Shift-Enter` | insert the full path(s) instead |

`Ctrl-Enter` is the original Commander gesture, and it inserts every marked
entry when there is a selection, or the entry under the cursor when there is
not. Names are quoted for the platform's shell, so `my holiday.jpg` arrives as
one argument rather than two.

Two things the command line handles rather than passing on:

* **`cd` is intercepted** and moves the panel. Handing it to a subprocess would
  change that process's directory and nothing else - the classic surprise.
* **Output is capped** at 256 KiB per command, so a stray `find /` cannot eat
  the machine.

### Colours

Colour is carried by the file-type icons; the chrome stays a quiet surface with
a single accent, so the two do not compete. Which colours those are is yours to
change: `Ctrl-K` opens the theme form.

**Three presets** are there to pick and be done with:

| | |
| --- | --- |
| **Midnight** | the default - a near-black window, blue cursor, amber marks |
| **Commander** | the original's palette: blue panels, cyan text, yellow cursor |
| **Paper** | light, for a bright room or a projector |

Below the picker the form lists **every colour the window has**, grouped by
what it does - Window, Text, Cursor and marks, Messages, File icons - each row
a swatch, a name, and the hex beside it. The swatch opens a colour picker; the
hex field takes `#4c8dff`, `#48f` or `4c8dff` typed or pasted, because a colour
is usually something you already have in writing rather than something you go
hunting for with a mouse.

Every change **applies as you make it**, to the whole window and to the form
itself - a theme is judged against real content, not against a preview strip.
Revert puts back what you started with, Cancel and `Esc` leave nothing behind,
and Done keeps it. Picking a preset and then changing one row is fine: the
dropdown reads "Custom" from that moment on.

What is kept is deliberately small. A preset is stored by name -
`theme = "Paper"` - so it follows any later change to what that preset means;
anything edited is written out colour by colour instead. Light and dark base
styling is not a fourth choice to get wrong but is derived from the background
you picked, so a light window gets light scrollbars and shadows without being
told.

There is one colour table and one type - `Palette` in `src/gui/theme.rs` - and
no colour anywhere else in the source. That is what makes "the form reaches
every colour" a test rather than a promise: it sets every field the form lists
to one distinctive value, saves, and counts it in the file.

### Verifying it without a screen

`rcmd-gui --screenshot out.png [DIRS]` renders a few frames, writes a PNG and
exits. Under a virtual X server that makes the graphical view checkable in CI:

```sh
LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 1280x780x24" \
    ./target/debug/rcmd-gui --screenshot shot.png ~/pictures ~/documents
```

Pointer behaviour is unit-tested through `apply_click` rather than by driving a
real window: a headless X server has no window manager, so modifier state is
not reliably delivered and a click test through it would prove nothing.

Requires a stable Rust toolchain (developed against 1.94; `rust-version` is
set to 1.74).

## Keys

| Key | Action |
| --- | --- |
| `Tab` | switch active panel |
| `Up` / `Down` / `PgUp` / `PgDn` | move the cursor |
| `Home` / `End` | first / last entry |
| `Enter` / `Right` | enter a directory, or open a file with its application |
| `Backspace` / `Left` | go to the parent directory |
| `Space` / `Insert` | mark the entry and step down |
| `*` | invert the marks |
| `+` / `-` | select / deselect by mask (`*.txt`) |
| `Ctrl-A` | mark everything |
| `F1` | help |
| `F2` | rename |
| `Shift-F2` | rename the whole selection at once |
| `F3` | view file |
| `F4` | edit: text with its encoding, a binary as bytes (graphical); `$EDITOR` (terminal) |
| `F4` in the dump | start editing the bytes (terminal) |
| `Alt-E` | edit with `$EDITOR` in a shell tab (graphical) |
| `e` / `E` in the viewer | read it as another encoding (terminal) |
| `F5` | copy to a directory |
| `F6` | move to a directory |
| `F7` | create a directory |
| `F8` / `Delete` | move to the trash (asks first) |
| `Shift-F8` / `Shift-Del` | delete for good |
| `F9` | cycle sort order: name → ext → size → time |
| `Ctrl-T` | another tab, here |
| `Ctrl-W` | close this tab |
| `Alt-W` | close the other tabs |
| `Ctrl-PgUp` / `Ctrl-PgDn` | walk the tabs |
| `Shift-F6` | send this tab to the other pane |
| `Alt-C` | mark what differs between the panes |
| `Alt-D` | compare two files, line by line |
| `Alt-U` | find files that are the same file twice |
| `Alt-S` | synchronize the two directories |
| `Alt-I` | turn, crop or resize the picture (graphical view) |
| `Ctrl-J` | what was done - the account |
| `Alt-T` | directory tree on/off |
| `F10` / `q` | quit |
| `F11` / `Ctrl-B` | network locations & bookmarks |
| `Ctrl-D` | bookmark the current directory |
| `Ctrl-H` | toggle hidden files |
| `Ctrl-R` | reload both panels |
| `Ctrl-U` | swap the panels |
| `Ctrl-P` | open with a chosen application |
| `Shift-F4` | edit a file you do not own (`sudoedit`) |
| `Ctrl-E` | a shell as administrator, here |
| `Ctrl-F` / `Alt-F7` | find files by name or contents |
| `Alt-Enter` | properties and permissions |

Operations act on every **marked** entry, or on the entry under the cursor when
nothing is marked. `..` is never a target.

## Directory tree

`Alt-T` turns the active panel into a directory tree, rooted at the filesystem
root and already opened down to wherever you were. (It was `Ctrl-T` until tabs
arrived and wanted the key every program with tabs uses; the graphical view
also has it on `Ctrl-3`.)

```
┌───── Tree: /home/user/rust-commander/src ──────┐
│- /                                             │
│  + etc                                         │
│  - home                                        │
│    - user                                      │
│      - rust-commander                          │
│          src                                   │
│        + target                                │
│      + winamp                                  │
│  + opt                                         │
└────────────────────────────────────────────────┘
```

| Key | Action |
| --- | --- |
| `Up` / `Down` / `PgUp` / `PgDn` / `Home` / `End` | move |
| `Right` / `+` | expand |
| `Left` / `-` | collapse, or step out to the parent when already collapsed |
| `Space` | toggle |
| `Enter` | go to that directory and close the tree |
| `Esc` / `Alt-T` | close without moving |
| `Ctrl-R` | re-read the tree, keeping what is open |

`+` means "can be opened", `-` means "open", and a blank marker means a branch
already checked and found to have no subdirectories.

Children are read only when a node is expanded, so opening the tree on a large
filesystem costs one `read_dir` rather than a full walk. Keys the tree does not
use - `Tab`, `F10` and the rest - still reach their normal bindings, so the
other panel stays usable while a tree is open.

Directories that the listing hides are still spliced in when they sit on the
path to your current directory: standing in `~/.config/app` has to be
reachable in the tree even though `.config` is hidden.

## Long operations

Copy, move and delete run on a worker thread behind a progress dialog, so the UI
keeps repainting and `Esc` cancels at any point:

```
┌─────────────────────────── Copying ────────────────────────────┐
│/tmp/media/source/dir1/big3.bin                                 │
│                                                                │
│██████████████████████████████ 52%                              │
│8 / 15 items    311M / 600M                                     │
│                                                                │
│Esc = cancel                                                    │
└────────────────────────────────────────────────────────────────┘
```

Files are copied in 128 KiB chunks, so the bar moves *within* a single large
file and a cancel takes effect promptly rather than at the next file boundary. A
cancelled copy removes the half-written file instead of leaving a truncated one.
Totals come from a pre-scan of the sources, and the bar tracks bytes when there
are any (item counts alone would sit at 0% through one huge file).

Failures on one source do not abort the rest; everything that could not be done
is reported together at the end.

## Network locations (SMB, FTP, …)

Press `F11` (or `Ctrl-B`) for the locations screen. It has two lists, switched
with `Tab`: **Saved** (bookmarks you keep) and **Recent** (the last 20
directories you visited, newest first).

```
┌──────────────────────────────── Locations ─────────────────────────────────┐
│ Saved (3)    Recent (12)    Tab switches                                   │
│ nas.local/media    SMB    smb://alex@nas.local/media                       │
│ ftp.example.org/p~ FTP    ftp://ftp.example.org/pub                        │
│ src                local  /home/user/rust-commander/src                    │
│Enter go  a add  c add cwd  u unmount  d delete  Esc close                  │
└────────────────────────────────────────────────────────────────────────────┘
```

**Saved**: `Enter` goes there (mounting first if needed), `a` adds a typed
location, `c` saves the current directory, `u` unmounts, `d` deletes.

**Recent**: `Enter` goes back, `s` promotes the entry into Saved, `d` forgets
one, `C` forgets everything. Every directory either panel lands in is recorded -
whether you got there by `Enter`, `Backspace`, the tree or a bookmark - and
re-visiting moves an entry back to the top instead of duplicating it. Both lists
persist across sessions.

Bookmarking inside a mounted share records the **network location**, not the
local mount path: saving while in `/Volumes/media/photos` stores
`smb://alex@nas.local/media/photos`, which still works after a reboot when
`/Volumes/media` no longer exists.

`a` accepts any of:

```
smb://user@host/share/subdir
ftp://host:2121/pub
sftp://build@ci.internal/srv
\\fileserver\share            (Windows UNC)
/any/local/directory
```

Saved locations and history persist to `bookmarks.toml` in the platform config directory
(`~/.config/rust-commander/` on Linux, `~/Library/Application Support/` on
macOS, `%APPDATA%` on Windows). The file is plain TOML and safe to hand-edit.

### How mapping works, and why

Rather than implementing SMB/FTP in-process, `rcmd` asks the operating system to
attach the share and then browses the result with the ordinary local code. One
filesystem implementation, and — importantly — **authentication is delegated to
the OS credential store**. Passwords are never prompted for, handled, or stored
by this program; only the user name is saved.

| | SMB | FTP / SFTP | NFS / AFP |
| --- | --- | --- | --- |
| **macOS** | `open smb://…` → mounts under `/Volumes`, Keychain handles auth | not mountable — Finder dropped FTP in macOS 11+ | `open` (NFS, AFP) |
| **Windows** | native: a UNC path needs no mount at all | map it in Explorer first, then bookmark the drive | map in Explorer |
| **Linux** | `gio mount` (gvfs, no root needed) | `gio mount` | needs a privileged `sudo mount` |

Already-mounted shares are detected by scanning `/Volumes`,
`/run/user/*/gvfs`, `/media` and `/mnt`, so connecting to something already
attached is instant. Unsupported combinations report *why*, with a suggested
workaround, instead of failing silently.

Two known rough edges: mounting blocks the UI for up to 10 seconds while the OS
works, and on Linux a share that needs interactive credentials will fail rather
than prompt (stdin is deliberately closed so it cannot hang the TUI) — configure
credentials in gvfs first, or mount it manually and bookmark the path.

## Command line

```sh
rcmd [LEFT_DIR] [RIGHT_DIR]
rcmd --list [DIR]     # print a directory listing and exit (no TUI)
rcmd --help
rcmd --version
```

`--list` renders the same listing logic without a terminal, which makes it handy
for scripting and for testing in CI.

## Portability

This is the whole point of the design, so it is worth being explicit about it.

* **Terminal**: everything goes through [crossterm], which drives the Windows
  console (ConPTY), macOS and Linux terminals from one code path. Key events are
  filtered to `KeyEventKind::Press` because Windows also reports key releases.
* **Filesystem**: only `std::fs` and `std::path` are used — no POSIX-only calls.
  Paths are `PathBuf` throughout, so Windows drive letters and separators work.
* **Hidden files**: the Unix leading-dot convention, plus the real
  `FILE_ATTRIBUTE_HIDDEN` attribute on Windows (behind `#[cfg(windows)]`).
* **Moves across filesystems**: `fs::rename` fails with `EXDEV` between mounts,
  so moves fall back to copy-then-delete automatically.
* **Root directories**: `..` is omitted when the current directory has no
  parent, which covers both `/` and `C:\`.
* **Terminal restoration**: a panic hook restores the terminal, so a crash
  cannot leave the shell in raw mode.

Both non-native targets are verified to compile:

```sh
cargo check --target x86_64-pc-windows-msvc
cargo check --target aarch64-apple-darwin
```

To produce actual binaries, build on (or cross-compile with a linker for) the
target platform:

```sh
cargo build --release --target aarch64-apple-darwin   # on macOS
cargo build --release --target x86_64-pc-windows-msvc # on Windows
```

[crossterm]: https://github.com/crossterm-rs/crossterm

## Layout

| File | Responsibility |
| --- | --- |
| `src/lib.rs` | the shared engine, with no user interface attached |
| `src/main.rs` | terminal front-end: setup/teardown, event loop, `$EDITOR` |
| `src/bin/rcmd-gui.rs` | graphical front-end: window setup and arguments |
| `src/app.rs` | application state, dialogs, key dispatch (pure, unit-tested) |
| `src/entry.rs` | directory-entry model and display formatting |
| `src/fsops.rs` | instant operations: rename / mkdir / file preview |
| `src/progress.rs` | copy / move / delete on a worker thread, with cancel |
| `src/config.rs` | persisted preferences, such as the chosen shell |
| `src/netloc.rs` | location URLs and the persisted bookmark store |
| `src/mount.rs` | per-platform mapping of network locations onto paths |
| `src/open.rs` | handing a file to the desktop, and what not to hand it |
| `src/apps.rs` | which applications could open a file, for "Open with..." |
| `src/find.rs` | searching a tree by name and by contents, off the UI thread |
| `src/perms.rs` | permissions, ownership and dates, and what each platform has |
| `src/rename.rs` | new names for a whole selection, and the order to write them in |
| `src/tabs.rs` | the directories one pane holds open, and which is on show |
| `src/compare.rs` | what differs between two trees, and which way it would go |
| `src/diff.rs` | two files line by line, and which two the panes mean |
| `src/hex.rs` | the files that are not text, shown as bytes |
| `src/dupes.rs` | files that are the same file twice, found without guessing |
| `src/panel.rs` | one pane: listing, cursor, sorting, marks, and noticing outside changes |
| `src/elevate.rs` | asking the system to authorise, and what not to ask it for |
| `src/trash.rs` | deleting to the system's trash, so it can be got back |
| `src/ui.rs` | all ratatui drawing |
| `src/tree.rs` | the directory tree: a flattened, lazily expanded node list |
| `src/preview.rs` | quick view decisions, and the system thumbnailer seam |
| `src/textindex.rs` | where a file's lines are, so its size stops mattering |
| `src/pty.rs` | interactive shells on pseudo-terminals, and their scrollback |
| `src/record.rs` | recording a session: pty stream to plain text on disk |
| `src/theme.rs` | the terminal view's classic blue/cyan palette |
| `src/gui/mod.rs` | the graphical view: sidebar, breadcrumbs, panes, status |
| `src/gui/preview.rs` | quick view: loading off-thread, decoding and drawing |
| `src/gui/icons.rs` | file-type and toolbar icons, drawn as vector shapes |
| `src/gui/theme.rs` | every colour, the presets, and the egui styling built from them |

Key handling is a pure state transition (`App::on_key`), so the whole
interaction model is tested without a terminal — see the tests in `src/app.rs`.

## Status

Working: dual panes, tabs in each of them, navigation, marking, sorting, copy,
move, rename - one file or a whole selection at once - mkdir,
comparing two directories and synchronizing them, comparing two files line by
line, a hex view for the files that are not text, finding files that are the
same file twice,
delete (the long ones with a cancellable progress dialog), directory tree
navigation, find by name and contents, properties and permissions, listings
that keep up with outside
changes, file viewer, opening files with the
desktop's own application or
a chosen one,
`$EDITOR` integration, hidden-file toggle, help, and saved local/network
locations with OS-level mounting.

Not yet implemented: in-place text editor, archive browsing, a panel filter,
and built-in FTP/SFTP clients — which are
what would make those protocols work on macOS, where the OS can no longer mount
either of them. Both would sit behind the same seam, so it is one piece of work.

Also outstanding: mounting a network share still blocks the UI for up to 10
seconds (it should move onto the same worker-thread machinery the file
operations now use).

## License

MIT
