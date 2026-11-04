# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed
- `sst link` now prints "Linking successful. Device provisioned." on success, "Linking timed out…" after 5 minutes of no QR scan, and clearly labels underlying failures. Previously the QR appeared and the command exited silently on both success and timeout, giving no feedback either way.
- Signal auth failures (HTTP 403 on the WebSocket upgrade — typical after the device is unlinked from the primary) now surface as: "Signal rejected this device. Run `sst link` to re-provision." with the underlying error printed below. Previously the raw "libsignal-service error: Websocket error: websocket upgrade failed" was shown, which was not actionable.
- CLI redesigned from ad-hoc flags to a consistent subcommand interface: `sst link`, `sst chats`, `sst contacts`, `sst print <uuid>`, `sst print-last [-n N] <uuid>`, `sst watch <uuid>`, `sst send <uuid> [text] [--attach <path>]…`. Running `sst` with no subcommand opens the TUI as before. All subcommands accept `--format=text|json` for machine-readable output. A global `--db <path>` flag overrides the default store location. `sst help` and `sst help <subcommand>` print usage information.
- `send`, `print`, and `print-last` no longer require the exclusive instance lock, so they can run concurrently with the TUI or `sst watch`. This enables scripting patterns such as watching for incoming messages in one process and sending replies from another.

### Fixed
- Edits keep the original message's quote, attachments and disappearing-message timer; previously an edit went out as bare text. Editing or deleting a message older than 24 hours is refused with a hint, since Signal ignores such requests and sst would otherwise diverge from the other devices.
- Timer changes ("Disappearing message timer set to 1 week"), session resets, group changes, view-once media, payments, gift badges, shared contacts and polls now show a label instead of disappearing from the chat, the chat list and the CLI. A call shows once as "Incoming call" instead of one "Call" line per signalling step. View-once media is no longer offered for download.
- Esc in Insert mode cancels the pending edit or reply as the status bar states; previously the edit or reply stayed armed and the next Enter sent it. Editing a photo or file message starts with an empty caption instead of the word "Attachment".
- In group chats the status bar names the selected message's author and the member being replied to; it showed the group's name.
- The selected message and the reply target stay on the same message when the chat reloads after a remote delete or an out-of-order delivery; previously they silently moved to a neighbouring message.
- Unread markers: messages sent from another of your devices, and messages arriving in the chat you are viewing, no longer mark the thread unread. Opening a chat clears its marker, and returning to the chat list keeps the others. A first message from an unlisted contact or group adds the thread under its real name instead of a UUID or not at all.
- Chat list order and previews follow the newest real message in each thread. Receipts, reactions and deleted-message markers no longer count, so opening a chat does not move it to the top, and group members who had only ever sent receipts no longer appear as empty threads. Edits update the preview when they hit the newest message.
- `sst contacts` no longer spins at full CPU when the Signal connection drops before the message queue is drained; it exits with an error instead.
- A person who changes their reaction on a message no longer shows up under both emojis; the new reaction replaces the old one, as on other Signal clients.
- A Signal error while sending, reacting, deleting or opening a chat is shown on the status bar instead of terminating the TUI. A failed send keeps the draft, its reply or edit target and its attachments for a retry.
- A failed attachment upload no longer discards the typed message, reply target or edit in progress; only the failed file is removed from the attachment bar.
- Quote previews in group chats show the quoted author's name instead of a UUID.
- Replying to a photo, file or sticker quotes it as media, so the other side shows a media quote instead of the literal word "Attachment".
- `print`, `print-last` and `watch` resolve group member names the same way the TUI does, including members missing from the contact store; previously those printed as UUIDs.
- Incoming mentions showed a placeholder glyph where the name belongs. Mentions now render as `@Name` in the chat window, quote previews, the chat list preview, and `print`/`watch` output. Edited messages keep their mentions.
- Replies sent from another linked device now show their quote preview; previously only replies received from others did.
- Group chat sender blocks now show contact names instead of raw UUIDs; for group members not present in the local contact store, a Signal profile fetch is attempted using the profile key from the group roster

### Added
- The chat header shows the thread's disappearing-message timer when one is set.
- Mentions: an `@Name` in the message (Tab-completed or typed) is sent as a Signal mention, so the member's client highlights it and notifies them. Applies to new messages, replies and edits. Replies quote the original text with its mentions intact.
- Startup progress output: launching the TUI now prints "Connecting to Signal…", a live message-sync counter, a one-time "Fetching group metadata…" notice when group data is missing, and "Loading chats…" before the interface appears. Previously the terminal showed nothing while the backlog drained, so a slow first sync looked like a hang. Progress is written to stderr and only for the interactive TUI; the scriptable subcommands stay silent.
- Attachment download: `:download` saves attachments from the selected message to `$HOME/Downloads/sst/<chat name>/`; `:download-all` downloads every attachment in the chat. Both commands accept an optional path argument to override the default directory. Filename collisions are resolved by inserting `_N` before the extension (`image.png` → `image_1.png`). Progress is shown on the status bar and updated per-file; files are written atomically so a mid-process kill never leaves a partial file.
- File attachments: `:upload <path>` stages a file (validates immediately via metadata); multiple `:upload` calls accumulate in the attachment bar shown below the status bar. All staged attachments are uploaded and included when the next message is sent or replied. If a file becomes unavailable at send time, only that file is removed and the send is aborted; the user can re-add it and retry
- Path Tab-completion in `:upload`: Tab after a partial path completes to the unique match or lists candidates; directories get a trailing `/` so Tab can chain through multiple levels without retyping
- Paste now works in Command mode: newlines are collapsed to spaces, so a full path can be pasted directly into `:upload <paste>`
- Attachment bar navigation: `j`/`k` in Normal mode cycle through messages and the attachment bar as a continuous ring (last message → first attachment → … → last attachment → first message, symmetric on `k`); `dd` on a selected attachment removes it with the usual two-press confirmation
- Bracketed paste mode: pasting multi-line content into the input bar inserts it as a single text block rather than firing individual key events; eliminates accidental sends on multi-line paste
- Input bar word-wrap: typed text now wraps at word boundaries to fit the terminal width, and reflows automatically on terminal resize; the block cursor tracks the correct visual position across wrapped sub-lines

### Fixed
- Replace Shift+Enter (unreliable on most terminals) with Alt+Enter for newline insertion; both modifiers are now accepted so terminals that do report Shift+Enter continue to work
- Paste: normalize \r\n and bare \r line endings to \n so multi-line pastes from Windows clipboards or certain terminals display and send correctly
- Group messages now include GroupContextV2 in the DataMessage; previously the field was omitted, causing Signal to deliver each group message as an individual DM to every member instead of routing it through the group

### Added
- Vim-style modal navigation in the chat window: Normal mode (default on open), Insert mode (`i`), Command mode (`:`). Normal mode uses `j`/`k` to select messages, `r` to reply, `e` to edit own messages, `d` to delete, `:` for colon commands, `q`/`←` to return to chat list.
- Delete for everyone: `d` in Normal mode on a selected own message sends a `DataMessage.delete` to the thread (Signal propagates it to all recipients/group members), then removes it from the local store. Non-own messages are rejected with a status bar error.
- Colon command dispatch (`:react <emoji|shortcode>`, `:react`, `:quit`) replaces the old slash command system.

### Fixed
- Starting a second sst instance while one is already running would silently corrupt the Signal session, causing messages to appear sent locally but never arrive on other devices. sst now acquires an exclusive `flock`-based lock on startup and exits immediately with a clear error if another instance holds it.

### Added
- `--contact-list` now fetches Signal profiles for UUID-only contacts (group members not in the phone's address book) using the profile key cached in the local store; resolved names are shown immediately and cached for future lookups

### Changed
- Slash command dispatch refactored into a single registry (`SLASH_COMMANDS` const + `SlashCmd` enum + `parse_slash_cmd()`); each entry declares `needs_selection` and `has_arg` metadata; commands that require a selection now show an error on the status bar instead of silently doing nothing

### Added
- `/react <emoji|shortcode>` slash command: reacts to the selected message; `/react wave` resolves via `emojis` crate shortcode lookup (👋), direct emoji passthrough for non-ASCII input (e.g. `/react ❤️`); sending the same emoji again toggles it off (`remove: true`)
- `/react` (no argument): shows existing reaction counts for the selected message on the status bar
- Reactions rendered inline below each message body: `[2x❤️, 1x👍]` in gray; reactions are loaded on chat open and refreshed on every incoming signal event
- Status bar selection hint now mentions `/react <emoji>` alongside `/reply`

### Fixed
- Signal WebSocket stream now reconnects automatically when presage closes it after delivering the first live event; fixes incoming messages not appearing in TUI after the first one, and `--read-stream` not delivering subsequent messages

### Added
- Phase 2: Signal data layer — syncs messages, persists discovered contacts (16-byte UUID blocks) and group master keys (32-byte blocks) across sessions
- Phase 2: `list_threads` builds chat list from `contacts()` + `groups()` presage APIs, falls back to UUID strings for unsaved contacts
- Phase 2: `request_contacts()` triggers primary device to push its contact list; UUIDs are replaced with real names on subsequent sync
- Phase 3: ratatui TUI shell — chat list view with highlight, scroll, PgUp/PgDn navigation, Q to quit
- Phase 3: Status bar with key hint legend
- Phase 4: chat window — open a thread with Enter, scroll messages with PgUp/PgDn, send with Enter, Shift+Enter for newline, Esc to return to chat list
- Phase 4: chat window shows sender blocks grouped by consecutive sender, with HH:MM timestamp and a `── date ──` separator when gap > 1h
- Phase 4: input bar grows vertically with multi-line content; own messages in cyan, contact/group messages in yellow
- Input bar cursor: inverted-block character at the insert point; ←→↑↓ move the cursor within the text, including across lines
- Message selection: Shift+↑ activates selection at the most recent message and moves toward older; Shift+↓ moves toward newer; Esc clears selection (second Esc returns to chat list). Selected message is highlighted in blue; status bar shows sender, timestamp, and position.
- `/reply <text>` sends a Signal reply to the selected message; quoted author and first line of quoted text are rendered inline above the reply body.
- Read receipts: opening a 1:1 chat sends a READ receipt to the contact for all their messages. Received delivery/read receipts are scanned from the store and shown as `✓` (delivered) or `✓✓` (read) at the end of the last body line of own sent messages.
- Tab completion for slash commands: Tab completes `/reply`, `/quit`, `/react` on a unique prefix match; double Tab shows all matching commands on the status bar.
- Tab completion for `@mentions`: `@<partial>Tab` completes on a unique match from the chat's participants; double Tab shows all candidates on the status bar. Excludes "Note to Self".
- Contact browser (`n` from chat list): shows all synced contacts and known groups regardless of message history; contacts first (alphabetical) then groups separated by a `── groups ──` divider. Allows opening any thread, including ones with no prior messages.

### Fixed
- Chat list is now refreshed from the store whenever you return to it (Esc from chat window, Esc/q from contact browser), so newly started threads appear immediately
- Read receipts now sent for messages that arrive while the chat is already open, not only on initial open
- Chat window: long message lines now word-wrap to the window width instead of being clipped
- Chat list: preview lines now truncate to terminal width with `…` instead of hard-clipping
- WebSocket close error no longer corrupts the TUI: tracing output is redirected to `sst.log` in the data directory when running in TUI mode; `--list` mode continues writing to stderr
- Separate chunk sizes for contact UUID blobs (16 bytes) vs group master key blobs (32 bytes) — previously a single loader used 32-byte chunks, silently merging two contacts into one unusable entry
- "Note to Self" thread now shows by name instead of phone number
- Chat list preview no longer shows "(no messages)" for threads with unread-only content — iterates up to 50 messages to find a previewable one
- Multi-line message previews collapse to a single line with spaces
- Sent message echo now appears in the open chat window immediately; messages are reloaded from the store synchronously after send (presage stores sent messages locally before returning from `send_message`)
- Trailing newline in multi-line input now immediately shows the cursor on the new line (was invisible until next keypress due to `str::lines()` dropping trailing `\n`)
