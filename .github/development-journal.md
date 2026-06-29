# Development Journal

## Software Stack

| Component | Choice | Status |
|-----------|--------|--------|
| Language | Rust | Decided |
| TUI framework | ratatui (crossterm backend) | Decided |
| Signal client | presage | Decided |
| Signal transport | libsignal-service-rs (via presage) | Decided |
| Storage | SQLite via presage built-in | Decided |
| Build system | Cargo | Decided |
| Test framework | Rust built-in (`cargo test`) | Decided |

## Key Decisions

### Use presage, not libsignal-protocol-c

`libsignal-protocol-c` (the installed C library) is purely a crypto layer — Double Ratchet and group session keys only. It has no knowledge of Signal's servers, contacts, groups, device provisioning, or message transport. It cannot be used to build a Signal client on its own.

`presage` (Rust) is a complete high-level Signal client library from the Whisperfish project. It provides:
- `Manager::link_secondary_device()` for QR code device linking — yields a provisioning URL via a `oneshot` channel, then returns a fully registered `Manager` after the mobile app scans it
- `receive_messages() -> Stream<Item = Received>` for async message delivery
- Group metadata: `Group { title: String, members: Vec<Member>, ... }` — title is always present (Signal groups always have names)
- SQLite-backed storage for sessions, contacts, messages, and keys
- Read receipts at per-message granularity (delivered / seen)

`libsignal-service-rs` (also Whisperfish) is the lower-level transport layer that presage wraps. It would require implementing multiple storage traits manually and managing all state transitions. No reason to use it directly for a client application.

Whisperfish (a production SailfishOS Signal client with active users) is built on presage — it's a validated choice.

### Rust as implementation language

`signal-cli` (Java) doesn't run on OpenBSD. Rolling a full Signal server protocol implementation on top of the C `libsignal-protocol-c` would be enormous scope. Rust with presage gives us a complete, maintained Signal client stack that builds natively on OpenBSD via Cargo.

### ratatui for TUI

Standard choice for Rust TUI applications. Crossterm backend works on OpenBSD (ANSI/termios). Provides the widget primitives needed: scrollable lists, text areas, layout constraints for the header/message area/status bar/input bar split.

## Core Features

See `README.md` for the full UX specification. High-level:

- QR-code-based device linking via presage
- Chat list with 1:1 and group chat support (groups always have a title from Signal)
- Chat window with inline reactions, quote-replies, and typing notifications
- `@mention` autocomplete
- Slash command autocomplete (`/reply`, `/react`)
- Message selection mode (Shift+arrow keys) as prerequisite for reply/react
- Async message receiving via presage's `Stream<Item = Received>`
