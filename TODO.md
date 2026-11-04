# TODO

## Open tasks

### Unread tracking (input/display polish)
- [ ] Unread flags live only in memory: opening a chat clears its flag, but nothing survives a restart. Needs a per-thread "last seen" timestamp persisted to disk. The same record would let read receipts go out once per message instead of for the whole thread on every open, and let `SyncMessage.read` from the phone clear the flag.
- [ ] `── new ──` unread boundary separator in chat window
- [ ] Unread visual treatment beyond the `*` prefix (bold/color — open design question)

### Chat window
- [ ] Typing notifications in the status bar (incoming typing events are currently discarded)

### Thread management
- [ ] `d` key on the chat list to delete a thread, with confirmation (chat-window `dd` delete-for-everyone already exists; this is list-level thread deletion)
