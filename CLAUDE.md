# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

Pre-implementation. The full specification lives in `README.md`. No build system, test framework, or language has been committed to yet.

Known constraint: **libsignal-protocol-c is already installed** on the system — use it.

---

## Application Architecture (from spec)

Two main views with a shared authentication pre-flight:

### Auth Flow
- On startup, check for a stored auth token.
- If absent (or `--relink` flag), run the QR code auth flow (display QR, wait for Signal mobile app to scan).
- On success, proceed to Chat List.
- **Implementation note:** The provisioning/device-linking mechanism in `libsignal-protocol-c` needs investigation before the auth flow can be implemented.

### Chat List View
- One line per chat, sorted most-recent-first. Unread chats prefixed with `*`.
- Format: `<Full Name>: <truncated preview>` for 1:1; `<Group Name>: <truncated preview>` for named groups; `<First, Names>: <truncated preview>` for unnamed groups.
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

- **QR auth flow:** How does `libsignal-protocol-c` expose device provisioning? Needs investigation.
- **Read receipts:** What granularity does `libsignal-protocol-c` expose? Spec assumes per-message delivered/seen.
- **Chat list unread:** Visual treatment beyond `*` prefix (bold? color?).

---

## Implementation Notes for Future Sessions

- `libsignal-protocol-c` provides the Signal Protocol crypto/session layer; do not reimplement it.
- The TUI layer must handle terminal resize gracefully (message area reflows; status/input bars stay pinned to bottom).
- Autocomplete state is transient UI state — keep it out of the message/session model.
- Message selection state is also transient — a separate highlight cursor that does not affect scroll position.
