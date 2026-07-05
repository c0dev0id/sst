# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed
- Group chat sender blocks now show contact names instead of raw UUIDs; for group members not present in the local contact store, a Signal profile fetch is attempted using the profile key from the group roster

### Added
- Bracketed paste mode: pasting multi-line content into the input bar inserts it as a single text block rather than firing individual key events; eliminates accidental sends on multi-line paste

### Added
- Vim-style modal navigation in the chat window: Normal mode (default on open), Insert mode (`i`), Command mode (`:`). Normal mode uses `j`/`k` to select messages, `r` to reply, `e` to edit own messages, `d` to delete, `:` for colon commands, `q`/`←` to return to chat list.
- Delete for everyone: `d` in Normal mode on a selected own message sends a `DataMessage.delete` to the thread (Signal propagates it to all recipients/group members), then removes it from the local store. Non-own messages are rejected with a status bar error.
- Colon command dispatch (`:react <emoji|shortcode>`, `:react`, `:quit`) replaces the old slash command system.

### Fixed
- Starting a second sst instance while one is already running would silently corrupt the Signal session, causing messages to appear sent locally but never arrive on other devices. sst now acquires an exclusive `flock`-based lock on startup and exits immediately with a clear error if another instance holds it.

### Added
- `--contact-list` now fetches Signal profiles for UUID-only contacts (group members not in the phone's address book) using the profile key cached in the local store; resolved names are shown immediately and cached for future lookups

### Changed
- Slash command dispatch refactored into a single registry (`SLASH_COMMANDS` const + `SlashCmd` enum + `parse_slash_cmd()`); each entry declares `needs_selection` and `has_arg` metadata; commands that require a selection now show an error on the status bar instead of silently doing nothing

### Added
- `/react <emoji|shortcode>` slash command: reacts to the selected message; `/react wave` resolves via `emojis` crate shortcode lookup (👋), direct emoji passthrough for non-ASCII input (e.g. `/react ❤️`); sending the same emoji again toggles it off (`remove: true`)
- `/react` (no argument): shows existing reaction counts for the selected message on the status bar
- Reactions rendered inline below each message body: `[2x❤️, 1x👍]` in gray; reactions are loaded on chat open and refreshed on every incoming signal event
- Status bar selection hint now mentions `/react <emoji>` alongside `/reply`

### Fixed
- Signal WebSocket stream now reconnects automatically when presage closes it after delivering the first live event; fixes incoming messages not appearing in TUI after the first one, and `--read-stream` not delivering subsequent messages

### Added
- Phase 2: Signal data layer — syncs messages, persists discovered contacts (16-byte UUID blocks) and group master keys (32-byte blocks) across sessions
- Phase 2: `list_threads` builds chat list from `contacts()` + `groups()` presage APIs, falls back to UUID strings for unsaved contacts
- Phase 2: `request_contacts()` triggers primary device to push its contact list; UUIDs are replaced with real names on subsequent sync
- Phase 3: ratatui TUI shell — chat list view with highlight, scroll, PgUp/PgDn navigation, Q to quit
- Phase 3: Status bar with key hint legend
- Phase 4: chat window — open a thread with Enter, scroll messages with PgUp/PgDn, send with Enter, Shift+Enter for newline, Esc to return to chat list
- Phase 4: chat window shows sender blocks grouped by consecutive sender, with HH:MM timestamp and a `── date ──` separator when gap > 1h
- Phase 4: input bar grows vertically with multi-line content; own messages in cyan, contact/group messages in yellow
- Input bar cursor: inverted-block character at the insert point; ←→↑↓ move the cursor within the text, including across lines
- Message selection: Shift+↑ activates selection at the most recent message and moves toward older; Shift+↓ moves toward newer; Esc clears selection (second Esc returns to chat list). Selected message is highlighted in blue; status bar shows sender, timestamp, and position.
- `/reply <text>` sends a Signal reply to the selected message; quoted author and first line of quoted text are rendered inline above the reply body.
- Read receipts: opening a 1:1 chat sends a READ receipt to the contact for all their messages. Received delivery/read receipts are scanned from the store and shown as `✓` (delivered) or `✓✓` (read) at the end of the last body line of own sent messages.
- Tab completion for slash commands: Tab completes `/reply`, `/quit`, `/react` on a unique prefix match; double Tab shows all matching commands on the status bar.
- Tab completion for `@mentions`: `@<partial>Tab` completes on a unique match from known 1:1 contacts; double Tab shows all candidates on the status bar. Excludes "Note to Self".
- Contact browser (`n` from chat list): shows all synced contacts and known groups regardless of message history; contacts first (alphabetical) then groups separated by a `── groups ──` divider. Allows opening any thread, including ones with no prior messages.

### Fixed
- Chat list is now refreshed from the store whenever you return to it (Esc from chat window, Esc/q from contact browser), so newly started threads appear immediately
- Read receipts now sent for messages that arrive while the chat is already open, not only on initial open
- Chat window: long message lines now word-wrap to the window width instead of being clipped
- Chat list: preview lines now truncate to terminal width with `…` instead of hard-clipping
- WebSocket close error no longer corrupts the TUI: tracing output is redirected to `sst.log` in the data directory when running in TUI mode; `--list` mode continues writing to stderr
- Separate chunk sizes for contact UUID blobs (16 bytes) vs group master key blobs (32 bytes) — previously a single loader used 32-byte chunks, silently merging two contacts into one unusable entry
- "Note to Self" thread now shows by name instead of phone number
- Chat list preview no longer shows "(no messages)" for threads with unread-only content — iterates up to 50 messages to find a previewable one
- Multi-line message previews collapse to a single line with spaces
- Sent message echo now appears in the open chat window immediately; messages are reloaded from the store synchronously after send (presage stores sent messages locally before returning from `send_message`)
- Trailing newline in multi-line input now immediately shows the cursor on the new line (was invisible until next keypress due to `str::lines()` dropping trailing `\n`)
