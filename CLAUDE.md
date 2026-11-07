# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Authoritative sources

- **`README.md`** — user-facing spec: TUI keybindings, modal design, CLI subcommands, on-disk layout. Treat as ground truth for UX and CLI surface.
- **`.github/development-journal.md`** — stack rationale and hard-won gotchas: OpenBSD SQLite workaround, presage store limitations, ratchet/receive-stream quirks, attachment pipeline, CLI redesign notes. Read before touching anything Signal-related.
- **`CHANGELOG.md`** — what shipped when. Keep updated per user's global rules.
- **`TODO.md`** — open work items. When a feature ships, remove its entry (don't leave it marked done) and update any wording that no longer matches the implemented mechanism, so the list keeps reflecting only genuinely open work.

This file is intentionally short: everything below is either the module map (which is not written down elsewhere) or a warning about landmines the journal doesn't cover.

## Build & test

```
make            # cargo build (target/debug/sst)
make release    # cargo build --release
make test       # cargo test
make install    # install release binary to ~/.bin/sst
```

CI (`.github/workflows/ci.yml`) runs check, clippy with warnings denied and the tests on Linux for every push; keep clippy clean. OpenBSD builds stay manual.

`cargo test <name>` runs a single test. Tests live in `#[cfg(test)]` modules next to the code they cover: the mention codec, reaction fold and thread ids in `signal.rs`, the socket protocol in `ipc.rs`, JSON records and `--since` in `main.rs`, cursor, completion, colon commands and the thread-list fold in `app.rs`, wrapping in `ui.rs`. Add a test with every piece of pure logic.

**OpenBSD build environment:** `.cargo/config.toml` sets `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` and `presage-store-sqlite` is pulled with `default-features = false`. This forces linking against system `libsqlite3` (LibreSSL-compatible) instead of the bundled SQLCipher build (which needs OpenSSL 3.x `EVP_MAC` APIs missing from LibreSSL). Do not remove either — see dev journal "OpenBSD SQLite workaround".

**Stack size:** `main.rs` spawns the tokio runtime on a manually-created 64 MB thread. OpenBSD's default 4 MB stack blows up inside the PQ ratchet crypto called from presage. Do not switch to `#[tokio::main]`.

## Module map

Seven files under `src/`. Read in this order for a first pass:

- **`main.rs`** (~700 lines) — Entry point, clap CLI, provisioning, instance lock, tokio runtime bootstrap. Dispatches to either `app::run` (TUI) or one of the `Cmd::{Link, Chats, Contacts, Print, PrintLast, Recent, Watch}` handlers inline; `send` is handled before the store opens (see `ipc.rs`). Locking: the TUI and `watch` require the instance lock; `chats`/`contacts`/`print`/`print-last`/`recent` take it if free (and sync) and otherwise read the store as is; `send` uses the socket or takes the lock for a direct send.
- JSON output is an interface mcpd parses: one builder per record in `main.rs` (`chat_record`, `contact_record`, `message_record`, `recent_record`), key sets pinned by a test and listed in the README. Renaming a field is a breaking change.
- **`signal.rs`** (~1400 lines) — All presage interaction lives here. Public API: `relink_hint`, `parse_thread_id`, `is_own`, `sync_contacts`, `sync`, `connect`, `live_receipt`, `is_message_in`, `extract_update`, `remember_contact`, `list_threads`, `recent_messages`, `names_for_messages`, `list_all_contacts`, `fetch_missing_profiles`, `or_uuid`, `message_body`, `is_displayable`, `expire_timer`, `message_text`, `mention_names`, `thread_title`, `name_or_uuid`, `quote_of`, `lookup_contact_name`, `load_sender_names`, `load_messages_and_reactions`, `delete_for_everyone`, `send_reaction`, `outside_edit_window`, `send_edit`, `send_to_thread`, `send_text`, `send_reply`, `message_quote`, `own_service_id`, `own_aci`, `load_receipt_state`, `send_read_receipts`. Types: `ReactionMap`, `ThreadEntry`, `MessageUpdate`, `RecentMessage`.
- **`mentions.rs`** (~300 lines) — The mention codec: `encode_mentions` turns `@Name` into a U+FFFC placeholder plus a `BodyRange` with UTF-16 offsets, `substitute_mentions` renders the placeholder back as `@Name`. Pure; fully covered by tests.
- **`attachments.rs`** (~200 lines) — Staging a local file (`stage_attachment`, MIME by extension), `upload_staged_attachments`, `message_attachments` (view-once excluded), filename derivation and the atomic `download_attachment`.
- **`ipc.rs`** (~200 lines) — The local send socket. The session holder (TUI or `watch`) calls `serve` and receives `SendJob`s on a channel; `sst send` calls `request_send` and falls back to a direct send under the lock when nobody listens. `perform` carries a request out with the session. One newline-delimited JSON request and reply per connection.
- **`app.rs`** (~1500 lines) — TUI state machine and event loop. `App` owns `threads`, `chat: Option<ChatState>`, view enum, and contact-browser state. `ChatState` carries the transient per-chat state (messages, cursor, mode, selection, reactions, staged attachments). Key handling is dispatched by view (`on_key_list`, `on_key_chat`, `on_key_contacts`) and, in the chat window, by mode (`normal_mode_key`, `insert_mode_key`, `command_mode_key`). `on_key` returns an `AppCmd`; the async event loop passes it to `execute_cmd`, which dispatches to one function per command (`open_chat_from_store`, `send_draft`, `run_colon_command`, `download`, …) that does the Signal I/O and mutates `App`. An error from `execute_cmd` goes to `App::show_error`, never out of the loop. `App.status` is the one transient status line for all views; `on_key` clears it on every key except Tab.
- **`ui.rs`** (~700 lines) — Pure ratatui rendering. No mutation of `App` beyond the widget-state fields ratatui writes back into (`ListState` selection, `viewport_top_msg`). Anything else is a bug.

## Event loop (in `app::run`)

```
tokio::select! {
    event = events.next()              => on_key / on_paste → execute_cmd
    event = next_or_pending(&mut stream) => on_live_event: fold into thread list; receipt → fold into ticks; message in open thread → load_chat_view
    Some(job) = send_jobs.recv()         => handle_send_job: a send from `sst send`, then shown like a live event
}
```

- Crossterm events come from `EventStream`.
- Signal events come from `Manager::receive_messages() -> Stream<Item = Received>`. The stream can and will die (see dev journal "Signal receive stream may close after first live event") — when it does, `next_or_pending` returns `None`, we sleep 500 ms and attempt `receive_messages()` again. `signal_stream` is `Option<Pin<Box<...>>>`; `None` means "reconnecting".
- A panic hook restores the terminal (raw mode off, alt-screen out, bracketed paste off) so an unexpected panic doesn't corrupt the user's shell. Do not remove.
- `ui.rs` runs synchronously inside `terminal.draw(...)`; `execute_cmd` runs outside it and awaits Signal I/O. Keep Signal calls out of the draw path.
- The open chat is reloaded (`load_chat_view`) only for a message in that thread (`signal::is_message_in`). A live receipt (`signal::live_receipt`) folds straight into the delivered/read sets without touching the store. Typing indicators do nothing.

## Commands are colon commands, not slash commands

The chat window is **vim-modal** (Normal / Insert / Command). Commands are typed as `:react`, `:upload`, `:download`, `:download-all`, `:quit` — registered in `COLON_COMMANDS` and parsed by `parse_colon_cmd` into `ColonCmd`. **Ignore the old spec text about `/react` slash commands and Shift+↑ selection** — those were superseded by the vim-style redesign. `README.md` is current; do not resurrect the slash-command flow.

Newline in Insert mode is **Alt+Enter** (Shift+Enter is not reliably distinguishable from Enter on most terminals — kitty-protocol or `modifyOtherKeys` required). Both are accepted so terminals that do report Shift+Enter still work.

Delete-for-everyone is `dd` (two-press confirmation, guarded by `pending_d`).

## Data layout

`~/.local/share/sst/` (from `directories::ProjectDirs`) holds:

- `db` (+ `db-wal`, `db-shm`) — presage SqliteStore
- `sst.log` — tracing output (`RUST_LOG=debug` or `RUST_LOG=presage=debug` for more)
- `sst.lock` — `fd_lock` exclusive lock for interactive/session-holding subcommands
- `sst.sock` — send socket, served by the TUI or `sst watch` while running; a stale file is replaced on the next start
- `known_contacts`, `known_groups` — contacts and groups seen in message traffic, which presage does not enumerate; written by the startup drain and by the live fold

`--db <path>` overrides the store location; the log and lock still go next to it (parent directory).

Pre-1.0.0: no migration code. If the on-disk format changes, `sst link` wipes and re-links.

## Gotchas not fully covered by the dev journal

- **`Content::timestamp()`** requires `use presage::store::ContentExt;` — it's a trait method, not intrinsic to `Content`.
- **`str::lines()` drops a trailing empty element** when the string ends with `\n`. Use `split('\n')` when rendering the input bar or anywhere trailing-newline visibility matters.
- **`selected_message` and `selected_attachment` are mutually exclusive** in `ChatState`. Navigation forms a circular ring: oldest message → newest → first staged file → last staged file → oldest message. Don't add a second selection cursor; extend the ring.
- **Cursor is a byte offset** into the UTF-8 input string. Visual (line, column) is derived at render time via `cursor_line_col`. Keep it that way — two representations will desync.
- **Groups always have a title** in Signal; no unnamed-group fallback is needed. But group metadata may be missing right after `sst link` (see journal "Group metadata gap after fresh device linking") — this is a presage limitation, not a bug in this code.
- **A thread's rows are not all messages.** presage stores receipts (including the ones sst sends), NullMessage rows from remote deletes, call signalling and edit records in the thread table. Anything deriving "newest message" or "display list" must go through `message_body` emptiness, as `last_message` and `load_messages_and_reactions` do. `ContentExt::timestamp()` of an EditMessage is the *target's* timestamp.
- **Group receipts live in members' 1:1 threads.** A ReceiptMessage has no group context, so presage files it under `Thread::Contact(sender)`. `load_receipt_state` scans roster members' threads for groups; `send_read_receipts` sends to each author and syncs `SyncMessage.read` to own devices.
- **Mentions are not text.** `@Name` exists only in the input buffer and on screen. On the wire a mention is U+FFFC in the body plus a `BodyRange` with UTF-16 offsets. `mentions.rs` is the only module that knows this; `message_body` takes a name resolver for that reason. Never display a raw `DataMessage.body`.
- **`Manager::send_message()` writes to the local store synchronously before returning** — after send, reload messages from the DB directly rather than waiting for the SyncMessage echo. This is what `reload_chat` does.

## What Claude should not do here

- Do not touch the on-disk schema or write migration code (pre-1.0.0).
- Do not add `libsignal-protocol-c` (the installed C lib) — it's crypto-only and irrelevant given presage.
- Do not switch off the manual thread + LocalSet setup in `main.rs` — the runtime shape is load-bearing on OpenBSD.
- Do not re-introduce slash commands (`/react` etc.). The current dispatch is colon-mode.
- Do not put Signal I/O inside `ui::draw`. Draw is sync; Signal calls belong in `execute_cmd`.
