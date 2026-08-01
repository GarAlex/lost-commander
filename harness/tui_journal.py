#!/usr/bin/env python3
"""Tab through the three views in the terminal front-end."""
import os, pty, time, select, struct, fcntl, shutil, termios
import pyte

BIN = os.environ.get(
    "BIN",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "debug", "lostc"),
)
WORK = "/tmp/tmwork"
CONF = "/tmp/tmconfig"
ROWS, COLS = 30, 118

shutil.rmtree(WORK, ignore_errors=True)
shutil.rmtree(CONF, ignore_errors=True)
os.makedirs(f"{WORK}/from")
os.makedirs(f"{WORK}/to")
for n in ("alpha", "beta"):
    open(f"{WORK}/from/{n}.txt", "w").write(f"contents of {n}\n")

# A command in the shell stream, written directly - the terminal front-end has
# no pty panel, so this stands in for one having been recorded.
os.makedirs(f"{CONF}/lost-commander/journal", exist_ok=True)
import json, datetime
now = int(time.time())
day = datetime.date.today().isoformat()
with open(f"{CONF}/lost-commander/journal/shell-{day}.jsonl", "w") as f:
    f.write(json.dumps({"record": "event", "at": now - 60, "kind": "command",
                        "path": "/tmp/tmwork", "note": "cargo build"}) + "\n")
    f.write(json.dumps({"record": "event", "at": now - 30, "kind": "session",
                        "path": "/tmp/tmwork",
                        "note": "dash has no way to report what it runs"}) + "\n")

screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.environ["XDG_CONFIG_HOME"] = CONF
    os.execv(BIN, ["lostc", f"{WORK}/from", f"{WORK}/to"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))


def pump(seconds=0.9):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            stream.feed(data)


def send(keys, wait=0.7):
    os.write(fd, keys)
    pump(wait)


def show(title, rows=14):
    print(f"\n===== {title} =====")
    for line in screen.display[:rows]:
        text = line.rstrip()
        if text:
            print("  |" + text)


pump(2.0)
send(b"\x01", 0.5)          # Ctrl-A, mark all
send(b"\x1b[15~", 1.0)      # F5
send(b"\r", 2.5)            # confirm

send(b"\x0a", 2.0)          # Ctrl-J
show("Ctrl-J - All")
send(b"\t", 1.2)
show("Tab - Files")
send(b"\t", 1.2)
show("Tab - Commands")
send(b"\t", 1.2)
show("Tab - back to All")

send(b"/", 1.0)
show("/ - the search box")
send(b"cargo", 1.2)
show("typed 'cargo' - a word on no path")
send(b"\x7f" * 5, 1.0)
send(b"alpha", 1.2)
show("typed 'alpha' - a file inside the run")

send(b"\x1b", 0.5)
try:
    os.kill(pid, 9)
except OSError:
    pass
