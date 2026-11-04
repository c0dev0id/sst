# TODO

## Open tasks

### Unread tracking (input/display polish)
- [ ] Unread flags live only in memory: opening a chat clears its flag, but nothing survives a restart. Needs a per-thread "last seen" timestamp persisted to disk. The same record would let read receipts go out once per message instead of for the whole thread on every open, and let `SyncMessage.read` from the phone clear the flag.
- [ ] `── new ──` unread boundary separator in chat window
- [ ] Unread visual treatment beyond the `*` prefix (bold/color — open design question)

### Chat window
- [ ] Typing notifications in the status bar (incoming typing events are currently discarded)

### Signal parity
- [ ] Delivery receipts: sst never sends `ReceiptMessage.DELIVERY`. The phone covers delivery state for the account, so impact is low, but linked desktop clients do send them.
- [ ] Disappearing messages are never expired locally. Doing it right needs the per-message read time, which is the same persisted "last seen" record as unread tracking.
- [ ] `Received::Contacts` arriving mid-session is ignored; names refresh on restart only.
- [ ] Signal caps edits at 10 per message; sst enforces the 24-hour window only.
- [ ] `print --format=json` carries no quote, reactions or attachments; `print` does not mark edited messages.

### Thread management
- [ ] `d` key on the chat list to delete a thread, with confirmation (chat-window `dd` delete-for-everyone already exists; this is list-level thread deletion)
