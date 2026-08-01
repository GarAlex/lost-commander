# Handoff

For whoever picks this up next. What it is, how it is built, what was just
done, and where the edges are.

## What this is

**lost-commander** — a dual-pane file manager in the Norton/Total Commander
tradition, with two front-ends over one engine:

| | |
|---|---|
| `rcmd` | terminal UI, ratatui 0.30 + crossterm 0.29 |
| `rcmd-gui` | graphical, eframe/egui 0.32 — the Linux one |
| `ffi/` | the C ABI, for front-ends that are not written in Rust |
| `core/` | the engine they all use — no UI code at all |

A native Windows front-end (WinUI 3, in C#) lives in its own repository and
calls in through `ffi/`. Nothing here knows it exists, which is the point:
`ffi/` is a contract, not a private arrangement with one caller.

~34,000 lines of engine and front-ends plus ~6,000 of C ABI; 966 tests on
Windows (768 engine, 92 `rcmd`, 106 `ffi`), and a few more where zsh, fish and
dash are installed, since those tests skip rather than fail without them.
`cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets` has
21 warnings left, all pre-existing and none from this code being wrong — 13
are one `f32`/`f64` inference lint in the graphical front-end. Rust 2021
edition, `rust-version = "1.74"`.

## The rules the code is written to

These are not style preferences; they are why the code looks like it does, and
several bugs this session came from breaking one.

**An answer that looks like an answer is worse than no answer.** Silently
dropping data you cannot handle is the failure mode to fear. Two real examples
from this session: a `let Ok(x) = … else { continue }` that quietly dropped
every encrypted entry, so a locked zip of three files listed as one; and
reading a shell's command line off the screen, which produces plausible wrong
answers. Both were found by driving the real binary, not by reading code.

**When it cannot know, it says so.** A shell that cannot report its commands
writes a line in the account saying that, because an empty tab must never be
able to mean "nothing was run" when the truth is "nothing could be seen".

**Comments say why, not what.** Every non-obvious line carries the reason it is
that way — usually a bug that made it necessary. Match this. A comment
restating the code is noise; a comment naming the failure it prevents is the
most valuable thing in the file.

**Tests are named as sentences and assert behaviour.** `a_locked_zip_still_lists_in_full`,
not `test_zip_2`. Where possible they run against real artefacts — archives made
by the system's own `zip`/`tar`, real shells on real ptys — because code that
only agrees with itself proves nothing.

**Passwords are never written anywhere.** In memory, for the session. Stated in
`core/src/netloc.rs` for network credentials and followed in the graphical
front-end for archives.
They never reach the journal either.

## Layout

Four crates in one workspace. The dependency direction is one way and the
compiler is what keeps it that way: each front-end depends on `core`, and
`core` depends on none of them.

```
core/src/         the engine - no drawing, no eframe, no ratatui
  lib.rs          module list, no logic
  panel.rs        a pane: cwd, entries, cursor, marks, sort — and `Inside`
  entry.rs        one row
  progress.rs     Operation/Job/Sink — every long-running file operation
  journal.rs      the account of what was done
  archive/        reading archives (mod.rs + one file per format)
  shellhook.rs    OSC 133 shell integration
  pty.rs          real shells on real ptys
  themes.rs       named colour schemes, in a shape any front-end can read
  ...             compare, dupes, find, perms, trash, encoding, hex, imageops…

tui/src/          rcmd
  main.rs         arguments and the event loop
  app.rs          terminal front-end state + keys   (large)
  ui.rs           ALL terminal drawing              (large)
  theme.rs        the terminal palette

egui/src/         rcmd-gui
  lib.rs          the window                        (very large, ~9k lines)
  main.rs         the thin shell that opens one
  *.rs            theme, icons, keys, journalview, textedit, hexedit, imageedit

ffi/src/lib.rs    the C ABI
```

Anything with real logic lives in `core` and is unit-tested without a UI. It
used to be possible to break that rule quietly - the graphical front-end was a
module of the engine behind a default feature, so `crate::gui::` compiled fine
from anywhere in it. Now it does not compile at all, which is the point of the
crates being crates.

One consequence worth knowing before it surprises you: `core` cannot hang
inherent methods on its own types *for* a front-end, and a front-end cannot
hang them on `core`'s types either. `egui/src/icons.rs` used to add a
`Kind::colour()`; it is a free `colour(kind)` now, because what colour
something is drawn in was always that view's opinion rather than the engine's.

`tui/src/ui.rs` and `egui/src/` should hold layout only.

## What was built in the recent sessions

Newest first. Each is one commit with a long message explaining the reasoning —
**read the commit message before changing any of this**, it usually contains the
bug that forced the design.

### Archives, read-only (`615dd7e`, `ceb9219`, `1884854`)

`Enter` on a `.zip`/`.tar.gz`/`.7z`/`.lzh` walks into it as a folder, in both
front-ends. `F5` copies out. Everything that would write refuses.

- `core/src/archive/mod.rs` — `Member`, `Reader` trait, `FORMATS` table, sniffing,
  and the level-walking (`at`, `under`, `holds`) that turns a flat member list
  into browsable directories.
- One file per format: `zip.rs`, `tarball.rs` (tar/gz/xz/bz2), `sevenz.rs`,
  `lha.rs`. **Adding a format is one module and one line in `FORMATS`.**
  `lha.rs` is 70 lines and touches nothing else — use it as the template.
- `panel.rs` gained `Inside { archive, at, format, members, password }`.
  `Panel::cwd` becomes a synthetic path (`/home/me/docs.zip/docs`) that nothing
  can stat — the directory watcher is disabled while inside for that reason.
- `progress.rs` gained `Operation::Extract`, which runs through the same job
  machinery as a copy and is journaled as `Kind::Copy`.

Decisions worth not re-litigating:

- **Bytes before names** when identifying a format. A zip saved as `.jpg` opens.
- **Directories are inferred**, not trusted — a zip of `docs/a.txt` need not
  contain an entry for `docs`.
- **Extraction lands relative to the level being viewed**, not the archive root.
- **Locked archives still list in full.** A zip keeps its names in the clear.
- `NEEDS_PASSWORD` and `WRONG_PASSWORD` are separate errors so the UI can tell
  "not asked yet" from "that is not it".

### Shell integration (`f87f3b7`, `8c2a006`)

The terminal panel runs a real shell, so what you type never passes through this
program. It now asks the shell instead, via `OSC 133` marks injected into the
shell's own startup (`shellhook.rs`).

- bash, zsh, fish are hooked. `sh`/`dash`/`ksh`/`nu` have no seam and are
  **still offered**, marked `· not recorded` — and opening one writes a journal
  line saying its commands are not recorded.
- bash reads its line from `PS0` + history, **not** the `DEBUG` trap:
  `$BASH_COMMAND` is one simple command, so a pipeline arrives as its first
  component and a subshell not at all. The `DEBUG` trap survives only as a
  tie-break for the `HISTCONTROL=ignoredups:ignorespace` ambiguity — read the
  `Pairing` doc comment before touching this.
- Commands carry the shell name and a duration.

### The account / journal (`1096374`, `479ca52`, `029cc3d`)

`Ctrl-J`. Three tabs — All / Files / Commands — over two append-only JSONL
files a day in `~/.config/lost-commander/journal/`.

- Runs are groups with a heading written *before* the work, so a killed run
  still has one; a separate `Done` record carries the total time, and its
  absence means the run never finished.
- Durations on commands and on runs, never per file.
- The search box matches **everything the row shows** — path, target, note,
  failure, shell, kind label — not just paths.

### Before that

Image editing (crop/rotate/resize with format-loss warnings), text editing with
encoding detection and override, a hex editor, folder compare/sync, duplicate
finder, network locations, tabs, trash.

## Working on it

**Before every commit:** `cargo test --workspace`, `cargo clippy --all-targets`,
`cargo fmt --check`, and `cargo check --no-default-features` (the engine must
build without the GUI).

`--workspace` is not optional. A bare `cargo test` at the root runs the root
package only — 860 of the 966 tests — and silently skips every one of `ffi/`'s,
which is exactly the code least likely to be exercised any other way.

## Verifying by driving the real thing

This has found nearly every real bug in these sessions. Reading the code does
not substitute for it.

Two working harnesses live in `harness/`, with their own README:
`gui_archives.sh` (Xvfb + xdotool + scrot) and `tui_journal.py` (pty + pyte).

**Graphical**, under Xvfb — `harness/gui_archives.sh` is the pattern:

```bash
Xvfb :99 -screen 0 1400x860x24 &
DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 XDG_CONFIG_HOME=/tmp/conf ./target/debug/rcmd-gui A B &
xdotool key --clearmodifiers F5 ; scrot -o shot.png
```

**Terminal**, with `pty.fork()` + `pyte` — `harness/tui_journal.py`.

Two traps that cost real time:

- `pkill -f Xvfb` in the *same* Bash call that writes the script via heredoc
  kills the wrapper (exit 144). Run the script in a separate call.
- `pkill -f rcmd` matches background task wrappers. Use `pkill -x rcmd`.

Also: F5 starts a copy or extract **immediately** — there is no confirmation.
A harness that sends `Return` afterwards out of habit will navigate the pane and
look like a bug. It is not one.

## Where the edges are

**Archives are read-only.** The agreed design for writing, if it is picked up:

- Listing and single-file reads stay lazy — cost proportional to the task.
- The moment something needs to *change*, extract to a working copy so every
  existing subsystem (editors, F5/F6/F2, permissions, `cd here`) works on real
  paths unchanged. This matches how `netloc.rs` already handles non-local
  places: make it a real directory.
- **Repack by copying untouched entries through byte-for-byte.** Recompressing
  everything to rename one file silently flattens permissions, symlinks, extra
  fields and comments — data loss from a rename. Write to a temp file beside the
  archive and rename over it.
- Formats split: zip supports per-entry update; `.tar.*` must be rewritten
  wholesale anyway; rar/7z stay read-only.

**Other known gaps**

- No `.arc`, `.rar`, `.cab`, `.zst`. The user intends to add formats — the
  registry exists for exactly this.
- Nothing previews an archive member *in-place*: `Enter` extracts a copy to
  `/tmp/lost-commander-<pid>/` and hands it to the desktop. F3 quick-view of a
  member is not wired.
- `PtySession::shell_cwd()` is plumbed and tested but **nothing acts on it**.
  Making a pane follow an interactive `cd` is a small follow-on and was
  deliberately left as a UI decision for the user. One thing to know first: a
  Unix-flavoured bash on Windows — Git Bash, MSYS, WSL — answers in *its*
  namespace, calling `C:\Users\me\AppData\Local\Temp\x` by the name `/tmp/x`.
  Nothing translates that, so a pane fed it would find nothing there.
- The terminal front-end has no pty terminal panel, so shell integration and
  `rec` are graphical-only.
- ~~Deleting to the Windows recycle bin starts a PowerShell per file.~~ **Fixed.**
  The batch now goes in one call and the script reports on each path, so the
  per-file progress, error attribution and journal records survive. Eight files
  went from 4.53 s to 0.78 s; what is left is one shell startup however many
  there are. See the commit for why a path the report says nothing about is an
  error and never a success.

## The C ABI

`ffi/` is how a front-end that is not written in Rust drives the engine. There
is one such front-end today — WinUI 3, in C#, in a repository of its own,
because a file manager is judged against the Explorer window next to it and
egui draws its own widgets on a GPU surface: no shell context menus, no drag
and drop with Explorer, no native file dialogs, and the one that decides it,
no UI Automation, so no screen reader can read a pane at all. `rcmd-gui`
stays the Linux front-end. macOS is not started.

None of that is a reason for anything in here to know who is calling. The
rules below are the contract, and they are what makes a second caller — a
GTK front-end, a Swift one, a test harness in Python — possible at all.

**The boundary** is `ffi/`, deliberately narrow.

- **Values cross as JSON.** A `#[repr(C)]` mirror of every engine type would be
  faster and would also be a second definition of each, free to disagree with
  the first in silence. When a directory of a hundred thousand entries makes
  this the slow part, replace it with a flat array of fixed-width records —
  *measured first*.
- **The front-end polls; nothing calls back into it.** A function pointer into
  managed code called from a Rust worker thread is where interop gets
  frightening. `Job` already hands out a snapshot, so a timer suffices.
- **Nothing may unwind into C.** Every entry point catches a panic. Every
  string and handle has one owner and one way back — `rcmd_string_free`,
  `rcmd_job_free`.
- **Keys cross by name, not by code** — `rcmd_term_key("Up", ctrl, alt)`. A key
  code is a fact about one windowing system, and a table of them on this side
  would be that system's table living in the engine.

**If you extend it, drive it.** Every fault in the first version was invisible
in the source and obvious in the window: `..` appeared twice, rows read as
their class name to a screen reader, and Enter and Tab did nothing because the
list had marked the key handled before the window ever saw it. None of those
is findable by review, and the tests in `ffi/` cannot find them either — they
prove the ABI answers correctly, not that anything sensible is drawn.

## Windows

It builds and passes there. `cargo test` is green on Windows 11 with the
engine, the `rcmd` binary and the GUI: 712 + 92 tests, and `rcmd --list` lists
a real directory. Two things had to change in the engine, and neither was a
test problem:

- **A terminal has to answer when it is asked something.** ConPTY opens by
  sending `ESC[6n` — "where is the cursor?" — and renders *not one byte* until
  it is told. Nothing answered, so every terminal tab on Windows stayed blank
  forever and looked like a shell that had failed to start. It had started; it
  was waiting for us. `Answering` in `pty.rs` replies from the emulator's real
  cursor position. Found by driving cmd.exe through a real pty and reading the
  four bytes that came back, not by reading code.
- **A session is finished when its shell has exited**, which is not the same
  as the pty reaching end of file. On Unix the last close of the slave ends
  the stream, so the reader's EOF was the whole answer. Windows keeps the
  pseudoconsole open with the master, so the read never returns zero and a tab
  whose shell had gone was never reaped. `finished()` now asks the process too.

The tests themselves carried Unix assumptions, which is why 29 of them failed
on a tree that built fine. Worth knowing when adding more:

- `pty::plain` (in `pty.rs`, `#[cfg(test)]`, used by the GUI tests too) is the
  shell abstraction: it hands out a plain shell and the few things the tests
  need to say to it — set a variable, list a directory, print N numbered
  lines — spelled per platform. **Ask it for what you need rather than naming
  `/bin/sh`.** `plain::found(&["bash"])` and `plain::hookable()` search PATH as
  well as `/usr/bin` and `/bin`, so a Git Bash is found and the hook tests
  really run instead of quietly skipping.
- Where the bytes matter — carriage returns that overwrite a line, the escape
  that switches to the alternate screen — the tests write a file and have the
  shell copy it out (`plain::dump`). Typing an escape at a prompt does not
  work: that is what the Windows line editor uses to clear what you typed.
- Still Unix-only, for reasons that are not going away: the two gvfs tests in
  `mount.rs`, because a colon is not legal in a Windows filename and
  `candidate_roots(Windows)` is empty anyway; and the chooser tests in
  `app.rs`, because Windows has a chooser of its own.
- `clippy --all-targets` has four warnings left, all pre-existing and all from
  lints newer than whenever this was last clean: `apps.rs` `sort_by_key`,
  `rename.rs` and `shellhook.rs` `match`-to-`?`, `app.rs` collapsible `if`.

## If you read nothing else

1. Drive the binary. Every real bug this session came from that, not from
   review.
2. When something cannot be known, say so in the UI rather than leaving a
   silence that reads as an answer.
3. Read the commit message before changing what it describes.
