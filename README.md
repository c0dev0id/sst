# SST — Simple Signal TUI

A terminal client for Signal. For people who live in the shell and want to read,
send, and script Signal messages without a desktop GUI.

```
 Alice Wagner
────────────────────────────────────────────
 Alice Wagner  09:14
   Hey, are you free tonight?

 You  09:31
   Sure, sounds good!  ✓✓
   [2x❤️, 1x👍]
────────────────────────────────────────────
  j/k navigate   r reply   e edit   d delete   : command   q back
  -- NORMAL --
```

## Features

- **Vim-modal chat** — Normal / Insert / Command modes, colon commands (`:react`, `:quit`).
- **Full message flow** — reply, edit, delete-for-everyone, reactions, read receipts.
- **Contact browser** — start a chat with anyone synced, not just existing threads.
- **Attachments** — stage, upload, and download files.
- **Scriptable CLI** — `send`, `print`, `watch`, and more with text or JSON output.
- **Local SQLite store** — messages and session keys live on disk, no server round-trip to read.

## Getting started

1. Build and install: `make install` (puts the release binary in `~/.bin/sst`).
2. Link to your phone: `sst link` — scan the QR code from
   **Signal → Settings → Linked Devices → Link New Device**.
3. Run `sst` to open the TUI.

Data lives in `~/.local/share/sst` (store, session keys, `sst.log`). Set
`RUST_LOG=debug` for verbose logging.

## Keybindings

Chat list and contact browser:

| Key | Action |
|-----|--------|
| `j` `k` ↑ ↓ | Navigate |
| `l` / Enter / → | Open chat |
| `n` | Contact browser (chat list only) |
| `h` / Esc / ← | Back |
| `Q` | Quit |

Chat window (Normal mode):

| Key | Action |
|-----|--------|
| `j` `k` | Select newer / older message |
| `i` | Compose (Insert mode) |
| `r` / `e` | Reply / edit selected message |
| `dd` | Delete selected own message for everyone |
| `:` | Command mode |
| `h` / `q` / ← | Back to chat list |

Insert mode: Enter sends, Alt+Enter inserts a newline, Tab completes an `@mention`,
Esc discards. Command mode: `:react <emoji>` (raw emoji or gemoji shortcode like
`wave`), `:quit`.

## CLI

`sst` runs non-interactively for scripting. `send`, `print`, and `print-last` can run
alongside the TUI; other subcommands hold an exclusive lock.

| Command | Description |
|---------|-------------|
| `sst link` | Re-link via QR code (wipes existing session) |
| `sst chats` | List threads, most recent first |
| `sst contacts` | List known contacts and groups |
| `sst print <ID>` | Print full chat history |
| `sst print-last [-n N] <ID>` | Print the last N messages |
| `sst watch <ID>` | Stream new incoming messages |
| `sst send <ID> [TEXT] [--attach PATH]…` | Send a message (text or stdin) |

`<ID>` is a contact UUID or 64-char hex group master key (from `sst contacts`).
Add `--format=json` for NDJSON output, or `--db <PATH>` to override the store location.
