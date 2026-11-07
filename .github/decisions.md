# Decisions

Context and rationale for choices that are not obvious from the code. Newest additions go at the end of the section they belong to.

## Stack

### Use presage, not libsignal-protocol-c

`libsignal-protocol-c` (the installed C library) is purely a crypto layer: Double Ratchet and group session keys. It has no knowledge of Signal's servers, contacts, groups, device provisioning or message transport, so a client cannot be built on it alone.

`presage` (Rust) is a complete high-level Signal client library from the Whisperfish project. It provides `Manager::link_secondary_device()` for QR code linking, `receive_messages() -> Stream<Item = Received>` for message delivery, group metadata with a title that is always present, SQLite-backed storage for sessions, contacts, messages and keys, and read receipts at per-message granularity.

`libsignal-service-rs` (also Whisperfish) is the transport layer presage wraps. Using it directly would mean implementing several storage traits and all state transitions by hand. Whisperfish, a production SailfishOS client, is built on presage, which validates the choice.

### Rust as implementation language

`signal-cli` (Java) does not run on OpenBSD. A Signal server protocol implementation on top of the C library would be enormous scope. Rust with presage gives a complete, maintained client stack that builds natively on OpenBSD with Cargo.

### ratatui for the TUI

The standard choice for Rust TUIs. The crossterm backend works on OpenBSD (ANSI and termios). It provides the primitives needed: scrollable lists, text areas and layout constraints for the header, message area, status bar and input bar split.

### OpenBSD SQLite workaround

The bundled SQLCipher build in `presage-store-sqlite` uses OpenSSL 3.x `EVP_MAC` APIs that OpenBSD's LibreSSL lacks. `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` in `.cargo/config.toml` plus `default-features = false` on `presage-store-sqlite` skip the bundled build and link the system sqlite3. No cipher support results, which is fine because the store is opened without a passphrase.

### 64 MB runtime thread

`main.rs` spawns the tokio runtime on a manually created thread with a 64 MB stack and drives a `LocalSet` on it. OpenBSD's default 4 MB stack overflows inside the PQ ratchet crypto called from presage. `#[tokio::main]` would lose this.

### presage revision and crate patches

presage is pinned to a git revision because it has no release that matches libsignal-service-rs. The pin moved from `c363731` to `4b81740` (October 2026) to pick up fixes for linking, PNI routing, receive-error propagation and group sync. The revision brought two changes to the build:

- `Received` gained a `DecryptionError(ServiceId)` variant, which sst logs and skips.
- `SyncMessage` became a oneof (`content: Some(sync_message::Content::Sent(..))`), and the retired `END_SESSION` data-message flag disappeared from the proto, so the "Session reset" label went with it.

presage now pulls reqwest with the `rustls` feature, whose crypto provider is AWS-LC, compiled from C at build time. The earlier revision used `ring`. AWS-LC carries OpenBSD-specific code and ships universal pre-generated bindings for non-FIPS builds, so no `cmake` or `bindgen` should be needed, but OpenBSD is not in its tested-platform list; the first manual OpenBSD build after the bump is the verification.

sst's `[patch.crates-io]` mirrors upstream's: only `libsqlite3-sys` (Whisperfish fork of rusqlite). The `curve25519-dalek` and `quickcheck_async` patches were dropped because the crate graph no longer uses them.

## presage behaviour

### No threads() API in presage store

The presage store has no method that lists threads. The chat list is assembled from `contacts()`, `groups()` and two side files, `known_contacts` and `known_groups`, which record contacts and groups seen in message traffic that presage does not enumerate. The startup drain and the live fold write them. For each thread, `messages(thread, ..)` (descending by timestamp) yields the newest displayable row as the preview, and the entries are sorted by that timestamp. flare and gurk-rs assemble their lists the same way.

### Group metadata gap after fresh device linking

presage fetches group metadata during `receive_messages()` and stores it in the `groups` table. The fetch can fail right after linking because GV2 credentials are not yet established. Messages are still stored in `thread_messages`, but the group stays absent from `groups()` until a new message triggers another fetch with working credentials. All presage-based clients share this limitation. No workaround is implemented; one would need direct SQLite access or an extended store API. The revision pinned since October 2026 also syncs groups from the storage service, which may fill the table earlier; this has not been checked.

### ContentExt trait for message timestamps

`Content::timestamp()` is provided by the `presage::store::ContentExt` trait, not by the struct, and must be imported explicitly.

### presage stores sent messages before send_message returns

`Manager::send_message()` saves the message to the local store synchronously before returning. Reloading from the store right after a send therefore always includes the just-sent message; waiting for the SyncMessage echo is unnecessary. `reload_chat` relies on this, which also keeps the UI correct when the receive stream is down.

### The receive stream can end

The `Stream<Item = Received>` from `receive_messages()` can close after delivering a live message, apparently because a send opens a new WebSocket and invalidates the old one. The event loop treats `None` from the stream as "reconnecting": it sleeps 500 ms and calls `receive_messages()` again. UI updates after a send never depend on the echo; they come from the store.

### Rows in a thread that are not messages

The store keeps receipts (including the ones sst sends), NullMessage rows left by remote deletes, call signalling and edit records in the same thread table as messages. Anything that derives "newest message" must skip them; the chat list used to jump to the top on opening a chat because `last_message` took the first row's timestamp, which was sst's own read receipt. `ContentExt::timestamp()` of an EditMessage is the target's timestamp, not the edit's. Two rows with the same millisecond timestamp in one thread overwrite each other (`INSERT OR REPLACE` on `(ts, thread_id)`), and ordering uses the sender's timestamp, so a peer with a skewed clock sorts out of place. Of the call signalling, sst renders only the incoming offer, as "Incoming call", and drops the rest, including the phone's call-event syncs.

### Receipts for group messages live in 1:1 threads

presage derives a receipt's thread from its sender, and a ReceiptMessage carries no group context, so a member's delivery or read receipt for a group message is stored under `Thread::Contact(member)`. `load_receipt_state` therefore scans every roster member's thread for a group, starting at the oldest own message in the group, and skips receipts sst sent itself. Read receipts go to each author individually, which is also how the official clients send them, and every batch is followed by a `SyncMessage.read` to the user's other devices so they clear the unread state. A message counts as delivered or read as soon as one recipient says so, as on Signal Android.

### Linking feedback and relink-needed detection

`Manager::link_secondary_device` sends the provisioning URL over a `oneshot` channel and then awaits the primary device's confirmation. Without a scan it waits forever, so the join of the link and QR futures is wrapped in a 300-second `tokio::time::timeout`, giving three outcomes: success, a linking error, and a timeout message.

presage's `RelinkNecessary` error is emitted only for a PNI registration mismatch, not for the HTTP 403 on the WebSocket upgrade that follows unlinking from the primary. That case surfaces as a websocket handshake error with the status code buried in the chain. `signal::relink_hint(&anyhow::Error)` walks `err.chain()` and matches "403", "Forbidden", "Authorization failed", "Unauthorized" or "please relink" against each level's `Display`; `async_main` then prints a relink message and exits 1. String matching was chosen over downcasting to keep `libsignal_service` types out of `main.rs`.

## Signal semantics

### Mentions are body ranges, not text

A Signal mention is a U+FFFC placeholder in `DataMessage.body` plus a `BodyRange` with `start`/`length` in UTF-16 code units and the mentioned ACI as its value. Receiving clients substitute the current display name at render time. `@Name` therefore exists only in sst's input buffer and on screen: `encode_mentions` converts it to placeholder plus range at send time (matching against the thread's roster, longest name first) and `substitute_mentions` converts back for display. Quotes carry their own `body_ranges`, and so do edits. Offsets must be counted in UTF-16 units, not bytes or chars, or an emoji earlier in the line shifts the highlight on the recipient's client.

### Edits are complete messages

An EditMessage's DataMessage replaces the original in full. Official clients re-send the quote and attachment pointers and carry the thread's expire timer. presage's `restore_thread_timer` fills the timer for DataMessage bodies only, so `send_edit` copies quote, attachments and timer itself. Signal accepts edits and delete-for-everyone for 24 hours; recipients ignore later requests, so sst refuses them up front instead of applying them locally and diverging.

### Last-seen marks

presage keeps no record of what the user has read, so sst keeps one: `last_seen.json` maps each thread to the timestamp of the newest message shown. The chat list derives unread from it, read receipts go out only for messages newer than it, and a `SyncMessage.read` from the phone advances it. A message that arrives late with an older timestamp is treated as seen; that is the price of a per-thread mark instead of per-message state, and the reason expiry of disappearing messages still waits.

### Attachment upload pipeline

Attachment handling is split into three phases to keep failures contained:

1. **Staging** (`:upload <path>`): `std::fs::metadata` validates that the file exists and is regular; the MIME type is derived from the extension (no new crate); a `StagedAttachment { path, kind, mime, size }` is pushed to `ChatState::staged_attachments`. Staged attachments survive mode changes and persist until the user leaves the chat.
2. **Uploading** (at send time): `upload_staged_attachments` reads each file, calls `manager.upload_attachment(spec, bytes)` and sets the GIF flag (`8`, from `AttachmentPointer.Flags`) after the upload because `AttachmentSpec` has no gif field and neither presage nor libsignal-service-rs sets it. If a file fails (IO error, 413 over size), only that file is removed from staging and the send is aborted; the user can re-add it.
3. **Sending**: the `Vec<AttachmentPointer>` goes into `DataMessage.attachments`.

### Attachment download pipeline

`manager.get_attachment(&self, pointer)` downloads and decrypts the whole file into memory before returning, so a kill mid-download writes nothing. Files are written to `path.tmp` and renamed into place; a kill mid-write leaves only an orphaned `.tmp`. `&self` avoids borrow conflicts with the `&mut app` the caller holds for status updates.

`download` takes the terminal so it can redraw between files during `:download-all`. The event loop is blocked while the command awaits, so explicit draws are the only way to show progress without a thread or a channel.

Filename collisions are resolved by `unique_path()`, which inserts `_N` before the extension. The default download directory is `$HOME/Downloads/sst/<thread name>/`; path separators in the thread name are replaced with `_`.

## TUI

### Cursor byte-offset representation

The input cursor is a byte offset into the UTF-8 `input` string, the native unit for `String::insert` and `String::remove`. The visual (line, column) is derived at render time from the prefix before the cursor. Keeping two representations would mean keeping them in sync.

### str::lines() drops a trailing newline

`str::lines()` does not yield a trailing empty element when the string ends with `\n`. `str::split('\n')` is used where a trailing newline must stay visible, such as the input bar.

### Attachment bar and navigation ring

`selected_message` and `selected_attachment` are mutually exclusive in `ChatState`. Navigation in Normal mode forms a ring: oldest message → … → newest message → first staged file → … → last staged file → oldest message, symmetric on `k`. One ring avoids a second navigation context and lets the user move from reviewing messages to checking queued files without a mode switch.

### Errors stay in the TUI

`execute_cmd` errors are shown on the status bar and logged; they never propagate out of the event loop. A send consumes the draft only after the send succeeds, so a failed upload or send leaves text, reply or edit target and files in place.

## CLI and processes

### CLI subcommand design

The old CLI used ad-hoc boolean flags (`--list`, `--send`, `--read-stream`) with inconsistent naming and no machine-readable output. The redesign uses clap subcommands: `link`, `chats`, `contacts`, `print`, `print-last`, `recent`, `watch` and `send`, with a global `--db <path>`. `--format=json` outputs one JSON object per line (NDJSON); `--format=text` is the default. `Format` derives `ValueEnum` and implements `Display` so clap can show the default in `--help`. The JSON field names are an interface other tools (mcpd) parse and are pinned by a test.

Lock modes: the TUI and `watch` require the instance lock and fail if it is held. `chats`, `contacts`, `print`, `print-last` and `recent` take it when free so they can sync first; when another instance holds it they read the store as is, since that instance keeps it current. `send` goes through the send socket and takes the lock only for a direct send when no instance is running.

### Send socket

Two processes must never write ratchet state at the same time, yet a script wants to reply while `sst watch` streams or the TUI is open. The process holding the session (TUI or `watch`) therefore listens on `sst.sock` in the data directory and performs sends on behalf of `sst send`, which is a thin client: one JSON request per connection, one JSON reply, attachment paths made absolute by the client. When no holder is listening, `send` takes the instance lock and sends directly; while a holder is starting up or a short-lived subcommand has the lock, it retries both for up to 30 seconds. The holder owns the lock, so a socket file left by a crash is known to be stale and is replaced on bind. The socket is mode 0600; anyone who can connect can send as the user. A separate daemon process was rejected: it would add a service to manage without removing a code path, since the TUI and `watch` already hold the session.
