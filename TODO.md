# TODO

## Where we left off

Phase 4 (chat window) is complete. The input bar has a working cursor with arrow-key movement.

## Open tasks

### Near-term (input/display polish)
- [ ] Group sender names show raw UUID — resolve to contact name
- [ ] `ThreadEntry.unread` is set on incoming messages but never cleared when a chat is opened; needs a "last-seen timestamp" mechanism persisted to disk

### Phase 5 candidates (from CLAUDE.md spec)
- [ ] Message selection: Shift+↑ activates selection at most recent message, Shift+↓ moves down; ESC clears; auto-scroll at edges
- [ ] Slash commands (require active selection): `/reply <text>`, `/react <emoji>`, `/react` (show reactions)
- [ ] `@mention` autocomplete: single Tab on unique match, double Tab lists all candidates on status bar
- [ ] Reactions display inline: `[1x❤, 3x👋]` appended to message
- [ ] Reply quoting: `> original` prefix, one level only
- [ ] Read receipts: ✓ (delivered) / ✓✓ (seen) per sent message
- [ ] Typing notifications in status bar
- [ ] `── new ──` unread boundary separator

### Infrastructure
- [ ] Signal receive stream closes after first live event — reconnection logic needed for live incoming messages from others
- [ ] `d` key on chat list to delete thread (with confirmation), per spec
