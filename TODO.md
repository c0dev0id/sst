# TODO

## Where we left off

Stream reconnect, CLI flags (--send, --read, --read-stream, --contact-list), and --relink hardening are done.

## Open tasks

### Near-term (input/display polish)
- [ ] Group sender names show raw UUID — resolve to contact name
- [ ] `ThreadEntry.unread` is set on incoming messages but never cleared when a chat is opened; needs a "last-seen timestamp" mechanism persisted to disk
- [ ] `── new ──` unread boundary separator in chat window
- [ ] Typing notifications in status bar
- [ ] Unread visual treatment beyond `*` prefix (bold/color — open design question)

### Reactions
- [x] `/react <shortcode|emoji>` slash command — done
- [x] Reactions rendered inline — done

### File transfer
- [ ] `/upload <path>` slash command: send a local file into the open chat
  - Images (jpg, png, gif, webp, …) → sent as image attachment
  - Everything else → sent as generic file attachment
  - presage API to check: `DataMessage { attachments: vec![…], … }` + how presage handles CDN upload before send
  - Tab-completion on path argument is nice-to-have but non-trivial
- [ ] `/download` slash command: download the attachment from the selected message to `$HOME/Downloads/`
  - Requires an active message selection (Shift+↑)
  - If the selected message has no attachment, show an error on the status bar
  - On success, show the saved local path on the status bar
  - presage API to check: `Manager::get_attachment()` or similar CDN fetch

### Message editing
- [ ] `/edit` slash command: requires a selected own message; pulls its body into the input bar; sending replaces the message on Signal
  - Only works on own messages — show error on status bar if the selected message is from someone else
  - Wire up via `ContentBody::EditMessage { target_sent_timestamp: Some(original_ts), data_message: Some(DataMessage { body: Some(new_text), .. }) }`
  - Send path mirrors `send_to_thread`/`send_message_to_group` — same 1:1 vs group branch
  - After send, call `reload_chat()` so the updated body appears immediately
  - The store may or may not update the original message in-place (presage handles incoming edits via `save_message`); test whether the local store reflects the edit or stores a second entry
  - Register in `SLASH_COMMANDS` with `needs_selection: true, has_arg: false` (arg comes from pre-filled input bar, not the command line)
  - UX: when `/edit` is executed, clear the input bar and populate it with the selected message body; cursor goes to end

### Infrastructure
- [ ] `d` key on chat list to delete thread (with confirmation), per spec
- [ ] `--contact-list`: after printing, walk UUID-only contacts and fetch missing profiles
  - For each contact where `contact_display_name` falls back to a UUID: call `manager.store().profile_key(service_id)` — if `Some(key)`, call `manager.retrieve_profile_by_uuid(uuid, key)` (caches in store); re-print with resolved name
  - `retrieve_profile_by_uuid` checks its own cache first, so calling it in a loop is safe
