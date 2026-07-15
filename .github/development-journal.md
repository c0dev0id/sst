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

### OpenBSD SQLite workaround

The bundled SQLCipher build in `presage-store-sqlite` uses OpenSSL 3.x `EVP_MAC` APIs not present in OpenBSD's LibreSSL. Fix: `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` in `.cargo/config.toml` + `default-features = false` on `presage-store-sqlite` to skip the bundled build and link against the system sqlite3 (3.53.2 at `/usr/local/lib/libsqlite3.so`). No cipher support, but we pass `None` as passphrase so this is fine.

### No threads() API in presage store

The presage store has no `threads()` listing method. The chat list is assembled manually: `contacts()` yields `Contact { uuid, name, phone_number, ... }`, `groups()` yields `(GroupMasterKeyBytes, Group { title, ... })`. For each, call `messages(thread, ..)` (returns DESC by timestamp) and take the first item as the last-message preview. Sort all entries by that timestamp.

This is the same approach used by flare and gurk-rs. Both clients have the same limitation.

### Group metadata gap after fresh device linking

Presage calls `upsert_group()` (an internal function) during `receive_messages()` to fetch group metadata from Signal's servers and store it in the `groups` table. This fetch can fail immediately after fresh device linking because GV2 credentials are not yet established. When it fails, messages are still stored in `thread_messages` but the `groups` table stays empty for that group — so `groups()` never returns it.

Affected threads (e.g. Note to Self, any group with no activity after re-linking) will not appear in the chat list until a new message arrives and triggers another `upsert_group()` attempt with working credentials. This is a known limitation shared by all presage-based clients. No workaround is implemented — it would require either direct SQLite access or extending the presage-store-sqlite API.

### ContentExt trait for message timestamps

`Content::timestamp()` is not a method on the struct itself — it's provided by the `presage::store::ContentExt` trait. Must be imported explicitly.

### presage stores sent messages locally before returning from send_message

`Manager::send_message()` calls `save_message()` on the local store synchronously before returning (verified in `presage/src/manager/registered.rs` lines 1058–1064). This means reloading from the store immediately after `send_to_thread` returns will always include the just-sent message — no need to wait for the SyncMessage echo from the server.

Practical consequence: we reload `chat.messages` from DB inside `execute_cmd` right after send. This is reliable regardless of whether the signal receive stream is alive.

### Signal receive stream may close after first live event

The `Stream<Item = Received>` returned by `receive_messages()` and driven in a `spawn_local` task appears to close after delivering the first post-backlog message (possibly because `send_message` opens a new WebSocket and the old receive socket is invalidated). When the stream ends, the `spawn_local` task exits, `tx` is dropped, `mpsc::Receiver::recv()` returns `None`, and `next_signal` silences itself with `std::future::pending()` forever.

Workaround: don't rely on the echo stream for UI updates after send. Reload directly from the local store. Incoming messages from other people are less critical for now but would require reconnection logic to fix properly.

### Cursor byte-offset representation

The input cursor (`ChatState::cursor`) is stored as a byte offset into the UTF-8 `input` string — the native unit for `String::insert` and `String::remove`. Visual column (char count) is derived at render time only, by splitting the `&input[..cursor]` prefix on `'\n'` and counting chars on the last segment. This avoids keeping two representations in sync.

### str::lines() drops trailing newline

`str::lines()` in Rust does not yield a trailing empty element when the string ends with `\n`. Use `str::split('\n')` instead when preserving trailing newlines matters (e.g., rendering a multi-line input field after Shift+Enter).

### Attachment upload pipeline

Attachment handling is split into three phases to keep failures contained:

1. **Staging** (`:upload <path>`): `std::fs::metadata` validates the file exists and is a regular file; MIME type is derived from the extension (no new crate); `StagedAttachment { path, kind, mime, size }` is pushed to `ChatState::staged_attachments`. Staged attachments survive mode changes and persist until the user leaves the chat.
2. **Uploading** (at send time): `upload_staged_attachments` reads each file's bytes, calls `manager.upload_attachment(spec, bytes)`, and sets the GIF flag (`= 8`) manually post-upload because `AttachmentSpec` has no gif field. If any file fails (IO error, 413 over-size, etc.), only that file is removed from staging and the send is aborted — the user can re-add the file from a new path.
3. **Sending**: `Vec<AttachmentPointer>` is included in the `DataMessage.attachments` field. `send_edit` intentionally does not forward attachments (you're changing body text, not re-attaching files).

GIF flag value (8) is defined in `SignalService.proto` `AttachmentPointer.Flags` enum. It is NOT exposed via `AttachmentSpec` or set automatically by presage/libsignal-service-rs — confirmed in `sender.rs`.

### Attachment bar / navigation ring

`selected_message` and `selected_attachment` are mutually exclusive in `ChatState`. Navigation in Normal mode forms a circular ring: oldest message → … → newest message → first staged file → … → last staged file → oldest message (k is fully symmetric). This avoids two separate navigation contexts and lets the user move naturally from reviewing messages to checking/removing queued files.

## Core Features

See `README.md` for the full UX specification. High-level:

- QR-code-based device linking via presage
- Chat list with 1:1 and group chat support (groups always have a title from Signal)
- Chat window with inline reactions, quote-replies, and typing notifications
- `@mention` autocomplete
- Slash command autocomplete (`/reply`, `/react`)
- Message selection mode (Shift+arrow keys) as prerequisite for reply/react
- Async message receiving via presage's `Stream<Item = Received>`
