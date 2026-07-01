# simple-signal-tui

Terminal UI client for Signal, written in Rust.

**Stack:** [presage](https://github.com/whisperfish/presage) · [ratatui](https://github.com/ratatui/ratatui) · [tokio](https://tokio.rs)

---

## Setup

```
sst [--relink] [--data-dir <path>]
```

On first run (or with `--relink`), a QR code is printed in the terminal. Scan it from **Signal → Settings → Linked Devices → Link New Device**. On success the app proceeds to the Chat List.

Data (SQLite store, session keys, cached contacts) lives in `~/.local/share/sst` by default.

---

## Chat List

One line per conversation, sorted most-recent-first. Unread threads are prefixed with `*`.

```
* Alice Wagner: Hey, are you free tonight?
  Bob Richter: The meeting is pushed to Friday
  Family Group: See you all Sunday!
```

Lines are truncated with `…` when they exceed the terminal width.

| Key | Action |
|-----|--------|
| ↑ / ↓ | Navigate |
| PgUp / PgDn | Scroll |
| Enter | Open chat |
| `n` | Open contact browser (new chat) |
| `Q` | Quit |

---

## Contact Browser

Press `n` from the Chat List to open a full-screen picker of all synced contacts and known groups, regardless of message history. Useful for starting a conversation with someone you haven't messaged yet, or opening a group that went quiet before the device was linked.

```
  Alice Wagner
  Bob Richter
  Carol Brauer
─── groups ────────────────────────────────
  Family Group
  Weekend Plans
```

| Key | Action |
|-----|--------|
| ↑ / ↓ | Navigate |
| PgUp / PgDn | Scroll |
| Enter | Open chat |
| Esc / q | Back to Chat List |

---

## Chat Window

```
 Alice Wagner                               ← header
────────────────────────────────────────────
 Alice Wagner  09:14                        ← sender block
   Hey, are you free tonight?
   I was thinking dinner around 7?

 You  09:31
   Sure, sounds good!  ✓✓

── 2026-06-28 14:00 ──                      ← hour gap separator

 Alice Wagner  14:02
   > You:                                   ← quoted reply
   > Sure, sounds good!
   Perfect, see you then!
────────────────────────────────────────────
  ←→↑↓ cursor  PgUp/PgDn scroll  ...        ← status bar
────────────────────────────────────────────
 > |                                        ← input bar
```

Long lines are word-wrapped to the terminal width.

### Message area

| Key | Action |
|-----|--------|
| PgUp / PgDn | Scroll up / down |
| Shift+↑ | Select most recent message / move selection toward older |
| Shift+↓ | Move selection toward newer (no-op when at newest) |
| Esc | Clear selection (first press); return to Chat List (second press) |

Consecutive messages from the same sender are grouped under one header block. An `── date time ──` separator is inserted when the gap between messages exceeds one hour.

Own sent messages show a receipt indicator on the last line:
- `✓` — delivered
- `✓✓` — read

### Input bar

Always focused. Grows vertically as content requires (no line cap). A block cursor shows the insert position.

| Key | Action |
|-----|--------|
| Enter | Send message |
| Shift+Enter | Insert newline |
| ← / → | Move cursor left / right |
| ↑ / ↓ | Move cursor up / down (multi-line) |
| Backspace | Delete character left of cursor |
| Tab | Complete slash command or @mention (unique match) |
| Tab Tab | Show all completion candidates on status bar |
| Esc | Clear selection / return to Chat List |

### Slash commands

`/quit` exits the app from within a chat.

`/reply <text>` sends `<text>` as a quoted reply to the currently selected message (requires Shift+↑ to select first). The quoted author and first line are shown inline above the reply body.

Tab-completion applies to slash commands: `/r` + Tab completes to `/reply ` when it is the only match; `/` + Tab Tab lists all commands on the status bar.

### @mention completion

`@ali` + Tab completes to `@Alice Wagner ` when it is the only match among known contacts. `@` + Tab Tab lists all candidates on the status bar.

---

## Status bar

Shows key hints by default. Overridden by autocomplete candidates after a double-Tab (clears on the next keystroke or Backspace). While a message is selected, shows sender, timestamp, and position:

```
  [3/17]  Alice Wagner  ·  2026-06-28 09:14  |  /reply <text>↵   Shift+↑↓   Esc deselect
```

---

## Building

```sh
make          # debug build  (target/debug/sst)
make release  # release build
make install  # install release build to ~/.bin/sst
make test     # run test suite
```
