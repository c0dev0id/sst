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
- [ ] `/react <emoji>` slash command — Tab-completes but doesn't send yet
- [ ] Reactions rendered inline: `[1x❤, 3x👋]` appended to last line of message body

### File upload
- [ ] `/upload <path>` slash command: send a local file into the open chat
  - Images (jpg, png, gif, webp, …) → sent as image attachment
  - Everything else → sent as generic file attachment
  - presage API to check: `DataMessage { attachments: vec![…], … }` + how presage handles CDN upload before send
  - Tab-completion on path argument is nice-to-have but non-trivial

### Infrastructure
- [ ] `d` key on chat list to delete thread (with confirmation), per spec
