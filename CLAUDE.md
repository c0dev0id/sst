# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Authoritative sources

- **`README.md`** — user-facing spec: TUI keybindings, modal design, CLI subcommands, on-disk layout. Treat as ground truth for UX and CLI surface.
- **`.github/development-journal.md`** — stack rationale and hard-won gotchas: OpenBSD SQLite workaround, presage store limitations, ratchet/receive-stream quirks, attachment pipeline, CLI redesign notes. Read before touching anything Signal-related.
- **`CHANGELOG.md`** — what shipped when. Keep updated per user's global rules.
- **`TODO.md`** — open work items.

This file is intentionally short: everything below is either the module map (which is not written down elsewhere) or a warning about landmines the journal doesn't cover.

## Build & test

```
make            # cargo build (target/debug/sst)
make release    # cargo build --release
make test       # cargo test
make install    # install release binary to ~/.bin/sst
```

`cargo test <name>` runs a single test. There are no tests yet — a suite is worth adding when non-trivial pure logic accumulates (cursor helpers, thread-id parsing, emoji resolution, reaction map folding).

**OpenBSD build environment:** `.cargo/config.toml` sets `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` and `presage-store-sqlite` is pulled with `default-features = false`. This forces linking against system `libsqlite3` (LibreSSL-compatible) instead of the bundled SQLCipher build (which needs OpenSSL 3.x `EVP_MAC` APIs missing from LibreSSL). Do not remove either — see dev journal "OpenBSD SQLite workaround".

**Stack size:** `main.rs` spawns the tokio runtime on a manually-created 64 MB thread. OpenBSD's default 4 MB stack blows up inside the PQ ratchet crypto called from presage. Do not switch to `#[tokio::main]`.

## Module map

Four files under `src/`. Read in this order for a first pass:

- **`main.rs`** (~500 lines) — Entry point, clap CLI, provisioning, instance lock, tokio runtime bootstrap. Dispatches to either `app::run` (TUI) or one of the `Cmd::{Link, Chats, Contacts, Print, PrintLast, Watch, Send}` handlers inline. `parse_thread_id` accepts a UUID (contact) or 64-char hex (group master key). The `send`/`print`/`print-last` subcommands intentionally skip the exclusive lock; all others hold it.
- **`signal.rs`** (~1100 lines) — All presage interaction lives here. Public API: `connect`, `sync`, `sync_contacts`, `list_threads`, `list_all_contacts`, `load_messages`, `load_messages_and_reactions`, `load_receipt_state`, `load_sender_names`, `send_to_thread`, `send_edit`, `send_reaction`, `send_read_receipt`, `delete_for_everyone`, `stage_attachment`, `upload_staged_attachments`, `download_attachment`, `fetch_missing_profiles`, `extract_update`, `message_body`, `fmt_ts_long`. Types: `ThreadEntry`, `MessageUpdate`, `SyncState`, `StagedAttachment`, `ReactionMap`.
- **`app.rs`** (~1300 lines) — TUI state machine and event loop. `App` owns `threads`, `chat: Option<ChatState>`, view enum, and contact-browser state. `ChatState` carries the transient per-chat state (messages, cursor, mode, selection, reactions, staged attachments). Key handling is dispatched by view (`on_key_list`, `on_key_chat`, `on_key_contacts`) and by mode (`Normal`/`Insert`/`Command(String)`). `on_key` returns an `AppCmd`; the async event loop passes it to `execute_cmd` which does the actual Signal I/O and mutates `App`.
- **`ui.rs`** (~640 lines) — Pure ratatui rendering. No mutation of `App` beyond the widget-state fields ratatui writes back into (`ListState` selection, `viewport_top_msg`). Anything else is a bug.

## Event loop (in `app::run`)

```
tokio::select! {
    event = events.next()              => on_key / on_paste → execute_cmd
    event = next_or_pending(&mut stream) => on_signal + reload chat if open
}
```

- Crossterm events come from `EventStream`.
- Signal events come from `Manager::receive_messages() -> Stream<Item = Received>`. The stream can and will die (see dev journal "Signal receive stream may close after first live event") — when it does, `next_or_pending` returns `None`, we sleep 500 ms and attempt `receive_messages()` again. `signal_stream` is `Option<Pin<Box<...>>>`; `None` means "reconnecting".
- A panic hook restores the terminal (raw mode off, alt-screen out, bracketed paste off) so an unexpected panic doesn't corrupt the user's shell. Do not remove.
- `ui.rs` runs synchronously inside `terminal.draw(...)`; `execute_cmd` runs outside it and awaits Signal I/O. Keep Signal calls out of the draw path.

## Commands are colon commands, not slash commands

The chat window is **vim-modal** (Normal / Insert / Command). Commands are typed as `:react`, `:upload`, `:download`, `:download-all`, `:quit` — registered in `COLON_COMMANDS` and parsed by `parse_colon_cmd` into `ColonCmd`. **Ignore the old spec text about `/react` slash commands and Shift+↑ selection** — those were superseded by the vim-style redesign. `README.md` is current; do not resurrect the slash-command flow.

Newline in Insert mode is **Alt+Enter** (Shift+Enter is not reliably distinguishable from Enter on most terminals — kitty-protocol or `modifyOtherKeys` required). Both are accepted so terminals that do report Shift+Enter still work.

Delete-for-everyone is `dd` (two-press confirmation, guarded by `pending_d`).

## Data layout

`~/.local/share/sst/` (from `directories::ProjectDirs`) holds:

- `db` (+ `db-wal`, `db-shm`) — presage SqliteStore
- `sst.log` — tracing output (`RUST_LOG=debug` or `RUST_LOG=presage=debug` for more)
- `sst.lock` — `fd_lock` exclusive lock for interactive/session-holding subcommands
- `own_aci` — cached local ACI UUID, written by `signal::connect`

`--db <path>` overrides the store location; the log and lock still go next to it (parent directory).

Pre-1.0.0: no migration code. If the on-disk format changes, `sst link` wipes and re-links.

## Gotchas not fully covered by the dev journal

- **`Content::timestamp()`** requires `use presage::store::ContentExt;` — it's a trait method, not intrinsic to `Content`.
- **`str::lines()` drops a trailing empty element** when the string ends with `\n`. Use `split('\n')` when rendering the input bar or anywhere trailing-newline visibility matters.
- **`selected_message` and `selected_attachment` are mutually exclusive** in `ChatState`. Navigation forms a circular ring: oldest message → newest → first staged file → last staged file → oldest message. Don't add a second selection cursor; extend the ring.
- **Cursor is a byte offset** into the UTF-8 input string. Visual (line, column) is derived at render time via `cursor_line_col`. Keep it that way — two representations will desync.
- **Groups always have a title** in Signal; no unnamed-group fallback is needed. But group metadata may be missing right after `sst link` (see journal "Group metadata gap after fresh device linking") — this is a presage limitation, not a bug in this code.
- **`Manager::send_message()` writes to the local store synchronously before returning** — after send, reload messages from the DB directly rather than waiting for the SyncMessage echo. This is what `reload_chat` does.

## What Claude should not do here

- Do not touch the on-disk schema or write migration code (pre-1.0.0).
- Do not add `libsignal-protocol-c` (the installed C lib) — it's crypto-only and irrelevant given presage.
- Do not switch off the manual thread + LocalSet setup in `main.rs` — the runtime shape is load-bearing on OpenBSD.
- Do not re-introduce slash commands (`/react` etc.). The current dispatch is colon-mode.
- Do not put Signal I/O inside `ui::draw`. Draw is sync; Signal calls belong in `execute_cmd`.
