use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{Stream, StreamExt};
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::Content;
use presage::libsignal_service::prelude::Uuid;
use presage::libsignal_service::proto::{AttachmentPointer, ReceiptMessage};
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use crate::attachments::{self, StagedAttachment};
use crate::ipc;
use crate::mentions;
use crate::signal::{self, ReactionMap, ThreadEntry, MessageUpdate};

pub enum View {
    ChatList,
    ChatWindow,
    ContactBrowser,
}

pub enum Mode {
    Normal,
    Insert,
    Command(String),
}

/// What the store holds for a thread, as the chat window shows it. Receipts are
/// scanned on open only; afterwards live receipts fold in as they arrive.
pub struct ChatView {
    pub messages: Vec<Content>,
    pub reactions: ReactionMap,
    pub receipts: Option<(HashSet<u64>, HashSet<u64>)>,
    pub expire_timer: Option<u32>,
}

async fn load_chat_view<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
    with_receipts: bool,
) -> anyhow::Result<ChatView> {
    let (messages, reactions) = signal::load_messages_and_reactions(manager, thread, None).await?;
    let receipts = if with_receipts {
        Some(signal::load_receipt_state(manager, thread, &messages).await.unwrap_or_default())
    } else {
        None
    };
    let expire_timer = signal::expire_timer(manager, thread).await;
    Ok(ChatView { messages, reactions, receipts, expire_timer })
}

pub struct ChatState {
    pub thread: Thread,
    pub thread_name: String,
    pub expire_timer: Option<u32>,         // disappearing-message timer in seconds
    pub messages: Vec<Content>,
    pub scroll: usize,
    pub viewport_height: u16,
    pub viewport_top_msg: usize,            // index of first visible message, written by renderer
    pub input: String,
    pub cursor: usize,                     // byte offset into `input`
    pub selected_message: Option<usize>,   // index into `messages`
    pub delivered: HashSet<u64>,           // timestamps of our messages confirmed delivered
    pub read: HashSet<u64>,                // timestamps of our messages confirmed read
    pub reactions: ReactionMap,
    pub mode: Mode,
    pub reply_to: Option<usize>,           // message index set by 'r' in Normal mode
    pub editing: Option<u64>,              // timestamp of the message being edited, set by 'e'
    pub sender_names: HashMap<Uuid, String>, // resolved names for sender display and @mention
    pub pending_d: bool,                   // true after first 'd'; second 'd' triggers delete
    pub staged_attachments: Vec<StagedAttachment>,
    pub selected_attachment: Option<usize>, // mutually exclusive with selected_message
}

impl ChatState {
    /// Replace the message list, keeping the selection and reply target on the
    /// same messages by timestamp. Indices shift when a message is deleted or
    /// arrives out of order.
    pub fn set_messages(&mut self, messages: Vec<Content>) {
        let ts_at = |idx: Option<usize>| idx.and_then(|i| self.messages.get(i)).map(|m| m.timestamp());
        let selected_ts = ts_at(self.selected_message);
        let reply_ts = ts_at(self.reply_to);
        self.messages = messages;
        let index_of = |ts: Option<u64>| ts.and_then(|t| self.messages.iter().position(|m| m.timestamp() == t));
        self.selected_message = index_of(selected_ts);
        self.reply_to = index_of(reply_ts);
    }

    /// Replace everything the store provides, keeping cursor and draft state.
    pub fn apply(&mut self, view: ChatView) {
        self.set_messages(view.messages);
        self.reactions = view.reactions;
        if let Some((delivered, read)) = view.receipts {
            self.delivered = delivered;
            self.read = read;
        }
        self.expire_timer = view.expire_timer;
    }

    /// Fold a receipt that arrived live into the delivery state.
    pub fn apply_receipt(&mut self, receipt: &ReceiptMessage) {
        signal::fold_receipt(receipt, &mut self.delivered, &mut self.read);
    }

    /// Name shown for a thread participant; the local account is "You".
    pub fn display_name(&self, uuid: Uuid, own_aci: Option<Uuid>) -> String {
        if own_aci == Some(uuid) {
            return "You".to_string();
        }
        signal::name_or_uuid(&self.sender_names, uuid)
    }
}

pub enum AppCmd {
    OpenChat { thread: Thread, name: String },
    OpenContactBrowser,
    RefreshThreadList,
    SendMessage,
    ExecCmd(String), // colon command text (without leading ':')
    DeleteMessage,
}

pub struct App {
    pub threads: Vec<ThreadEntry>,
    pub list_state: ListState,
    pub status: Option<String>, // transient status line: completion candidates, errors, progress; cleared by the next key (Tab keeps it)
    pub acked: HashSet<u64>,    // messages read-receipted this session
    pub quit: bool,
    pub view: View,
    pub chat: Option<ChatState>,
    pub own_aci: Option<Uuid>,
    pub data_dir: PathBuf,
    pub contacts: Vec<ThreadEntry>,
    pub contacts_split: usize,
    pub contact_list_state: ListState,
}

impl App {
    pub fn new(threads: Vec<ThreadEntry>, own_aci: Option<Uuid>, data_dir: PathBuf) -> Self {
        let mut list_state = ListState::default();
        if !threads.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            threads,
            list_state,
            status: None,
            acked: HashSet::new(),
            quit: false,
            view: View::ChatList,
            chat: None,
            own_aci,
            data_dir,
            contacts: Vec::new(),
            contacts_split: 0,
            contact_list_state: ListState::default(),
        }
    }

    pub fn open_chat(&mut self, thread: Thread, thread_name: String, view: ChatView, sender_names: HashMap<Uuid, String>) {
        let (delivered, read) = view.receipts.unwrap_or_default();
        self.chat = Some(ChatState {
            thread,
            thread_name,
            expire_timer: view.expire_timer,
            messages: view.messages,
            scroll: 0,
            viewport_height: 0,
            viewport_top_msg: 0,
            input: String::new(),
            cursor: 0,
            selected_message: None,
            delivered,
            read,
            reactions: view.reactions,
            mode: Mode::Normal,
            reply_to: None,
            editing: None,
            sender_names,
            pending_d: false,
            staged_attachments: Vec::new(),
            selected_attachment: None,
        });
        self.view = View::ChatWindow;
    }

    fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn select(&mut self, idx: usize) {
        if self.threads.is_empty() {
            return;
        }
        self.list_state.select(Some(idx.min(self.threads.len() - 1)));
    }

    pub fn on_paste(&mut self, text: String) {
        let Some(chat) = self.chat.as_mut() else { return };
        match &mut chat.mode {
            Mode::Insert => {
                // Normalize line endings: terminals may send \r\n (Windows) or bare \r.
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                chat.input.insert_str(chat.cursor, &normalized);
                chat.cursor += normalized.len();
            }
            Mode::Command(s) => {
                // Command line is single-line; collapse newlines to spaces.
                let single = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
                s.push_str(single.trim_end());
            }
            Mode::Normal => {}
        }
    }

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        // Tab keeps completion candidates on the bar; any other key starts fresh.
        if !(matches!(self.view, View::ChatWindow) && key.code == KeyCode::Tab) {
            self.status = None;
        }
        match self.view {
            View::ChatList => self.on_key_list(key),
            View::ChatWindow => self.on_key_chat(key),
            View::ContactBrowser => self.on_key_contacts(key),
        }
    }

    /// Show a failed command's error where the user is looking, and log it.
    pub fn show_error(&mut self, err: &anyhow::Error) {
        let text = format!("{err:#}");
        tracing::warn!("{text}");
        self.status = Some(text);
    }

    fn on_key_list(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => { self.quit = true; None }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = self.selected().map_or(0, |i| i + 1);
                self.select(next);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let prev = self.selected().and_then(|i| i.checked_sub(1)).unwrap_or(0);
                self.select(prev);
                None
            }
            KeyCode::PageDown => {
                let next = self.selected().map_or(0, |i| i + 10);
                self.select(next);
                None
            }
            KeyCode::PageUp => {
                let prev = self.selected().and_then(|i| i.checked_sub(10)).unwrap_or(0);
                self.select(prev);
                None
            }
            KeyCode::Char('n') => Some(AppCmd::OpenContactBrowser),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.selected().and_then(|idx| {
                    let t = self.threads.get(idx)?;
                    Some(AppCmd::OpenChat { thread: t.thread.clone(), name: t.name.clone() })
                })
            }
            _ => None,
        }
    }

    fn on_key_chat(&mut self, key: KeyEvent) -> Option<AppCmd> {
        // Keys that need to set self.chat = None must fire before any borrow of self.chat.
        if matches!(self.chat.as_ref()?.mode, Mode::Normal)
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left)
        {
            self.chat = None;
            return Some(AppCmd::RefreshThreadList);
        }

        let own_aci = self.own_aci;
        let chat = self.chat.as_mut()?;
        if chat.pending_d && (!matches!(chat.mode, Mode::Normal) || key.code != KeyCode::Char('d')) {
            chat.pending_d = false;
        }

        // PgUp/PgDn scroll the message viewport in Normal and Insert modes;
        // Command mode ignores them so typing a command isn't interrupted.
        if !matches!(chat.mode, Mode::Command(_)) {
            let page = chat.viewport_height as usize;
            match key.code {
                KeyCode::PageUp => {
                    chat.scroll = chat.scroll.saturating_add(page);
                    return None;
                }
                KeyCode::PageDown => {
                    chat.scroll = chat.scroll.saturating_sub(page);
                    return None;
                }
                _ => {}
            }
        }

        match chat.mode {
            Mode::Normal => normal_mode_key(chat, &mut self.status, own_aci, key),
            Mode::Insert => insert_mode_key(chat, &mut self.status, key),
            Mode::Command(_) => command_mode_key(chat, &mut self.status, key),
        }
    }

    fn on_key_contacts(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        // Whether a separator row exists between contacts and groups.
        let has_sep = self.contacts_split > 0 && self.contacts_split < self.contacts.len();
        let total = self.contacts.len() + if has_sep { 1 } else { 0 };
        let sep = self.contacts_split; // display index of the separator row

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('h') | KeyCode::Left => {
                Some(AppCmd::RefreshThreadList)
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let cur = self.contact_list_state.selected().unwrap_or(0);
                let mut next = (cur + 1).min(total.saturating_sub(1));
                if has_sep && next == sep { next = (next + 1).min(total - 1); }
                self.contact_list_state.select(Some(next));
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let cur = self.contact_list_state.selected().unwrap_or(0);
                if cur == 0 { return None; }
                let mut prev = cur - 1;
                if has_sep && prev == sep { prev = prev.saturating_sub(1); }
                self.contact_list_state.select(Some(prev));
                None
            }
            KeyCode::PageDown => {
                let cur = self.contact_list_state.selected().unwrap_or(0);
                let mut next = (cur + 10).min(total.saturating_sub(1));
                if has_sep && next == sep { next = (next + 1).min(total - 1); }
                self.contact_list_state.select(Some(next));
                None
            }
            KeyCode::PageUp => {
                let cur = self.contact_list_state.selected().unwrap_or(0);
                let mut prev = cur.saturating_sub(10);
                if has_sep && prev == sep { prev = prev.saturating_sub(1); }
                self.contact_list_state.select(Some(prev));
                None
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                let display_idx = self.contact_list_state.selected()?;
                // Map display index to data index (skip separator row).
                let data_idx = if has_sep {
                    if display_idx < sep { display_idx }
                    else if display_idx == sep { return None; }
                    else { display_idx - 1 }
                } else {
                    display_idx
                };
                let entry = self.contacts.get(data_idx)?;
                Some(AppCmd::OpenChat { thread: entry.thread.clone(), name: entry.name.clone() })
            }
            _ => None,
        }
    }

    /// Whether a live message should mark its thread unread: not when the user
    /// sent it from another device, and not when they are looking at that chat.
    fn marks_unread(&self, update: &MessageUpdate) -> bool {
        self.own_aci != Some(update.sender) && !self.chat.as_ref().is_some_and(|c| c.thread == update.thread)
    }

    fn entry_mut(&mut self, thread: &Thread) -> Option<&mut ThreadEntry> {
        self.threads.iter_mut().find(|e| &e.thread == thread)
    }

    /// Fold a live message into its thread entry. Returns the update when no
    /// entry exists for its thread, so the caller can add one with a name.
    fn apply_update(&mut self, update: MessageUpdate) -> Option<MessageUpdate> {
        let mark_unread = self.marks_unread(&update);
        let Some(entry) = self.entry_mut(&update.thread) else {
            return update.edit_of.is_none().then_some(update);
        };
        if let Some(target_ts) = update.edit_of {
            // An edit changes text only; it shows in the list if it hit the newest message.
            if target_ts == entry.last_ts {
                entry.last_preview = Some(update.preview);
            }
            return None;
        }
        // Backlog delivery can be out of order; an older message must not replace the preview.
        if update.ts < entry.last_ts {
            return None;
        }
        entry.last_preview = Some(update.preview);
        if update.ts > entry.last_ts {
            entry.last_ts = update.ts;
            if mark_unread {
                entry.unread = true;
            }
        }
        signal::sort_recent_first(&mut self.threads);
        None
    }

    fn add_thread(&mut self, update: MessageUpdate, name: String) {
        let unread = self.marks_unread(&update);
        let mut entry = ThreadEntry::new(update.thread, name, Some(update.preview), update.ts);
        entry.unread = unread;
        self.threads.push(entry);
        signal::sort_recent_first(&mut self.threads);
    }

    fn mark_read(&mut self, thread: &Thread) {
        if let Some(entry) = self.entry_mut(thread) {
            entry.unread = false;
        }
    }
}

/// Selection index after a list shrank: the same position, or the new last item.
fn clamp_selection(sel: usize, len: usize) -> Option<usize> {
    (len > 0).then(|| sel.min(len - 1))
}

// ── Chat window key handling by mode ─────────────────────────────────────────

/// Normal mode: selection, reply, edit, delete and the mode switches.
fn normal_mode_key(chat: &mut ChatState, status: &mut Option<String>, own_aci: Option<Uuid>, key: KeyEvent) -> Option<AppCmd> {
    match key.code {
        KeyCode::Esc => {
            if chat.selected_message.is_some() || chat.selected_attachment.is_some() {
                chat.selected_message = None;
                chat.selected_attachment = None;
                chat.reply_to = None;
                chat.editing = None;
            }
        }
        KeyCode::Char('i') => chat.mode = Mode::Insert,
        KeyCode::Char(':') => chat.mode = Mode::Command(String::new()),
        KeyCode::Char('r') => {
            if chat.selected_message.is_some() {
                chat.reply_to = chat.selected_message;
                chat.cursor = chat.input.len();
                chat.mode = Mode::Insert;
            } else {
                *status = Some(HINT_SELECT_FIRST.to_string());
            }
        }
        KeyCode::Char('e') => match mutable_selection(chat, own_aci, "edit") {
            Err(why) => *status = Some(why),
            Ok(content) => {
                let ts = content.timestamp();
                // Roster names only: a label like "Attachment" or "You" would be sent as text.
                let body = signal::message_text(content, |u| signal::name_or_uuid(&chat.sender_names, u));
                chat.input = body;
                chat.cursor = chat.input.len();
                chat.editing = Some(ts);
                chat.mode = Mode::Insert;
            }
        },
        KeyCode::Char('d') => {
            if let Some(att_idx) = chat.selected_attachment {
                if chat.pending_d {
                    chat.pending_d = false;
                    chat.staged_attachments.remove(att_idx);
                    chat.selected_attachment = clamp_selection(att_idx, chat.staged_attachments.len());
                } else {
                    chat.pending_d = true;
                    *status = Some("d again to remove attachment".to_string());
                }
            } else if let Err(why) = mutable_selection(chat, own_aci, "delete") {
                *status = Some(why);
            } else if chat.pending_d {
                chat.pending_d = false;
                return Some(AppCmd::DeleteMessage);
            } else {
                chat.pending_d = true;
                *status = Some("d again to delete for everyone".to_string());
            }
        }
        KeyCode::Up | KeyCode::Char('k') => select_previous(chat),
        KeyCode::Down | KeyCode::Char('j') => select_next(chat),
        _ => {}
    }
    None
}

// The selection ring: oldest message → newest → first staged file → last → oldest.

fn select_previous(chat: &mut ChatState) {
    if chat.messages.is_empty() {
        return;
    }
    if let Some(i) = chat.selected_attachment {
        if i == 0 {
            chat.selected_attachment = None;
            chat.selected_message = Some(chat.messages.len() - 1);
        } else {
            chat.selected_attachment = Some(i - 1);
        }
    } else if let Some(s) = chat.selected_message {
        if s == 0 && !chat.staged_attachments.is_empty() {
            chat.selected_message = None;
            chat.selected_attachment = Some(chat.staged_attachments.len() - 1);
        } else {
            chat.selected_message = Some(s.saturating_sub(1));
        }
    } else {
        chat.selected_message = Some(chat.messages.len() - 1);
    }
}

fn select_next(chat: &mut ChatState) {
    if chat.messages.is_empty() {
        return;
    }
    if let Some(i) = chat.selected_attachment {
        let last = chat.staged_attachments.len().saturating_sub(1);
        if i >= last {
            chat.selected_attachment = None;
            chat.selected_message = Some(0);
        } else {
            chat.selected_attachment = Some(i + 1);
        }
    } else if let Some(sel) = chat.selected_message {
        let last_msg = chat.messages.len() - 1;
        if sel >= last_msg && !chat.staged_attachments.is_empty() {
            chat.selected_message = None;
            chat.selected_attachment = Some(0);
        } else {
            chat.selected_message = Some((sel + 1).min(last_msg));
        }
    } else {
        chat.selected_message = Some(chat.viewport_top_msg);
    }
}

/// Insert mode: editing the draft, Tab completion, Enter to send.
fn insert_mode_key(chat: &mut ChatState, status: &mut Option<String>, key: KeyEvent) -> Option<AppCmd> {
    match key.code {
        KeyCode::Esc => {
            // Cancels a pending edit or reply, as the status bar says it does.
            if chat.editing.take().is_some() {
                chat.input.clear();
                chat.cursor = 0;
            }
            chat.reply_to = None;
            chat.mode = Mode::Normal;
        }
        KeyCode::Left => chat.cursor = cursor_left(&chat.input, chat.cursor),
        KeyCode::Right => chat.cursor = cursor_right(&chat.input, chat.cursor),
        KeyCode::Up => chat.cursor = cursor_up(&chat.input, chat.cursor),
        KeyCode::Down => chat.cursor = cursor_down(&chat.input, chat.cursor),
        // Shift+Enter or Alt+Enter → newline.
        // Shift+Enter is only distinguishable from Enter on terminals that
        // support modifyOtherKeys/kitty-protocol. Alt+Enter (ESC \r) is more
        // universally reported with ALT modifier and is the reliable alternative.
        KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
            chat.input.insert(chat.cursor, '\n');
            chat.cursor += 1;
        }
        KeyCode::Enter => {
            if !chat.input.trim().is_empty() || !chat.staged_attachments.is_empty() {
                return Some(AppCmd::SendMessage);
            }
        }
        // Ctrl+H is the terminal-conventional backspace (0x08). Some
        // terminal emulators (including OpenBSD's) send it for Shift+Backspace.
        KeyCode::Backspace | KeyCode::Char('h')
            if key.code == KeyCode::Backspace || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if chat.cursor > 0 {
                let new_cursor = cursor_left(&chat.input, chat.cursor);
                chat.input.remove(new_cursor);
                chat.cursor = new_cursor;
            }
        }
        KeyCode::Char(c) => {
            chat.input.insert(chat.cursor, c);
            chat.cursor += c.len_utf8();
        }
        KeyCode::Tab => {
            if let Some(span) = completion_candidates(&chat.input, chat.cursor, &chat.sender_names) {
                if span.values.len() == 1 {
                    let rep = span.values[0].clone();
                    chat.input.replace_range(span.start..span.end, &rep);
                    chat.cursor = span.start + rep.len();
                    *status = None;
                } else {
                    let labels = span.display.as_deref().unwrap_or(&span.values);
                    *status = Some(labels.join("  "));
                }
            }
        }
        _ => {}
    }
    None
}

/// Command mode: typing a colon command, with Tab completion of its name or argument.
fn command_mode_key(chat: &mut ChatState, status: &mut Option<String>, key: KeyEvent) -> Option<AppCmd> {
    let Mode::Command(typed) = &chat.mode else { return None };
    let cmd_so_far = typed.clone();
    match key.code {
        KeyCode::Esc => chat.mode = Mode::Normal,
        KeyCode::Enter => {
            chat.mode = Mode::Normal;
            if !cmd_so_far.is_empty() {
                return Some(AppCmd::ExecCmd(cmd_so_far));
            }
        }
        KeyCode::Tab => {
            if let Some((name, arg)) = cmd_so_far.split_once(' ') {
                // Argument completion: dispatch on the registered ArgKind.
                *status = match COLON_COMMANDS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k) {
                    Some(ArgKind::Path) => apply_path_completion(name, arg, chat),
                    Some(ArgKind::Emoji) => apply_emoji_completion(name, arg, chat),
                    Some(ArgKind::None) | None => None,
                };
            } else {
                // Base-name completion: no argument typed yet.
                let partial = cmd_so_far.to_lowercase();
                let matches: Vec<&str> = COLON_COMMANDS
                    .iter()
                    .map(|(n, _)| *n)
                    .filter(|c| c.starts_with(partial.as_str()))
                    .collect();
                if matches.len() == 1 {
                    chat.mode = Mode::Command(matches[0].to_string());
                    *status = None;
                } else if !matches.is_empty() {
                    *status = Some(matches.iter().map(|c| format!(":{}", c)).collect::<Vec<_>>().join("  "));
                }
            }
        }
        KeyCode::Backspace => {
            if let Mode::Command(s) = &mut chat.mode {
                s.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Mode::Command(s) = &mut chat.mode {
                s.push(c);
            }
        }
        _ => {}
    }
    None
}

// ── Colon command registry ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum ArgKind {
    None,  // e.g. :quit
    Path,  // :upload, :download, :download-all
    Emoji, // :react
}

const COLON_COMMANDS: &[(&str, ArgKind)] = &[
    ("download",     ArgKind::Path),
    ("download-all", ArgKind::Path),
    ("quit",         ArgKind::None),
    ("react",        ArgKind::Emoji),
    ("upload",       ArgKind::Path),
];

const HINT_SELECT_FIRST: &str = "select a message first (j/k)";

/// The selected message if the user may edit or delete it, otherwise the status
/// text saying why not. `verb` is "edit" or "delete".
fn mutable_selection<'a>(chat: &'a ChatState, own_aci: Option<Uuid>, verb: &str) -> Result<&'a Content, String> {
    let Some(content) = chat.selected_message.and_then(|i| chat.messages.get(i)) else {
        return Err(HINT_SELECT_FIRST.to_string());
    };
    if !signal::is_own(content, own_aci) {
        return Err(format!("can only {verb} own messages"));
    }
    if signal::outside_edit_window(content.timestamp()) {
        return Err(format!("can only {verb} messages from the last 24 hours"));
    }
    Ok(content)
}

enum ColonCmd<'a> {
    Quit,
    React(&'a str),       // emoji/shortcode arg; empty string means "show reactions"
    Upload(&'a str),      // file path
    Download(&'a str),    // optional destination dir; empty = default
    DownloadAll(&'a str), // optional destination dir; empty = default
}

fn parse_colon_cmd(input: &str) -> Option<ColonCmd<'_>> {
    let s = input.trim().strip_prefix(':')?;
    let (name, arg) = s
        .split_once(' ')
        .map_or((s, ""), |(n, a)| (n, a.trim()));
    match name {
        "quit"         => Some(ColonCmd::Quit),
        "react"        => Some(ColonCmd::React(arg)),
        "upload"       => Some(ColonCmd::Upload(arg)),
        "download"     => Some(ColonCmd::Download(arg)),
        "download-all" => Some(ColonCmd::DownloadAll(arg)),
        _              => None,
    }
}

// ── Reaction helpers ──────────────────────────────────────────────────────────

/// Resolve `:react <arg>` input to an emoji string.
/// Non-ASCII input is treated as a raw emoji; ASCII input is looked up as a shortcode.
fn resolve_emoji(arg: &str) -> Option<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if !arg.is_ascii() {
        return Some(arg.to_string());
    }
    emojis::get_by_shortcode(arg).map(|e| e.to_string())
}

fn reaction_hint(reactions: &ReactionMap, target_ts: u64) -> String {
    let Some(map) = reactions.get(&target_ts).filter(|m| !m.is_empty()) else {
        return "(no reactions)".to_string();
    };
    signal::fmt_reaction_pairs(map).join("  ")
}

// ── Tab completion ────────────────────────────────────────────────────────────

pub struct CompletionSpan {
    pub start: usize,
    pub end: usize,
    pub values: Vec<String>,
    // Some when the hint text should differ from the completion values.
    pub display: Option<Vec<String>>,
}

fn completion_candidates(
    input: &str,
    cursor: usize,
    sender_names: &HashMap<Uuid, String>,
) -> Option<CompletionSpan> {
    let before = &input[..cursor.min(input.len())];

    if let Some(at_pos) = before.rfind('@') {
        let partial = &before[at_pos + 1..];
        if !partial.contains(' ') {
            let partial_lower = partial.to_lowercase();
            let mut candidates: Vec<String> = sender_names
                .values()
                .filter(|n| n.to_lowercase().starts_with(&partial_lower))
                .map(|n| format!("@{} ", n))
                .collect();
            candidates.sort();
            if candidates.is_empty() {
                return None;
            }
            return Some(CompletionSpan {
                start: at_pos,
                end: cursor,
                values: candidates,
                display: None,
            });
        }
    }

    None
}

// ── Path completion ───────────────────────────────────────────────────────────

fn complete_path(partial: &str) -> Vec<String> {
    let path = std::path::Path::new(partial);
    let (dir, file_prefix, dir_prefix) = if partial.ends_with('/') {
        (path, "", partial.to_string())
    } else {
        let parent_opt = path.parent().filter(|p| !p.as_os_str().is_empty());
        let parent = parent_opt.unwrap_or(std::path::Path::new("."));
        let prefix = path.file_name().and_then(|n| n.to_str()).unwrap_or(partial);
        let dir_prefix = parent_opt
            .map(|p| format!("{}/", p.to_str().unwrap_or("")))
            .unwrap_or_default();
        (parent, prefix, dir_prefix)
    };

    let Ok(read_dir) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut results: Vec<String> = read_dir
        .flatten()
        .filter_map(|entry| {
            let fname = entry.file_name().into_string().ok()?;
            if !fname.starts_with(file_prefix) { return None; }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(format!("{}{}{}", dir_prefix, fname, if is_dir { "/" } else { "" }))
        })
        .collect();
    results.sort();
    results
}

/// Complete the emoji shortcode argument in place on a unique match; otherwise
/// return the candidates to show.
fn apply_emoji_completion(cmd_name: &str, partial: &str, chat: &mut ChatState) -> Option<String> {
    let partial = partial.trim_start();
    if partial.is_empty() || !partial.is_ascii() {
        return None;
    }
    let partial_lower = partial.to_lowercase();
    let mut matches: Vec<&str> = emojis::iter()
        .flat_map(|e| e.shortcodes())
        .filter(|s| s.starts_with(partial_lower.as_str()))
        .collect();
    matches.sort_unstable();
    match matches.len() {
        0 => None,
        1 => {
            chat.mode = Mode::Command(format!("{} {}", cmd_name, matches[0]));
            None
        }
        _ => Some(matches.join("  ")),
    }
}

/// Complete the path argument in place on a unique match; otherwise return the
/// candidates to show.
fn apply_path_completion(cmd_name: &str, partial: &str, chat: &mut ChatState) -> Option<String> {
    let completions = complete_path(partial.trim_start());
    match completions.len() {
        0 => None,
        1 => {
            chat.mode = Mode::Command(format!("{} {}", cmd_name, completions[0]));
            None
        }
        _ => {
            let labels: Vec<String> = completions.iter()
                .map(|c| {
                    let p = std::path::Path::new(c.trim_end_matches('/'));
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(c);
                    if c.ends_with('/') { format!("{}/", name) } else { name.to_string() }
                })
                .collect();
            Some(labels.join("  "))
        }
    }
}

// ── Cursor movement helpers ───────────────────────────────────────────────────

fn cursor_left(input: &str, cursor: usize) -> usize {
    if cursor == 0 { return 0; }
    input[..cursor].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn cursor_right(input: &str, cursor: usize) -> usize {
    if cursor >= input.len() { return input.len(); }
    cursor + input[cursor..].chars().next().map_or(0, |c| c.len_utf8())
}

// Returns (line_index, visual_col) for a byte cursor position.
fn cursor_line_col(input: &str, cursor: usize) -> (usize, usize) {
    let before = &input[..cursor.min(input.len())];
    let parts: Vec<&str> = before.split('\n').collect();
    let line = parts.len().saturating_sub(1);
    let col = parts.last().map_or(0, |l| l.chars().count());
    (line, col)
}

// Returns the byte start of a given line index.
fn line_byte_start(input: &str, line_idx: usize) -> usize {
    input.split('\n').take(line_idx).map(|l| l.len() + 1).sum()
}

fn cursor_up(input: &str, cursor: usize) -> usize {
    let (line, col) = cursor_line_col(input, cursor);
    if line == 0 { return cursor; }
    let prev_line = input.split('\n').nth(line - 1).unwrap_or("");
    let col_clamped = col.min(prev_line.chars().count());
    let byte_in_line = prev_line
        .char_indices()
        .nth(col_clamped)
        .map_or(prev_line.len(), |(b, _)| b);
    line_byte_start(input, line - 1) + byte_in_line
}

fn cursor_down(input: &str, cursor: usize) -> usize {
    let (line, col) = cursor_line_col(input, cursor);
    let lines: Vec<&str> = input.split('\n').collect();
    if line + 1 >= lines.len() { return cursor; }
    let next_line = lines[line + 1];
    let col_clamped = col.min(next_line.chars().count());
    let byte_in_line = next_line
        .char_indices()
        .nth(col_clamped)
        .map_or(next_line.len(), |(b, _)| b);
    line_byte_start(input, line + 1) + byte_in_line
}

// ─────────────────────────────────────────────────────────────────────────────

async fn next_or_pending(stream: &mut Option<Pin<Box<dyn Stream<Item = Received>>>>) -> Option<Received> {
    match stream {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

/// Fold a live Signal event into the thread list and the open chat.
async fn on_live_event<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>, received: &Received) {
    if let Received::Content(content) = received {
        fold_live_message(app, manager, content).await;
    }
    let Some(thread) = app.chat.as_ref().map(|c| c.thread.clone()) else { return };
    if let Some(receipt) = signal::live_receipt(received) {
        if let Some(chat) = &mut app.chat {
            chat.apply_receipt(receipt);
        }
    } else if signal::is_message_in(received, &thread) {
        match load_chat_view(manager, &thread, false).await {
            Ok(view) => {
                if let Some(chat) = &mut app.chat {
                    chat.apply(view);
                }
                ack_unseen(app, manager).await;
            }
            Err(e) => tracing::warn!("reload chat: {e:#}"),
        }
    }
}

/// Update the thread list for a live message, adding an entry for a thread not yet listed.
async fn fold_live_message<S: Store>(app: &mut App, manager: &Manager<S, Registered>, content: &Content) {
    let names = signal::mention_names(manager, content).await;
    let Some(update) = signal::extract_update(content, |u| signal::name_or_uuid(&names, u)) else { return };
    if let Some(update) = app.apply_update(update) {
        if let Thread::Contact(sid) = &update.thread
            && app.own_aci != Some(sid.raw_uuid())
        {
            signal::remember_contact(&app.data_dir, sid.raw_uuid());
        }
        let name = signal::thread_title(manager, &update.thread).await;
        app.add_thread(update, name);
    }
}

/// Carry out a send that arrived over the socket, then show the stored message
/// exactly as if it had arrived live.
async fn handle_send_job<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>, job: ipc::SendJob) {
    let result = ipc::perform(manager, &job.request).await;
    if let Ok((thread, ts)) = &result
        && let Ok(Some(content)) = manager.store().message(thread, *ts).await
    {
        on_live_event(app, manager, &Received::Content(Box::new(content))).await;
    }
    job.finish(result);
}

/// Send read receipts for the open chat's messages from others that have not
/// been acknowledged this session, and remember them.
async fn ack_unseen<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>) {
    let Some(chat) = &app.chat else { return };
    let unseen: Vec<(Uuid, u64)> = chat
        .messages
        .iter()
        .filter(|m| !signal::is_own(m, app.own_aci) && !app.acked.contains(&m.timestamp()))
        .map(|m| (m.metadata.sender.raw_uuid(), m.timestamp()))
        .collect();
    let stamps: Vec<u64> = unseen.iter().map(|(_, ts)| *ts).collect();
    match signal::send_read_receipts(manager, unseen).await {
        Ok(()) => app.acked.extend(stamps),
        Err(e) => tracing::warn!("send_read_receipts: {e:#}"),
    }
}

// Reload messages + reactions from the store after a send and apply them to the open chat.
// presage commits sent messages to the local store before returning from the send call,
// so this is immediately consistent without waiting for the SyncMessage echo.
async fn reload_chat<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    chat: &mut Option<ChatState>,
) {
    if let Ok((msgs, rxns)) = signal::load_messages_and_reactions(manager, thread, None).await
        && let Some(c) = chat
    {
        c.set_messages(msgs);
        c.reactions = rxns;
    }
}

fn resolve_download_dir(path_arg: &str, thread_name: &str) -> anyhow::Result<PathBuf> {
    if !path_arg.is_empty() {
        return Ok(PathBuf::from(path_arg));
    }
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("$HOME not set"))?;
    let safe_name = thread_name.replace(['/', '\\', '\0'], "_");
    Ok(PathBuf::from(home).join("Downloads").join("sst").join(safe_name))
}

async fn execute_cmd<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    cmd: AppCmd,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> anyhow::Result<()> {
    match cmd {
        AppCmd::OpenChat { thread, name } => open_chat_from_store(app, manager, terminal, thread, name).await,
        AppCmd::OpenContactBrowser => open_contact_browser(app, manager).await,
        AppCmd::RefreshThreadList => refresh_thread_list(app, manager).await,
        AppCmd::SendMessage => send_draft(app, manager).await,
        AppCmd::ExecCmd(text) => run_colon_command(app, manager, terminal, &text).await,
        AppCmd::DeleteMessage => delete_selected(app, manager).await,
    }
}

/// Open a chat with the names the store has, then fetch the missing member
/// profiles one at a time with a redraw between them, so the chat is usable at once.
async fn open_chat_from_store<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    thread: Thread,
    name: String,
) -> anyhow::Result<()> {
    let view = load_chat_view(manager, &thread, true).await?;
    let (sender_names, mut unnamed) = signal::roster_names(manager, &thread, app.own_aci, None).await;
    app.mark_read(&thread);
    app.open_chat(thread, name, view, sender_names);
    ack_unseen(app, manager).await;
    while let Some((uuid, profile_key)) = unnamed.pop() {
        terminal.draw(|f| crate::ui::draw(f, app))?;
        if let Some(name) = signal::fetch_member_name(manager, uuid, profile_key).await
            && let Some(chat) = &mut app.chat
        {
            chat.sender_names.insert(uuid, name);
        }
    }
    Ok(())
}

async fn open_contact_browser<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
    let (contacts, split) = signal::list_all_contacts(manager, app.own_aci).await?;
    app.contacts = contacts;
    app.contacts_split = split;
    let mut state = ListState::default();
    if !app.contacts.is_empty() {
        state.select(Some(0));
    }
    app.contact_list_state = state;
    app.view = View::ContactBrowser;
    Ok(())
}

async fn refresh_thread_list<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
    let mut threads = signal::list_threads(manager, &app.data_dir, app.own_aci).await?;
    // The store cannot know what the user has seen; keep this session's flags.
    for entry in &mut threads {
        entry.unread = app.threads.iter().any(|old| old.unread && old.thread == entry.thread);
    }
    app.threads = threads;
    // Clamp selection so it stays valid after the list changes.
    let new_len = app.threads.len();
    match app.list_state.selected() {
        Some(sel) if new_len > 0 => app.list_state.select(Some(sel.min(new_len - 1))),
        None if new_len > 0 => app.list_state.select(Some(0)),
        _ => {}
    }
    app.view = View::ChatList;
    Ok(())
}

/// Send the draft as a new message, a reply or an edit, with the staged files.
async fn send_draft<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
    let chat = app.chat.as_mut().expect("SendMessage with no open chat");
    let thread = chat.thread.clone();

    // Upload before touching the draft, so a failed upload keeps the text,
    // reply target and edit target for a retry; only the failed file is dropped.
    let staged = if chat.editing.is_none() {
        std::mem::take(&mut chat.staged_attachments)
    } else {
        Vec::new()
    };
    let pointers = if staged.is_empty() {
        Vec::new()
    } else {
        match attachments::upload_staged_attachments(manager, &staged).await {
            Ok(pts) => pts,
            Err((msg, failed_path)) => {
                chat.staged_attachments = staged.into_iter().filter(|a| a.path != failed_path).collect();
                chat.selected_attachment =
                    chat.selected_attachment.and_then(|sel| clamp_selection(sel, chat.staged_attachments.len()));
                return Err(anyhow::anyhow!(msg));
            }
        }
    };

    let quoted = chat.reply_to.and_then(|idx| chat.messages.get(idx)).map(signal::quote_of);
    let targeted = chat.editing.is_some() || quoted.is_some();
    let (text, body_ranges) = mentions::encode_mentions(chat.input.trim(), &chat.sender_names);
    if text.is_empty() && pointers.is_empty() {
        return Ok(());
    }

    let sent = if let Some(edit_ts) = chat.editing {
        match chat.messages.iter().find(|m| m.timestamp() == edit_ts) {
            Some(original) => signal::send_edit(manager, &thread, original, text, body_ranges).await,
            None => Err(anyhow::anyhow!("the message being edited no longer exists")),
        }
    } else if let Some(quoted) = quoted {
        signal::send_reply(manager, &thread, text, body_ranges, quoted, pointers).await
    } else {
        signal::send_to_thread(manager, &thread, text, body_ranges, pointers).await.map(|_| ())
    };
    match sent {
        Ok(()) => {
            chat.input.clear();
            chat.cursor = 0;
            chat.reply_to = None;
            chat.editing = None;
            chat.selected_attachment = None;
            if targeted {
                chat.selected_message = None;
            }
            reload_chat(manager, &thread, &mut app.chat).await;
            Ok(())
        }
        Err(e) => {
            // Keep the draft and put the files back so a retry re-uploads them.
            chat.staged_attachments = staged;
            Err(e.context("send failed"))
        }
    }
}

async fn run_colon_command<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    text: &str,
) -> anyhow::Result<()> {
    let Some(thread) = app.chat.as_ref().map(|c| c.thread.clone()) else { return Ok(()) };
    match parse_colon_cmd(&format!(":{text}")) {
        Some(ColonCmd::Quit) => app.quit = true,
        Some(ColonCmd::React(arg)) => react(app, manager, &thread, arg).await?,
        Some(ColonCmd::Upload(path)) => stage_upload(app, path),
        Some(ColonCmd::Download(dir)) => download(app, manager, terminal, dir, DownloadScope::Selected).await?,
        Some(ColonCmd::DownloadAll(dir)) => download(app, manager, terminal, dir, DownloadScope::All).await?,
        None => app.status = Some(format!("unknown command: :{text}")),
    }
    Ok(())
}

/// `:react` with no argument shows the selected message's reactions; with an
/// emoji or shortcode it toggles that reaction.
async fn react<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>, thread: &Thread, arg: &str) -> anyhow::Result<()> {
    let target = app.chat.as_ref().and_then(|c| {
        let msg = c.messages.get(c.selected_message?)?;
        Some((msg.timestamp(), msg.metadata.sender.raw_uuid()))
    });
    let Some((target_ts, target_author)) = target else {
        app.status = Some("no message selected".to_string());
        return Ok(());
    };
    if arg.is_empty() {
        if let Some(chat) = &app.chat {
            app.status = Some(reaction_hint(&chat.reactions, target_ts));
        }
        return Ok(());
    }
    let Some(emoji) = resolve_emoji(arg) else {
        app.status = Some(format!("unknown emoji: {arg}"));
        return Ok(());
    };
    let already_mine = app.own_aci.zip(app.chat.as_ref()).is_some_and(|(me, chat)| {
        chat.reactions
            .get(&target_ts)
            .and_then(|per_emoji| per_emoji.get(&emoji))
            .is_some_and(|reactors| reactors.contains(me.as_bytes()))
    });
    signal::send_reaction(manager, thread, emoji, target_ts, target_author, already_mine).await?;
    reload_chat(manager, thread, &mut app.chat).await;
    Ok(())
}

fn stage_upload(app: &mut App, path: &str) {
    if path.is_empty() {
        app.status = Some(":upload <path>".to_string());
        return;
    }
    match attachments::stage_attachment(std::path::Path::new(path)) {
        Ok(att) => {
            if let Some(chat) = &mut app.chat {
                chat.staged_attachments.push(att);
            }
        }
        Err(msg) => app.status = Some(msg),
    }
}

enum DownloadScope {
    Selected,
    All,
}

/// Download the selected message's attachments or every attachment in the
/// chat into `dir_arg` (default: ~/Downloads/sst/<chat>), drawing progress
/// between files.
async fn download<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    dir_arg: &str,
    scope: DownloadScope,
) -> anyhow::Result<()> {
    let Some(chat) = app.chat.as_ref() else { return Ok(()) };
    let thread_name = chat.thread_name.clone();
    // (filename, pointer, progress label)
    let queue: Vec<(String, AttachmentPointer, String)> = match scope {
        DownloadScope::Selected => {
            let Some(idx) = chat.selected_message else {
                app.status = Some(HINT_SELECT_FIRST.to_string());
                return Ok(());
            };
            let attachments = chat.messages.get(idx).map(attachments::message_attachments).unwrap_or(&[]);
            attachments
                .iter()
                .enumerate()
                .map(|(i, pointer)| (attachments::attachment_filename(pointer, i), pointer.clone(), String::new()))
                .collect()
        }
        DownloadScope::All => {
            let with_attachments: Vec<&Content> =
                chat.messages.iter().filter(|m| !attachments::message_attachments(m).is_empty()).collect();
            let mut queue = Vec::new();
            for (n, msg) in with_attachments.iter().enumerate() {
                for pointer in attachments::message_attachments(msg) {
                    let label = format!(" (message {}/{})", n + 1, with_attachments.len());
                    queue.push((attachments::attachment_filename(pointer, queue.len()), pointer.clone(), label));
                }
            }
            queue
        }
    };
    if queue.is_empty() {
        app.status = Some(
            match scope {
                DownloadScope::Selected => "no attachments on selected message",
                DownloadScope::All => "no attachments in this chat",
            }
            .to_string(),
        );
        return Ok(());
    }

    let dir = resolve_download_dir(dir_arg, &thread_name)?;
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    let total = queue.len();
    for (i, (filename, pointer, label)) in queue.iter().enumerate() {
        if total > 1 {
            app.status = Some(format!("downloading {}/{}{}…", i + 1, total, label));
            terminal.draw(|f| crate::ui::draw(f, app))?;
        }
        attachments::download_attachment(manager, pointer, &dir, filename)
            .await
            .map_err(|e| anyhow::anyhow!("file {}: {e}", i + 1))?;
    }
    app.status = Some(format!("saved {total} file(s) to {}", dir.display()));
    Ok(())
}

async fn delete_selected<S: Store>(app: &mut App, manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
    // Ownership and age were checked when the second d was pressed.
    let Some(chat) = app.chat.as_ref() else { return Ok(()) };
    let Some(sel_idx) = chat.selected_message else { return Ok(()) };
    let Some(timestamp) = chat.messages.get(sel_idx).map(|m| m.timestamp()) else { return Ok(()) };
    let thread = chat.thread.clone();
    signal::delete_for_everyone(manager, &thread, timestamp).await?;
    // The store row is gone; the reload drops the message, and with it the selection.
    reload_chat(manager, &thread, &mut app.chat).await;
    if let Some(chat) = &mut app.chat {
        chat.selected_message = clamp_selection(sel_idx, chat.messages.len());
    }
    Ok(())
}

pub async fn run<S: Store>(
    threads: Vec<ThreadEntry>,
    own_aci: Option<Uuid>,
    data_dir: PathBuf,
    mut manager: Manager<S, Registered>,
    signal_stream: Pin<Box<dyn Stream<Item = Received>>>,
) -> anyhow::Result<()> {
    // Ensure the terminal is restored on panic so the user's shell is not corrupted.
    // The normal error path (returning Err from the event loop) is already covered by
    // the explicit cleanup below; this hook only fires on unexpected panics.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::terminal::LeaveAlternateScreen,
        );
        original_hook(info);
    }));

    let (mut send_jobs, _send_socket) = ipc::serve(&data_dir)?;

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste,
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(threads, own_aci, data_dir);
    let mut events = EventStream::new();
    let mut signal_stream: Option<Pin<Box<dyn Stream<Item = Received>>>> = Some(signal_stream);

    let result: anyhow::Result<()> = async {
        loop {
            terminal.draw(|f| crate::ui::draw(f, &mut app))?;

            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(Event::Key(key))) => {
                            if let Some(cmd) = app.on_key(key)
                                && let Err(e) = execute_cmd(&mut app, &mut manager, cmd, &mut terminal).await
                            {
                                app.show_error(&e);
                            }
                        }
                        Some(Ok(Event::Paste(text))) => {
                            app.on_paste(text);
                        }
                        Some(Err(e)) => return Err(anyhow::anyhow!(e)),
                        _ => {}
                    }
                }
                Some(job) = send_jobs.recv() => {
                    handle_send_job(&mut app, &mut manager, job).await;
                }
                event = next_or_pending(&mut signal_stream) => {
                    if let Some(received) = event {
                        on_live_event(&mut app, &mut manager, &received).await;
                    } else {
                        // Drop the dead stream so the next iteration's next_or_pending
                        // sleeps on future::pending() rather than re-polling a closed stream.
                        signal_stream = None;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        match manager.receive_messages().await {
                            Ok(s) => signal_stream = Some(Box::pin(s)),
                            Err(e) => tracing::warn!("signal stream reconnect failed: {e}"),
                        }
                    }
                }
            }

            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    )?;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Uuid {
        Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap()
    }

    fn other() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn thread(id: &str) -> Thread {
        signal::parse_thread_id(id).unwrap()
    }

    #[test]
    fn cursor_moves_by_character_not_byte() {
        let s = "aé😀b";
        assert_eq!(cursor_right(s, 0), 1);
        assert_eq!(cursor_right(s, 1), 3);
        assert_eq!(cursor_right(s, 3), 7);
        assert_eq!(cursor_right(s, 8), 8);
        assert_eq!(cursor_left(s, 7), 3);
        assert_eq!(cursor_left(s, 3), 1);
        assert_eq!(cursor_left(s, 0), 0);
    }

    #[test]
    fn cursor_up_and_down_keep_the_column_where_the_line_allows() {
        let s = "hello\nhi\nworld";
        assert_eq!(cursor_line_col(s, 4), (0, 4));
        assert_eq!(cursor_down(s, 4), 8);
        assert_eq!(cursor_line_col(s, 8), (1, 2));
        assert_eq!(cursor_down(s, 8), 11);
        assert_eq!(cursor_down(s, 11), 11);
        assert_eq!(cursor_up(s, 11), 8);
        assert_eq!(cursor_up(s, 8), 2);
        assert_eq!(cursor_up(s, 2), 2);
    }

    #[test]
    fn completion_offers_roster_names_matching_the_partial() {
        let names = HashMap::from([(me(), "Alice".to_string()), (other(), "Albert".to_string())]);
        let input = "hi @al";
        let span = completion_candidates(input, input.len(), &names).unwrap();
        assert_eq!((span.start, span.end), (3, 6));
        assert_eq!(span.values, vec!["@Albert ".to_string(), "@Alice ".to_string()]);
        assert!(completion_candidates("hi @zed", 7, &names).is_none());
        assert!(completion_candidates("@Alice is here", 14, &names).is_none());
    }

    #[test]
    fn colon_commands_parse_with_an_optional_argument() {
        assert!(matches!(parse_colon_cmd(":quit"), Some(ColonCmd::Quit)));
        assert!(matches!(parse_colon_cmd(":react wave"), Some(ColonCmd::React("wave"))));
        assert!(matches!(parse_colon_cmd(":react"), Some(ColonCmd::React(""))));
        assert!(matches!(parse_colon_cmd(":upload /tmp/x.png"), Some(ColonCmd::Upload("/tmp/x.png"))));
        assert!(matches!(parse_colon_cmd(":download-all"), Some(ColonCmd::DownloadAll(""))));
        assert!(parse_colon_cmd(":nope").is_none());
        assert!(parse_colon_cmd("quit").is_none());
    }

    #[test]
    fn emoji_resolves_shortcodes_and_passes_raw_emoji_through() {
        assert_eq!(resolve_emoji("wave").as_deref(), Some("👋"));
        assert_eq!(resolve_emoji("❤️").as_deref(), Some("❤️"));
        assert!(resolve_emoji("not-a-shortcode").is_none());
        assert!(resolve_emoji("  ").is_none());
    }

    #[test]
    fn selection_clamps_after_a_removal() {
        assert_eq!(clamp_selection(3, 5), Some(3));
        assert_eq!(clamp_selection(4, 4), Some(3));
        assert_eq!(clamp_selection(0, 0), None);
    }

    fn update(thread: &Thread, sender: Uuid, ts: u64, edit_of: Option<u64>) -> MessageUpdate {
        MessageUpdate { thread: thread.clone(), sender, preview: format!("m{ts}"), ts, edit_of }
    }

    #[test]
    fn live_messages_reorder_the_list_and_mark_unread() {
        let a = thread("11111111-1111-1111-1111-111111111111");
        let b = thread("22222222-2222-2222-2222-222222222222");
        let mut app = App::new(
            vec![
                ThreadEntry::new(a.clone(), "A".to_string(), Some("old".to_string()), 100),
                ThreadEntry::new(b.clone(), "B".to_string(), None, 50),
            ],
            Some(me()),
            PathBuf::from("/nonexistent"),
        );

        // A newer message from someone else moves the thread up and marks it unread.
        assert!(app.apply_update(update(&b, other(), 200, None)).is_none());
        assert_eq!(app.threads[0].name, "B");
        assert!(app.threads[0].unread);
        assert_eq!(app.threads[0].last_preview.as_deref(), Some("m200"));

        // An own message updates the preview without marking unread.
        app.apply_update(update(&a, me(), 300, None));
        assert_eq!(app.threads[0].name, "A");
        assert!(!app.threads[0].unread);

        // An older message arriving late does not replace the preview.
        app.apply_update(update(&a, other(), 250, None));
        assert_eq!(app.threads[0].last_preview.as_deref(), Some("m300"));

        // An edit of the newest message changes the preview only; of an older one, nothing.
        app.apply_update(MessageUpdate { thread: a.clone(), sender: other(), preview: "edited".to_string(), ts: 400, edit_of: Some(300) });
        assert_eq!(app.threads[0].last_preview.as_deref(), Some("edited"));
        assert_eq!(app.threads[0].last_ts, 300);
        app.apply_update(MessageUpdate { thread: a.clone(), sender: other(), preview: "stale".to_string(), ts: 401, edit_of: Some(100) });
        assert_eq!(app.threads[0].last_preview.as_deref(), Some("edited"));

        // An unknown thread comes back to be added by name; an edit for one does not.
        let c = thread("33333333-3333-3333-3333-333333333333");
        assert!(app.apply_update(update(&c, other(), 500, Some(1))).is_none());
        let pending = app.apply_update(update(&c, other(), 500, None)).unwrap();
        app.add_thread(pending, "C".to_string());
        assert_eq!(app.threads[0].name, "C");
        assert!(app.threads[0].unread);
        app.mark_read(&c);
        assert!(!app.threads[0].unread);
    }
}
