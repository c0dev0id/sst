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

### Infrastructure
- [ ] `d` key on chat list to delete thread (with confirmation), per spec
