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
- [ ] `/react <shortcode|emoji>` slash command — sends a reaction to the selected message
  - `/react wave` → resolves via `emojis::get_by_shortcode("wave")` → 👋 (add `emojis` crate)
  - `/react ❤️` → direct emoji, passed through as-is
  - `/react` (no arg) → show existing reactions on the selected message on the status bar
  - `remove` flag: reacting with the same emoji again should toggle it off (`remove: true`)
  - Proto: `DataMessage { reaction: Some(Reaction { emoji, remove, target_author_aci, target_sent_timestamp }), .. }`
- [ ] Reactions rendered inline: `[1x❤️, 3x👋]` appended to last line of message body
  - Reaction messages arrive as `DataMessage` with `reaction` set and empty body — currently filtered out by `load_messages`
  - Load separately, group by `target_sent_timestamp + emoji`, count, attach to matching message

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

### Infrastructure
- [ ] `d` key on chat list to delete thread (with confirmation), per spec
