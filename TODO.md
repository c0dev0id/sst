# TODO

## Open tasks

### Unread tracking (input/display polish)
- [ ] `ThreadEntry.unread` is set on incoming messages (app.rs) but never cleared when a chat is opened; needs a "last-seen timestamp" mechanism persisted to disk
- [ ] `── new ──` unread boundary separator in chat window
- [ ] Unread visual treatment beyond the `*` prefix (bold/color — open design question)

### Chat window
- [ ] Typing notifications in the status bar (incoming typing events are currently discarded)

### Thread management
- [ ] `d` key on the chat list to delete a thread, with confirmation (chat-window `dd` delete-for-everyone already exists; this is list-level thread deletion)
