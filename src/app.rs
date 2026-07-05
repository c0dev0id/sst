use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::{Stream, StreamExt};
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::Content;
use presage::libsignal_service::prelude::Uuid;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use crate::signal::{self, ReactionMap, ThreadEntry};

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

pub struct ChatState {
    pub thread: Thread,
    pub thread_name: String,
    pub messages: Vec<Content>,
    pub scroll: usize,
    pub viewport_height: u16,
    pub viewport_top_msg: usize,            // index of first visible message, written by renderer
    pub input: String,
    pub cursor: usize,                     // byte offset into `input`
    pub selected_message: Option<usize>,   // index into `messages`
    pub delivered: HashSet<u64>,           // timestamps of our messages confirmed delivered
    pub read: HashSet<u64>,                // timestamps of our messages confirmed read
    pub autocomplete_hint: Option<String>, // shown on status bar after Tab
    pub reactions: ReactionMap,
    pub mode: Mode,
    pub reply_to: Option<usize>,           // message index set by 'r' in Normal mode
    pub editing: Option<(usize, u64)>,     // (message index, timestamp) set by 'e' in Normal mode
    pub sender_names: HashMap<Uuid, String>, // resolved names for sender display and @mention
    pub pending_d: bool,                   // true after first 'd'; second 'd' triggers delete
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

    pub fn open_chat(
        &mut self,
        thread: Thread,
        thread_name: String,
        messages: Vec<Content>,
        delivered: HashSet<u64>,
        read: HashSet<u64>,
        reactions: ReactionMap,
        sender_names: HashMap<Uuid, String>,
    ) {
        self.chat = Some(ChatState {
            thread,
            thread_name,
            messages,
            scroll: 0,
            viewport_height: 0,
            viewport_top_msg: 0,
            input: String::new(),
            cursor: 0,
            selected_message: None,
            delivered,
            read,
            autocomplete_hint: None,
            reactions,
            mode: Mode::Normal,
            reply_to: None,
            editing: None,
            sender_names,
            pending_d: false,
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
        if !matches!(chat.mode, Mode::Insert) { return }
        chat.input.insert_str(chat.cursor, &text);
        chat.cursor += text.len();
    }

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match self.view {
            View::ChatList => self.on_key_list(key),
            View::ChatWindow => self.on_key_chat(key),
            View::ContactBrowser => self.on_key_contacts(key),
        }
    }

    fn on_key_list(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => { self.quit = true; None }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = self.selected().map(|i| i + 1).unwrap_or(0);
                self.select(next);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let prev = self.selected().and_then(|i| i.checked_sub(1)).unwrap_or(0);
                self.select(prev);
                None
            }
            KeyCode::PageDown => {
                let next = self.selected().map(|i| i + 10).unwrap_or(0);
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

    fn on_key_chat(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        // Keys that need to set self.chat = None must fire before any borrow of self.chat.
        if matches!(self.chat.as_ref()?.mode, Mode::Normal) {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                    self.chat = None;
                    return Some(AppCmd::RefreshThreadList);
                }
                _ => {}
            }
        }

        // Snapshot own_aci (Copy) before borrowing chat so Normal 'e' can check message ownership.
        let own_aci = self.own_aci;
        let chat = self.chat.as_mut()?;

        if key.code != KeyCode::Tab {
            chat.autocomplete_hint = None;
        }
        if chat.pending_d && (!matches!(chat.mode, Mode::Normal) || key.code != KeyCode::Char('d')) {
            chat.pending_d = false;
        }

        // Snapshot mode discriminant to avoid holding a borrow into chat.mode across mutations.
        let mode_disc = match &chat.mode {
            Mode::Normal => 0u8,
            Mode::Insert => 1,
            Mode::Command(_) => 2,
        };

        match mode_disc {
            0 => { // Normal mode
                match key.code {
                    KeyCode::Esc => {
                        if chat.selected_message.is_some() {
                            chat.selected_message = None;
                            chat.reply_to = None;
                            chat.editing = None;
                        }
                    }
                    KeyCode::Char('i') => {
                        chat.mode = Mode::Insert;
                    }
                    KeyCode::Char(':') => {
                        chat.mode = Mode::Command(String::new());
                    }
                    KeyCode::Char('r') => {
                        if chat.selected_message.is_some() {
                            chat.reply_to = chat.selected_message;
                            chat.cursor = chat.input.len();
                            chat.mode = Mode::Insert;
                        } else {
                            chat.autocomplete_hint = Some(HINT_SELECT_FIRST.to_string());
                        }
                    }
                    KeyCode::Char('e') => {
                        if let Some(sel_idx) = chat.selected_message {
                            if let Some(content) = chat.messages.get(sel_idx) {
                                let is_own = own_aci
                                    .map(|a| a == content.metadata.sender.raw_uuid())
                                    .unwrap_or(false);
                                if is_own {
                                    let body = signal::message_body(content).to_string();
                                    let ts = content.timestamp();
                                    chat.input = body;
                                    chat.cursor = chat.input.len();
                                    chat.editing = Some((sel_idx, ts));
                                    chat.mode = Mode::Insert;
                                } else {
                                    chat.autocomplete_hint =
                                        Some("can only edit own messages".to_string());
                                }
                            }
                        } else {
                            chat.autocomplete_hint =
                                Some(HINT_SELECT_FIRST.to_string());
                        }
                    }
                    KeyCode::Char('d') => {
                        if chat.selected_message.is_none() {
                            chat.autocomplete_hint =
                                Some(HINT_SELECT_FIRST.to_string());
                        } else if chat.pending_d {
                            chat.pending_d = false;
                            return Some(AppCmd::DeleteMessage);
                        } else {
                            chat.pending_d = true;
                            chat.autocomplete_hint = Some("d again to delete for everyone".to_string());
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if !chat.messages.is_empty() {
                            chat.selected_message = Some(
                                chat.selected_message
                                    .map(|s| s.saturating_sub(1))
                                    .unwrap_or(chat.messages.len() - 1),
                            );
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !chat.messages.is_empty() {
                            chat.selected_message = Some(match chat.selected_message {
                                Some(sel) => (sel + 1).min(chat.messages.len() - 1),
                                None => chat.viewport_top_msg,
                            });
                        }
                    }
                    KeyCode::PageUp => {
                        let h = chat.viewport_height as usize;
                        chat.scroll = chat.scroll.saturating_add(h);
                    }
                    KeyCode::PageDown => {
                        let h = chat.viewport_height as usize;
                        chat.scroll = chat.scroll.saturating_sub(h);
                    }
                    _ => {}
                }
            }
            1 => { // Insert mode
                match key.code {
                    KeyCode::Esc => {
                        chat.mode = Mode::Normal;
                    }
                    KeyCode::Left => {
                        chat.cursor = cursor_left(&chat.input, chat.cursor);
                    }
                    KeyCode::Right => {
                        chat.cursor = cursor_right(&chat.input, chat.cursor);
                    }
                    KeyCode::Up => {
                        chat.cursor = cursor_up(&chat.input, chat.cursor);
                    }
                    KeyCode::Down => {
                        chat.cursor = cursor_down(&chat.input, chat.cursor);
                    }
                    KeyCode::PageUp => {
                        let h = chat.viewport_height as usize;
                        chat.scroll = chat.scroll.saturating_add(h);
                    }
                    KeyCode::PageDown => {
                        let h = chat.viewport_height as usize;
                        chat.scroll = chat.scroll.saturating_sub(h);
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        chat.input.insert(chat.cursor, '\n');
                        chat.cursor += 1;
                    }
                    KeyCode::Enter => {
                        if !chat.input.trim().is_empty() {
                            return Some(AppCmd::SendMessage);
                        }
                    }
                    KeyCode::Backspace => {
                        if chat.cursor > 0 {
                            let new_cursor = cursor_left(&chat.input, chat.cursor);
                            chat.input.remove(new_cursor);
                            chat.cursor = new_cursor;
                        }
                    }
                    // Ctrl+H is the terminal-conventional backspace (0x08). Some
                    // terminal emulators (including OpenBSD's) send it for Shift+Backspace.
                    KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                        if let Some((start, end, candidates, display)) =
                            completion_candidates(&chat.input, chat.cursor, &chat.sender_names)
                        {
                            if candidates.len() == 1 {
                                let rep = candidates[0].clone();
                                chat.input.replace_range(start..end, &rep);
                                chat.cursor = start + rep.len();
                                chat.autocomplete_hint = None;
                            } else {
                                let labels = display.as_deref().unwrap_or(&candidates);
                                chat.autocomplete_hint = Some(labels.join("  "));
                            }
                        }
                    }
                    _ => {}
                }
            }
            2 => { // Command mode — clone command text before any mutation
                let cmd_so_far = if let Mode::Command(s) = &chat.mode {
                    s.clone()
                } else {
                    return None;
                };
                match key.code {
                    KeyCode::Esc => {
                        chat.mode = Mode::Normal;
                    }
                    KeyCode::Enter => {
                        chat.mode = Mode::Normal;
                        if !cmd_so_far.is_empty() {
                            return Some(AppCmd::ExecCmd(cmd_so_far));
                        }
                    }
                    KeyCode::Tab => {
                        if !cmd_so_far.contains(' ') {
                            let partial = cmd_so_far.to_lowercase();
                            let matches: Vec<&str> = COLON_COMMANDS
                                .iter()
                                .copied()
                                .filter(|c| c.starts_with(partial.as_str()))
                                .collect();
                            if matches.len() == 1 {
                                chat.mode = Mode::Command(matches[0].to_string());
                                chat.autocomplete_hint = None;
                            } else if !matches.is_empty() {
                                chat.autocomplete_hint = Some(
                                    matches.iter().map(|c| format!(":{}", c)).collect::<Vec<_>>().join("  ")
                                );
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
            }
            _ => {}
        }
        None
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

    pub fn on_signal(&mut self, received: Received) {
        let Received::Content(boxed) = received else { return };
        let Some(update) = signal::extract_update(&boxed) else { return };

        if let Some(entry) = self.threads.iter_mut().find(|e| e.thread == update.thread) {
            entry.last_preview = update.preview.clone();
            if update.ts > entry.last_ts {
                entry.last_ts = update.ts;
                entry.unread = true;
            }
        } else {
            let name = match &update.thread {
                Thread::Contact(sid) => sid.raw_uuid().to_string(),
                Thread::Group(_) => return,
            };
            self.threads.push(ThreadEntry {
                thread: update.thread.clone(),
                name,
                last_preview: update.preview.clone(),
                last_ts: update.ts,
                unread: true,
            });
        }
        self.threads.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    }
}

// ── Colon command registry ────────────────────────────────────────────────────

const COLON_COMMANDS: &[&str] = &["quit", "react"];
const HINT_SELECT_FIRST: &str = "select a message first (j/k)";

enum ColonCmd<'a> {
    Quit,
    React(&'a str), // emoji/shortcode arg; empty string means "show reactions"
}

fn parse_colon_cmd(input: &str) -> Option<ColonCmd<'_>> {
    let s = input.trim().strip_prefix(':')?;
    let (name, arg) = s
        .split_once(' ')
        .map(|(n, a)| (n, a.trim()))
        .unwrap_or((s, ""));
    match name {
        "quit"  => Some(ColonCmd::Quit),
        "react" => Some(ColonCmd::React(arg)),
        _       => None,
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

// Returns (replace_start, replace_end, completion_values, display_labels).
// display_labels is Some when the hint text should differ from the completion values.
fn completion_candidates(
    input: &str,
    cursor: usize,
    sender_names: &HashMap<Uuid, String>,
) -> Option<(usize, usize, Vec<String>, Option<Vec<String>>)> {
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
            return Some((at_pos, cursor, candidates, None));
        }
    }

    None
}

// ── Cursor movement helpers ───────────────────────────────────────────────────

fn cursor_left(input: &str, cursor: usize) -> usize {
    if cursor == 0 { return 0; }
    input[..cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
}

fn cursor_right(input: &str, cursor: usize) -> usize {
    if cursor >= input.len() { return input.len(); }
    cursor + input[cursor..].chars().next().map(|c| c.len_utf8()).unwrap_or(0)
}

// Returns (line_index, visual_col) for a byte cursor position.
fn cursor_line_col(input: &str, cursor: usize) -> (usize, usize) {
    let before = &input[..cursor.min(input.len())];
    let parts: Vec<&str> = before.split('\n').collect();
    let line = parts.len().saturating_sub(1);
    let col = parts.last().map(|l| l.chars().count()).unwrap_or(0);
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
        .map(|(b, _)| b)
        .unwrap_or(prev_line.len());
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
        .map(|(b, _)| b)
        .unwrap_or(next_line.len());
    line_byte_start(input, line + 1) + byte_in_line
}

// ─────────────────────────────────────────────────────────────────────────────

async fn next_or_pending(stream: &mut Option<Pin<Box<dyn Stream<Item = Received>>>>) -> Option<Received> {
    match stream {
        Some(s) => {
            let v = s.next().await;
            if v.is_none() {
                *stream = None;
            }
            v
        }
        None => std::future::pending().await,
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
    if let Ok((msgs, rxns)) = signal::load_messages_and_reactions(manager, thread).await {
        if let Some(c) = chat {
            c.messages = msgs;
            c.reactions = rxns;
        }
    }
}

async fn execute_cmd<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    cmd: AppCmd,
) -> anyhow::Result<()> {
    match cmd {
        AppCmd::OpenChat { thread, name } => {
            let (messages, reactions) = signal::load_messages_and_reactions(manager, &thread).await?;
            let (delivered, read) = signal::load_receipt_state(manager, &thread)
                .await
                .unwrap_or_default();
            let sender_names = signal::load_sender_names(manager, &thread, app.own_aci).await;

            let own_aci = app.own_aci;
            let to_ack: Vec<u64> = messages
                .iter()
                .filter(|m| own_aci.map(|a| a != m.metadata.sender.raw_uuid()).unwrap_or(true))
                .map(|m| m.timestamp())
                .collect();

            app.open_chat(thread.clone(), name, messages, delivered, read, reactions, sender_names);

            if let Err(e) = signal::send_read_receipt(manager, &thread, to_ack).await {
                tracing::warn!("send_read_receipt: {e}");
            }
        }
        AppCmd::OpenContactBrowser => {
            let (contacts, split) = signal::list_all_contacts(manager, app.own_aci).await?;
            app.contacts = contacts;
            app.contacts_split = split;
            let mut state = ListState::default();
            if !app.contacts.is_empty() {
                state.select(Some(0));
            }
            app.contact_list_state = state;
            app.view = View::ContactBrowser;
        }
        AppCmd::RefreshThreadList => {
            let threads = signal::list_threads(manager, &app.data_dir, app.own_aci).await?;
            app.threads = threads;
            // Clamp selection so it stays valid after the list changes.
            let new_len = app.threads.len();
            match app.list_state.selected() {
                Some(sel) if new_len > 0 => app.list_state.select(Some(sel.min(new_len - 1))),
                None if new_len > 0 => app.list_state.select(Some(0)),
                _ => {}
            }
            app.view = View::ChatList;
        }
        AppCmd::SendMessage => {
            let (text, thread, reply_info, editing) = {
                let chat = app.chat.as_mut().expect("SendMessage with no open chat");
                chat.cursor = 0;
                let reply_info = chat.reply_to.and_then(|idx| {
                    let msg = chat.messages.get(idx)?;
                    Some((
                        msg.timestamp(),
                        msg.metadata.sender.raw_uuid(),
                        signal::message_body(msg),
                    ))
                });
                let editing = chat.editing.take();
                chat.reply_to = None;
                (std::mem::take(&mut chat.input), chat.thread.clone(), reply_info, editing)
            };

            let trimmed = text.trim();
            if !trimmed.is_empty() {
                if let Some((_idx, edit_ts)) = editing {
                    signal::send_edit(manager, &thread, edit_ts, trimmed.to_string()).await?;
                    if let Some(c) = &mut app.chat { c.selected_message = None; }
                } else if let Some((q_ts, q_author, q_text)) = reply_info {
                    signal::send_reply(manager, &thread, trimmed.to_string(), q_ts, q_author, q_text).await?;
                    if let Some(c) = &mut app.chat { c.selected_message = None; }
                } else {
                    signal::send_to_thread(manager, &thread, trimmed.to_string()).await?;
                }
                reload_chat(manager, &thread, &mut app.chat).await;
            }
        }
        AppCmd::ExecCmd(cmd_text) => {
            let colon_input = format!(":{}", cmd_text);
            let quote_info = app.chat.as_ref().and_then(|c| {
                let idx = c.selected_message?;
                let msg = c.messages.get(idx)?;
                Some((
                    msg.timestamp(),
                    msg.metadata.sender.raw_uuid(),
                    signal::message_body(msg),
                ))
            });
            let thread = match app.chat.as_ref().map(|c| c.thread.clone()) {
                Some(t) => t,
                None => return Ok(()),
            };

            match parse_colon_cmd(&colon_input) {
                Some(ColonCmd::Quit) => {
                    app.quit = true;
                }
                Some(ColonCmd::React(arg)) if arg.is_empty() => {
                    let hint = if let Some((target_ts, _, _)) = quote_info {
                        if let Some(chat) = &app.chat {
                            reaction_hint(&chat.reactions, target_ts)
                        } else {
                            String::new()
                        }
                    } else {
                        "no message selected".to_string()
                    };
                    if let Some(chat) = &mut app.chat {
                        chat.autocomplete_hint = Some(hint);
                    }
                }
                Some(ColonCmd::React(arg)) => {
                    if let Some((target_ts, target_author, _)) = quote_info {
                        if let Some(emoji) = resolve_emoji(arg) {
                            let remove = app.own_aci
                                .and_then(|u| {
                                    app.chat.as_ref()?.reactions.get(&target_ts)?.get(&emoji)
                                        .map(|s| s.contains(u.as_bytes()))
                                })
                                .unwrap_or(false);
                            signal::send_reaction(manager, &thread, emoji, target_ts, target_author, remove).await?;
                            reload_chat(manager, &thread, &mut app.chat).await;
                        } else if let Some(chat) = &mut app.chat {
                            chat.autocomplete_hint = Some(format!("unknown emoji: {}", arg));
                        }
                    } else if let Some(chat) = &mut app.chat {
                        chat.autocomplete_hint = Some("no message selected".to_string());
                    }
                }
                None => {
                    if let Some(chat) = &mut app.chat {
                        chat.autocomplete_hint = Some(format!("unknown command: :{}", cmd_text));
                    }
                }
            }
        }
        AppCmd::DeleteMessage => {
            let (thread, timestamp, sel_idx, is_own) = match app.chat.as_ref() {
                Some(chat) => {
                    let idx = match chat.selected_message {
                        Some(i) => i,
                        None => return Ok(()),
                    };
                    let msg = match chat.messages.get(idx) {
                        Some(m) => m,
                        None => return Ok(()),
                    };
                    let own = app.own_aci
                        .map(|a| a == msg.metadata.sender.raw_uuid())
                        .unwrap_or(false);
                    (chat.thread.clone(), msg.timestamp(), idx, own)
                }
                None => return Ok(()),
            };
            if !is_own {
                if let Some(chat) = &mut app.chat {
                    chat.autocomplete_hint = Some("can only delete own messages".to_string());
                }
                return Ok(());
            }
            signal::delete_for_everyone(manager, &thread, timestamp).await?;
            if let Some(chat) = &mut app.chat {
                chat.messages.remove(sel_idx);
                let new_len = chat.messages.len();
                chat.selected_message = if new_len == 0 {
                    None
                } else {
                    Some(sel_idx.min(new_len - 1))
                };
            }
        }
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
                            if let Some(cmd) = app.on_key(key) {
                                execute_cmd(&mut app, &mut manager, cmd).await?;
                            }
                        }
                        Some(Ok(Event::Paste(text))) => {
                            app.on_paste(text);
                        }
                        Some(Err(e)) => return Err(anyhow::anyhow!(e)),
                        _ => {}
                    }
                }
                event = next_or_pending(&mut signal_stream) => {
                    if let Some(received) = event {
                        app.on_signal(received);
                        // Reload chat messages and receipt state on any incoming event.
                        if matches!(app.view, View::ChatWindow) {
                            if let Some(thread) = app.chat.as_ref().map(|c| c.thread.clone()) {
                                // Snapshot known timestamps before reload so we only
                                // ack messages that weren't visible when the chat opened.
                                let known: HashSet<u64> = app.chat.as_ref()
                                    .map(|c| c.messages.iter().map(|m| m.timestamp()).collect())
                                    .unwrap_or_default();

                                if let Ok((msgs, reactions)) = signal::load_messages_and_reactions(&manager, &thread).await {
                                    let receipt_state = signal::load_receipt_state(&manager, &thread).await.ok();

                                    let own_aci = app.own_aci;
                                    let to_ack: Vec<u64> = msgs
                                        .iter()
                                        .filter(|m| !known.contains(&m.timestamp()))
                                        .filter(|m| own_aci.map(|a| a != m.metadata.sender.raw_uuid()).unwrap_or(true))
                                        .map(|m| m.timestamp())
                                        .collect();

                                    if let Some(chat) = &mut app.chat {
                                        chat.messages = msgs;
                                        if let Some((del, rd)) = receipt_state {
                                            chat.delivered = del;
                                            chat.read = rd;
                                        }
                                        chat.reactions = reactions;
                                    }

                                    if !to_ack.is_empty() {
                                        if let Err(e) = signal::send_read_receipt(&mut manager, &thread, to_ack).await {
                                            tracing::warn!("send_read_receipt: {e}");
                                        }
                                    }
                                }
                            }
                        }
                    } else {
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
