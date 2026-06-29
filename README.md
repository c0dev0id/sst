# simple-signal-tui

Terminal UI client for Signal.

**Features:** QR code device linking · typing notifications · read receipts · message reactions · quoted replies · @mention autocomplete · slash command autocomplete

---

## Auth Flow

On startup:

1. Check for a stored auth token.
2. If absent (or `--relink` is passed), display a QR code and wait for it to be scanned in the Signal mobile app.
3. On success, proceed to the Chat List.

> **Note:** The QR code device-linking mechanism depends on how `libsignal-protocol-c` exposes the provisioning flow — this needs investigation before implementation.

---

## Chat List

One line per conversation, sorted most-recent-first. Unread chats are marked with `*`.

**Format:**
- 1:1: `<Full Name>: <truncated last message preview>`
  - `Florian Heß: This is a test message that gets truncated on the right...`
- Group (named): `<Group Name>: <truncated last message preview>`
  - `Weekend Plans: This is the last message, which is also truncated...`
- Group (unnamed): `<First, Names, ...>: <truncated last message preview>`
  - `Florian, Andreas, Dominik: This is the last message, which is also truncated...`

**Keys:**

| Key | Action |
|-----|--------|
| ↑ / ↓ | Navigate chats |
| PgUp / PgDn | Scroll list |
| Return | Open selected chat |
| `d` | Delete chat (confirmation required) |
| `Q` | Quit |

The list scrolls automatically when the cursor reaches the screen edge, keeping one additional entry visible above and below the selection at all times.

---

## Chat Window

Layout (top to bottom):

```
┌─────────────────────────────────┐
│  Florian Heß                    │  ← header: contact or group name
├─────────────────────────────────┤
│                                 │
│  * Florian Heß:                 │  ← message area (scrollable)
│    Hello people!                │
│                                 │
│  * Andreas Schirra:             │
│    Hi Florian!                  │
│                                 │
├─────────────────────────────────┤
│  Florian Heß is typing...       │  ← status bar
├─────────────────────────────────┤
│  >                              │  ← input bar
└─────────────────────────────────┘
```

**Keys:**

| Key | Action |
|-----|--------|
| PgUp / PgDn | Scroll message area |
| Shift+↑ | Activate selection / move selection up (toward older messages) |
| Shift+↓ | Move selection down (toward newer messages); no-op if no selection |
| Escape | Clear selection (first press); return to Chat List (second press) |

---

### Message Area

Oldest messages at the top, newest at the bottom. Consecutive messages from the same sender are grouped under one header.

```
* Florian Heß:
  Hello people!
* Andreas Schirra:
  Hi Florian!
* Stefan Hagen:
  Messages can be long and
  contain line breaks
* Stefan Hagen:
  Messages with reactions have them appended inline. [1x❤, 3x👋]
* Stefan Hagen:
  > Florian Heß:
  > Hello people!
  Replies quote the original with a > prefix.
* Andreas Schirra:
  > Stefan Hagen:
  > Replies will quote the original message
  Nested replies do NOT include the grandparent quote — one level only.
```

Own messages use the same format; the username is colored to distinguish it.

Sent messages show a read receipt indicator:
- `✓` — delivered
- `✓✓` — seen

**Timestamps** appear as inline separators when the gap between consecutive messages exceeds one hour:

```
── 2026-06-26 09:00 ──
* Florian Heß:
  Good morning!
── 2026-06-26 14:35 ──
* Stefan Hagen:
  Afternoon everyone
```

**Unread boundary** is marked with a separator at the first unread message:

```
── new ──
* Florian Heß:
  You missed this
```

**Message selection** (Shift+↑/↓) scrolls automatically when the cursor reaches the screen edge, keeping one additional message visible above and below the selection. ESC clears the selection entirely; the next Shift+↑ starts fresh at the most recent message.

When a message is selected, the status bar shows full message metadata (sender, timestamp, delivery status) instead of the typing notification.

---

### Status Bar

Normally shows typing notifications:

| Scenario | Text |
|----------|------|
| One person | `Florian Heß is typing…` |
| Two people | `Florian and Andreas are typing…` |
| Three or more | `Florian, Andreas, and 3 more are typing…` |

Temporarily overridden by:
- Autocomplete candidates (double-Tab on `@mention` or `/command`)
- Message metadata when a message is selected

Restores to its normal content when the next character is typed or Backspace is pressed.

---

### Input Bar

Always focused. Grows vertically as needed (no line cap).

| Input | Action |
|-------|--------|
| Return | Send message |
| Shift+Return | Insert newline |
| Escape | Clear selection / return to Chat List |
| `@<partial>` Tab | Complete username (unique match; excludes self) |
| `@<partial>` Tab Tab | Show all matches on status bar |
| Shift+↑ | Activate message selection at most recent message |

In a 1:1 chat, `@`Tab always completes the other participant.

---

### Slash Commands (requires message selection)

| Command | Action |
|---------|--------|
| `/reply <text>` | Send `<text>` as a reply to the selected message |
| `/react <emoji-key>` | React to the selected message |
| `/react` | Show existing reactions on the selected message |

Autocomplete follows the same Tab logic as `@mentions` — single Tab completes on a unique match, double Tab lists all candidates on the status bar.

**Supported reaction keys:**

| Key | Emoji |
|-----|-------|
| `heart` | ❤️ |
| `thumbs-up` | 👍 |
| `wave` | 👋 |
| `laugh` | 😄 |

---

## Open Questions

- **Read receipts:** Need to verify what `libsignal-protocol-c` exposes and at what granularity.
- **QR device linking:** Investigate how `libsignal-protocol-c` handles the provisioning flow.
- **Chat list unread:** Visual treatment beyond `*` prefix (bold? color?).
