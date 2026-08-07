# Privacy policy

**lost-commander collects nothing and sends nothing.**

There is no telemetry, no analytics, no crash reporting to us, no update
check, no account, and no network connection made by the program on its
own behalf. Nothing you do in it is transmitted anywhere.

This policy covers the lost-commander file manager in all the shapes it
ships in: the terminal program (`lostc`), the graphical one (`lostc-gui`),
and **Lost Commander** for Windows from the Microsoft Store.

## What is written, and where

The program writes several files under your own user profile, and reads
them back when it starts. They never leave your machine.

On Windows they live in `%APPDATA%\lost-commander`; on Linux and macOS, in
the usual configuration directory (`~/.config/lost-commander` or the
platform's equivalent).

| File | What is in it |
|---|---|
| `settings.toml` | Your preferences: colour scheme, where the dividers sit, how tall the shell drawer is |
| `bookmarks.toml` | Folders you pinned |
| `recent.toml` | Folders you visited recently |
| `workspaces.toml` | Your open windows, so they come back as they were: their folders, layout, and the kind of shell each had |
| `pinned.toml` | Commands you pinned to a folder |
| `journal/files-YYYY-MM-DD.jsonl` | The account of what the program did to files: copies, moves, renames, deletions, and the ones that failed |
| `journal/shell-YYYY-MM-DD.jsonl` | The commands you ran in the shell inside the window, with the folder each ran in |

The account and the command history exist so the program can answer "what
happened to this file" and "what did I run here" — the undo, the folder
history, and the history column beside the shell all read them. They are
plain text files. You can read them, and you can delete them; the program
carries on without them.

Recording can be turned off entirely (`journal = false` in
`settings.toml`), and how many days are kept is yours to set
(`journal_days`).

**Passwords are never written down.** A password typed to open an archive
lives in memory for that session only, and is never put in the account,
the settings, or any other file.

## What the program does on your behalf

Some things you ask for reach outside the program, and it is worth being
plain about them:

- **Opening a file** hands it to Windows (or your desktop) to open in
  whatever program is registered for it. That program is then on its own
  terms, not ours.
- **The shell inside the window** runs the shell you chose - PowerShell,
  cmd, bash - which can do anything a shell can do, including reaching the
  network, because you told it to.
- **Right-clicking a file** shows the shell's own context menu, built by
  Windows and by whatever programs you have installed.

None of these send anything to us. We have no server to send it to.

## The Microsoft Store version

If you buy the add-on that removes the opening screen, the purchase is
handled entirely by the Microsoft Store. We never see your payment
details; we receive from Microsoft only the fact that a licence exists,
and only while the program is running. Nothing about the purchase is
written to disk.

Microsoft collects its own diagnostics about Store apps, including crash
reports, under
[Microsoft's privacy statement](https://privacy.microsoft.com/privacystatement).
That is between you and Microsoft; we see only aggregate crash counts and
stack traces with no identity attached to them.

## Children

The program is a file manager. It is not directed at children, and it
collects nothing from anyone.

## Changes

If this policy ever changes, the change will be in this file's history in
the public repository, where anyone can read what it used to say.

## Contact

Questions about this policy: open an issue at
<https://github.com/GarAlex/lost-commander>.
