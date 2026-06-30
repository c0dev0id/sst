# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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

### Fixed
- WebSocket close error no longer corrupts the TUI: tracing output is redirected to `sst.log` in the data directory when running in TUI mode; `--list` mode continues writing to stderr
- Separate chunk sizes for contact UUID blobs (16 bytes) vs group master key blobs (32 bytes) — previously a single loader used 32-byte chunks, silently merging two contacts into one unusable entry
- "Note to Self" thread now shows by name instead of phone number
- Chat list preview no longer shows "(no messages)" for threads with unread-only content — iterates up to 50 messages to find a previewable one
- Multi-line message previews collapse to a single line with spaces
- Sent message echo now appears in the open chat window immediately; messages are reloaded from the store synchronously after send (presage stores sent messages locally before returning from `send_message`)
- Trailing newline in multi-line input now immediately shows the cursor on the new line (was invisible until next keypress due to `str::lines()` dropping trailing `\n`)
