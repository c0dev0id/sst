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
- If absent (or `--relink` flag), run the QR code auth flow.
- On success, proceed to Chat List.

### Chat List View
- One line per chat, sorted most-recent-first.
- Format: `<Full Name>: <truncated last message preview>` for 1:1, `Group: <comma-separated first names>: <truncated preview>` for groups.
- Keys: arrow keys to navigate, Return to open, `Q` to quit, `d` to delete (with confirmation prompt).

### Chat Window View
Layout (top to bottom):
1. **Message area** — scrollable thread with per-sender blocks:
   - Reactions appended inline: `[1x❤, 3x👋]`
   - Replies quote the original with `>` prefix; nested replies do NOT include the grandparent quote.
2. **Status bar** — typing notifications (`X is typing…`, `X and Y are typing…`, `X, Y, and N more are typing…`). Also used for autocomplete disambiguation (see below).
3. **Input bar** — single line, grows to two lines on Shift+Return (multi-line input).

### Input Bar Interactions

| Input | Behaviour |
|-------|-----------|
| Return | Send message |
| Shift+Return | Insert newline; input bar expands |
| `@<partial><Tab>` | Complete username (unique match only; excludes self) |
| `@<partial><Tab><Tab>` | Show all matches on status bar |
| Shift+↑/↓ | Activate message selection mode |
| Escape | Clear message selection |

When a message is selected, slash commands activate:
- `/reply <text>` — reply to selected message
- `/react <emoji-key>` — react to selected message (e.g. `thumbs-up`, `laugh`, `heart`, `wave`)
- `/react` with no arg — show existing reactions on the selected message
- `/react <partial><Tab>` — autocomplete emoji key; `/react <Tab>` lists all supported emojis on the status bar

Slash command autocomplete follows the same Tab logic as `@mentions`: single Tab completes unique matches, double Tab shows all candidates on the status bar.

---

## Open Design Questions (unresolved)

- Chat list: how to indicate chats with unread messages.
- Chat window: how to mark the unread boundary.
- Chat window: timestamp display strategy — suggestion in spec is "show timestamp when gap > 1h"; also consider exposing full message metadata in the status bar when a message is highlighted.

---

## Implementation Notes for Future Sessions

- `libsignal-protocol-c` provides the Signal Protocol crypto/session layer; do not reimplement it.
- The TUI layer must handle terminal resize gracefully (message area reflow, status/input bar stay pinned to bottom).
- Autocomplete state is transient UI state — keep it out of the message/session model.
- Message selection state is also transient — a separate highlight cursor that does not affect scroll position.
