# Development Journal

## Software Stack

| Component | Choice | Status |
|-----------|--------|--------|
| Language | TBD | Not decided |
| TUI framework | TBD | Not decided |
| Signal Protocol | libsignal-protocol-c (system-installed) | Committed |
| Auth | QR code flow | Committed |
| Build system | TBD | Not decided |
| Test framework | TBD | Not decided |

## Key Decisions

### Use libsignal-protocol-c
The system already has `libsignal-protocol-c` installed. The implementation must use it rather than rolling a custom Signal Protocol implementation or pulling in a competing library.

## Core Features

See `README.md` for the full UX specification. High-level:

- QR-code-based device linking (Signal protocol)
- Chat list with 1:1 and group chat support
- Chat window with inline reactions, quote-replies, and typing notifications
- `@mention` autocomplete
- Slash command autocomplete (`/reply`, `/react`)
- Message selection mode (Shift+arrow keys) as prerequisite for reply/react
