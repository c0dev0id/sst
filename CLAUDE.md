# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

Pre-implementation. The full specification lives in `README.md`. Stack is decided; no code written yet.

**Stack:** Rust · presage (Signal client) · ratatui (TUI) · Cargo

---

## Application Architecture (from spec)

Two main views with a shared authentication pre-flight:

### Auth Flow
- On startup, check presage's SQLite store for a stored registration.
- If absent (or `--relink` flag), call `Manager::link_secondary_device()` — it sends a provisioning URL via a `oneshot` channel; render that URL as a terminal QR code and wait. On scan, presage returns a registered `Manager`. Proceed to Chat List.

### Chat List View
- One line per chat, sorted most-recent-first. Unread chats prefixed with `*`.
- Format: `<Full Name>: <truncated preview>` for 1:1; `<Group Name>: <truncated preview>` for groups (groups always have a title — no unnamed-group fallback needed).
- Keys: ↑/↓ to navigate, PgUp/PgDn to scroll, Return to open, `Q` to quit, `d` to delete (with confirmation).
- List scrolls automatically at screen edges, keeping one entry of context above/below selection.

### Chat Window View
Layout (top to bottom):
1. **Header** — contact or group name.
2. **Message area** — scrollable thread, oldest at top, newest at bottom. Per-sender blocks with grouped consecutive messages.
3. **Status bar** — typing notifications; overridden by autocomplete hints or message metadata during selection.
4. **Input bar** — always focused; grows vertically without line cap.

### Message Rendering
- Reactions appended inline: `[1x❤, 3x👋]`
- Replies quote the original with `>` prefix; nested replies do NOT include the grandparent quote (one level only).
- Own messages identical in format; username is colored to distinguish.
- Sent messages show `✓` (delivered) / `✓✓` (seen) read receipt indicator.
- Timestamp separator `── 2026-06-26 09:00 ──` when gap between consecutive messages > 1h.
- Unread boundary shown as `── new ──` separator.

### Input Bar Interactions

| Input | Behaviour |
|-------|-----------|
| Return | Send message |
| Shift+Return | Insert newline; input bar grows |
| Escape | Clear message selection (first press); return to Chat List (second press) |
| `@<partial>Tab` | Complete username (unique match only; excludes self) |
| `@<partial>Tab Tab` | Show all matches on status bar |
| Shift+↑ | Activate selection at most recent message / move selection up |
| Shift+↓ | Move selection down (no-op when no selection active) |

### Message Selection
- Oldest messages are at top; Shift+↑ moves toward older messages, Shift+↓ toward newer.
- No wrapping — Shift+↓ at the most recent message does nothing.
- ESC clears selection entirely; next Shift+↑ starts fresh at most recent.
- Message area scrolls automatically at edges during selection, keeping one message of context visible.
- While selection is active: status bar shows full message metadata (sender, timestamp, delivery status) instead of typing notification.

### Slash Commands (require active selection)

| Command | Action |
|---------|--------|
| `/reply <text>` | Send `<text>` as a reply to the selected message |
| `/react <emoji-key>` | React to the selected message |
| `/react` | Show existing reactions on selected message |

Slash command autocomplete follows the same Tab logic as `@mentions`: single Tab completes on unique match, double Tab lists all candidates on status bar. Status bar restores on next keystroke or Backspace.

---

## Open Design Questions

- **Chat list unread:** Visual treatment beyond `*` prefix (bold? color?).

---

## Implementation Notes

- presage's `receive_messages()` returns `Stream<Item = Received>` — drive this from the main async loop and forward events to the TUI via a channel.
- Autocomplete state is transient UI state — keep it out of the message/session model.
- Message selection state is also transient — a separate highlight cursor that does not affect scroll position.
- The TUI layer must handle terminal resize gracefully (message area reflows; status/input bars stay pinned to bottom). ratatui redraws on each frame; SIGWINCH triggers a resize event via crossterm.
- Do not use `libsignal-protocol-c` (the installed C library) — it is crypto-only and irrelevant given presage.
