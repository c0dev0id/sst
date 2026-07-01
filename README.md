# SST (Simple Signal TUI)

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
   [2x❤️, 1x👍]                             ← inline reactions

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

Reactions are shown inline below the message body: `[2x❤️, 1x👍]`.

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
| Tab | Complete slash command, @mention, or emoji shortcode |
| Esc | Clear selection / return to Chat List |

Tab completes immediately on a unique match. With multiple candidates, all matches are shown on the status bar.

### Slash commands

Slash commands are available in the input bar. Commands that require a selected message show an error on the status bar if none is active.

| Command | Requires selection | Action |
|---------|-------------------|--------|
| `/quit` | No | Exit the app |
| `/reply <text>` | Yes | Send `<text>` as a quoted reply |
| `/react <emoji>` | Yes | React to the selected message |
| `/react` | Yes | Show existing reaction counts on the status bar |

`/react` accepts either a raw emoji (`/react ❤️`) or a gemoji shortcode (`/react wave` → 👋). Sending the same emoji twice toggles it off.

Tab-completion for `/react <shortcode>`:
- `/react <Tab>` shows all available shortcodes with their emoji on the status bar
- `/react w<Tab>` narrows to shortcodes starting with `w` (e.g. `wave (👋)  weary (😩)`)
- Completes immediately when the partial matches exactly one shortcode

### @mention completion

`@ali` + Tab completes to `@Alice Wagner ` when it is the only match among known contacts. `@` + Tab lists all candidates on the status bar.

---

## Status bar

Shows key hints by default. After Tab, completion candidates are shown (clears on the next keystroke). While a message is selected, shows sender, timestamp, and position — Tab candidates take priority over the selection info:

```
  [3/17]  Alice Wagner  ·  2026-06-28 09:14  |  /reply <text>↵   /react <emoji>   Shift+↑↓   Esc deselect
```

---

## CLI Usage

In addition to the TUI, `sst` exposes several non-interactive commands for scripting and automation. All CLI modes share the same SQLite database as the TUI — **do not run them concurrently with `sst` or with each other**, as concurrent writes will corrupt the database.

### List chats

```sh
sst --list
```

Syncs new messages, then prints all conversations with a one-line preview to stdout.

### List contacts

```sh
sst --contact-list
```

Syncs the contact list from the primary device, then prints `<uuid> <name>` for every known contact and group to stdout. For contacts with no name (group members not in the phone's address book), a Signal profile fetch is attempted using any cached profile key — resolved names are shown immediately and cached locally for future use.

```
96c9d3f9-fccf-4517-a0a8-f4bf72a63e48 Note to Self
3fa85f64-5717-4562-b3fc-2c963f66afa6 Alice Wagner
7c9e6679-7425-40de-944b-e07fc1f90ae7 Bob Richter
a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 Family Group
```

Groups are identified by a 64-character hex master key instead of a UUID.

### Send a message

```sh
echo "Hello!" | sst --send <UUID|HEX>
printf "Line one\nLine two" | sst --send <UUID|HEX>
```

Reads the message body from stdin and sends it to the given contact (UUID) or group (64-char hex). Trailing newlines are stripped.

### Read chat history

```sh
sst --read <UUID|HEX>
```

Syncs new messages, then prints the full chat history to stdout as JSONL (one JSON object per line):

```json
{"timestamp":"2026-07-01T09:14:00Z","sender_uuid":"3fa85f64-...","sender_name":"Alice Wagner","body":"Hey!"}
```

### Stream incoming messages

```sh
sst --read-stream <UUID|HEX>
```

Connects to Signal and streams new incoming messages from the given thread to stdout as JSONL. Runs until interrupted. Only messages received after startup are emitted — backlog is silently discarded.

```sh
# Forward all incoming messages from Alice to a file:
sst --read-stream 3fa85f64-5717-4562-b3fc-2c963f66afa6 >> alice.jsonl
```

Note: messages you send via `--send` will not appear on `--read-stream` running on the same device — Signal does not echo sent messages back to the originating device.

### Custom database path

All modes accept `--db <path>` to override the default database location:

```sh
sst --db /tmp/test.db --relink
```

---

## Building

```sh
make          # debug build  (target/debug/sst)
make release  # release build
make install  # install release build to ~/.bin/sst
make test     # run test suite
```
