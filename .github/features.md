# Features

`README.md` is the user-facing reference for keybindings and CLI options. This file lists what sst does.

## Account

- Links as a secondary device by QR code (`sst link`). Linking times out after five minutes without a scan.
- Detects an unlinked device (HTTP 403 on the WebSocket) and prints a relink hint instead of the raw error.

## Chat list

- Lists 1:1 and group threads, most recent first, with a preview of the newest message.
- Marks threads with messages newer than the last-seen mark as unread. The mark survives restarts and is advanced by reading on the phone.
- Folds live events in place: a new message moves its thread to the top without rebuilding the list.
- Contact browser (`n`) for starting a chat with a contact or group that has no messages yet.

## Chat window

- Vim-modal: Normal, Insert and Command modes.
- Message selection with `j`/`k`; the selection ring continues into the staged-attachment bar.
- Reply with quote, edit own messages, delete for everyone (`dd`), react (`:react`). Edits and deletes are refused after Signal's 24-hour window.
- `@Name` mentions with Tab completion, sent as Signal body ranges and rendered back from them.
- Reactions shown inline below each message; delivery and read ticks on own messages.
- Read receipts sent for newly seen messages, to each author, and synced to the user's other devices.
- Disappearing-message timer shown in the header; timer changes shown as labels in the message list.
- Multi-line input with word wrap, bracketed paste and Alt+Enter for newlines.
- Attachments: `:upload <path>` stages files with Tab path completion; `:download` and `:download-all` write files atomically to `$HOME/Downloads/sst/<chat>/`.
- Errors land on the status bar; a failed send keeps the draft, reply target and staged files.

## Live updates

- Receives messages, edits, reactions, deletes and receipts over presage's stream and reconnects when the stream ends.
- Remembers contacts and groups first seen in message traffic, so threads presage does not enumerate still appear.

## Scripting

- `chats`, `contacts`, `print`, `print-last`, `recent` and `watch` with text or NDJSON output. JSON field names are a stable interface.
- `recent` lists messages from others across all chats since a point in time and flags mentions of the user.
- `send` with text, stdin or attachments. A running TUI or `watch` performs the send over the local socket; otherwise `send` takes the instance lock and sends directly.
- The read-only subcommands run beside a running instance and sync first when nothing else holds the lock.
