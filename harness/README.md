# Driving the real thing

Two harnesses, kept because reading the code is not a substitute for running
it — nearly every real bug in this program was found here rather than in
review.

| | |
|---|---|
| `gui_archives.sh` | drives `rcmd-gui` under Xvfb with `xdotool`, screenshots with `scrot` |
| `tui_journal.py`  | drives `rcmd` on a real pty, reading the screen with `pyte` |

Both build their own fixtures and clean up after themselves. Point `BIN` at a
different binary to run them against one.

```bash
cargo build --features gui
./harness/gui_archives.sh          # needs Xvfb, xdotool, scrot, zip, tar
python3 ./harness/tui_journal.py   # needs pyte
```

## Two traps that cost real time

**Killing Xvfb in the same shell call that writes a script via heredoc kills
the wrapper** (exit 144). Write the script in one call, run it in another.

**`pkill -f rcmd` matches background task wrappers.** Use `pkill -x rcmd`.

## One thing that looks like a bug and is not

`F5` starts a copy or an extract **immediately** — there is no confirmation
dialog. A harness that sends `Return` afterwards out of habit will find that
`Return` landing on the panel, where it navigates. The pane appearing to walk
somewhere after an operation is usually this.
