# simple signal tui

Signal TUI client

- uses libsignal-protocol-c (already installed)
- supports authentication via qr code
- supports typing notification
- supports seen notification

Application Flow (new auth)
- check if auth token is present, if not -> start auth flow (can be forced with parameter --relink)

Application Flow (existing auth)
- show chat list / index
  - format: one line per chat.
    - normal chat: "<full name>: <truncated last message preview>"
      Example: "Florian Heß: This is a test message that get's truncated on the right side of the available space. The message can be long..."
    - group chat: "Group: <comma separated first names>: <truncated last message preview>"
      Example: "Group: Florian, Andreas, Dominik: This is the last message, which is also truncated to stay on one line..."
  - use can select chat with arrow keys, return enters the chat
  - Q: quit application
  - d: delete chat (ask for confirmation)
  - most recent chat is on top
- chat window:
  - Chat Style
    * Florian Heß:
      Hello people!
    * Andreas Schirra:
      Hi Florian!
    * Stefan Hagen:
      Messages can be long and
      contain line breaks
    * Stefan Hagen:
      Messages with reactions will have the reactions being added to the end of the message. [1x❤, 3x👋]
    * Stefan Hagen:
      > Florian Heß:
      > Hello people!
      Replies will quote the original message
    * Andreas Schirra:
      > Stefan Hagen:
      > Replies will quote the original message
      Replies of messages that contain replies, will not include the previous reply.
  - At the bottom, there are two lines/bars: A status bar, a input bar
    - Status bar: shows typing notifications:
      Example (one person typing): Florian Heß is typing...
      Example (two person typing): Florain and Andreas are typing...
      Example (more than two person typing): Florian, Andrease, and 3 more are typing...
    - Input bar: Bar that receives user input
      - return: sends message
      - shift + return: - wraps into a new line. The input bar grows into two lines. Can be repeated to create multi line input
      - @<username>: will mention the user
      - @andr<tab>: will try to complete the username from the list of people in the chat (excluding myself, so @<tab> would always complete the chat partner in a one to one chat). "andr" here is the example of a partial username.
      - @and<tab>tab> or @<tab><tab>: "and" here is a partial username with multiple matches. <tab> would not complete it. A second <tab> would lead to all potential matches being displayed on the Status bar (replacing status bar content). The status bar content restores when the next character is typed.
    - In the chat, a user can use the shift+arrow keys to highlight a chat message from a user.
      - default: no message is selected
      - shift+arrow up/down keys: message highlight is created and the user can highlight a previous message in the chat
      - escape: escape clears the highlight
      - When a highlight is set, the use has additional options in the input bar:
        - /reply: if the user types /reply Hi Florian and has "[2026-06-26 09:23] Florian Heß: Hello people!" highlighted, this message would be replied on.
        - /react: if the user types /react thumbs-up and has "[2026-06-26 09:23] Florian Heß: Hello people!" highlighted, this message would get a thumbs up emoji reaction
        - /<tab> /rea<tab> /r<tab><tab>: Slash commends should autocomplete with the same tab logic as the user mention (<tab> completes if unique match, second tab would show match options on the status bar.
        - /react th<tab>: we support autocomplete for the emoji keys...
          - /react <tab>: <tab> without an emoji will show supported reaction emojis on the status bar. Example: 😄 laugh, 👍 thumbs-up, 👋 wave, ❤️ heart; the user would only enter the key, not the emoji itself - that's just preview.
          - /react: react without any parameter would show existing reactions on the message

open questions:
- index: how to mark chats with unread messages
- chat: how to mark unread messages
- chat: how to display a time stamp at reasonable times to not clutter chat.. maybe once when a message has been received and more that 1h has passed? maybe we can show more message data in the status bar while message highlight is active?



