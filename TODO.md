# TODO

## Open tasks

### Unread tracking (input/display polish)
- [ ] `── new ──` unread boundary separator in chat window
- [ ] Unread visual treatment beyond the `*` prefix (bold/color — open design question)

### Chat window
- [ ] Typing notifications in the status bar (incoming typing events are currently discarded)

### Signal parity
- [ ] Delivery receipts: sst never sends `ReceiptMessage.DELIVERY`. The phone covers delivery state for the account, so impact is low, but linked desktop clients do send them.
- [ ] Disappearing messages are never expired locally. The per-thread last-seen mark is not enough: expiry needs the read time of each message.
- [ ] `Received::Contacts` arriving mid-session is ignored; names refresh on restart only.
- [ ] Signal caps edits at 10 per message; sst enforces the 24-hour window only.
- [ ] `print --format=json` carries no quote, reactions or attachments; `print` does not mark edited messages.

### Thread management
- [ ] `d` key on the chat list to delete a thread, with confirmation (chat-window `dd` delete-for-everyone already exists; this is list-level thread deletion)
