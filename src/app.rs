use std::collections::HashSet;
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

use crate::signal::{self, ThreadEntry};

pub enum View {
    ChatList,
    ChatWindow,
    ContactBrowser,
}

pub struct ChatState {
    pub thread: Thread,
    pub thread_name: String,
    pub messages: Vec<Content>,
    pub scroll: usize,
    pub viewport_height: u16,
    pub input: String,
    pub cursor: usize,                       // byte offset into `input`
    pub selected_message: Option<usize>,     // index into `messages`
    pub delivered: HashSet<u64>,             // timestamps of our messages confirmed delivered
    pub read: HashSet<u64>,                  // timestamps of our messages confirmed read
    pub tab_pressed: bool,                   // true if the last keypress was Tab
    pub autocomplete_hint: Option<String>,   // shown on status bar after double-Tab
}

pub enum AppCmd {
    OpenChat { thread: Thread, name: String },
    OpenContactBrowser,
    RefreshThreadList,
    SendMessage,
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
    pub contacts_split: usize,           // how many entries in `contacts` are Contact threads
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
    ) {
        self.chat = Some(ChatState {
            thread,
            thread_name,
            messages,
            scroll: 0,
            viewport_height: 0,
            input: String::new(),
            cursor: 0,
            selected_message: None,
            delivered,
            read,
            tab_pressed: false,
            autocomplete_hint: None,
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
            KeyCode::Down => {
                let next = self.selected().map(|i| i + 1).unwrap_or(0);
                self.select(next);
                None
            }
            KeyCode::Up => {
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
            KeyCode::Enter => {
                self.selected().and_then(|idx| {
                    let t = self.threads.get(idx)?;
                    Some(AppCmd::OpenChat { thread: t.thread.clone(), name: t.name.clone() })
                })
            }
            _ => None,
        }
    }

    fn on_key_chat(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        // Esc is handled before borrowing `chat` because its two branches
        // either mutate chat.selected_message OR set self.chat = None — the
        // borrow checker can't elide a `chat` borrow that's used in one branch
        // while `self.chat = None` fires in the other.
        if key.code == KeyCode::Esc {
            let has_selection = self.chat.as_ref().map(|c| c.selected_message.is_some()).unwrap_or(false);
            if has_selection {
                if let Some(chat) = &mut self.chat {
                    chat.selected_message = None;
                    chat.tab_pressed = false;
                    chat.autocomplete_hint = None;
                }
                return None;
            } else {
                self.chat = None;
                return Some(AppCmd::RefreshThreadList);
            }
        }

        let chat = self.chat.as_mut()?;

        // Reset autocomplete state on any key that isn't Tab.
        if key.code != KeyCode::Tab {
            chat.tab_pressed = false;
            chat.autocomplete_hint = None;
        }

        match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if !chat.messages.is_empty() {
                    chat.selected_message = Some(
                        chat.selected_message
                            .map(|s| s.saturating_sub(1))
                            .unwrap_or(chat.messages.len() - 1),
                    );
                }
                None
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if let Some(sel) = chat.selected_message {
                    let max = chat.messages.len().saturating_sub(1);
                    if sel < max {
                        chat.selected_message = Some(sel + 1);
                    }
                }
                None
            }
            KeyCode::Left => {
                chat.cursor = cursor_left(&chat.input, chat.cursor);
                None
            }
            KeyCode::Right => {
                chat.cursor = cursor_right(&chat.input, chat.cursor);
                None
            }
            KeyCode::Up => {
                chat.cursor = cursor_up(&chat.input, chat.cursor);
                None
            }
            KeyCode::Down => {
                chat.cursor = cursor_down(&chat.input, chat.cursor);
                None
            }
            KeyCode::PageUp => {
                let h = chat.viewport_height as usize;
                chat.scroll = chat.scroll.saturating_add(h);
                None
            }
            KeyCode::PageDown => {
                let h = chat.viewport_height as usize;
                chat.scroll = chat.scroll.saturating_sub(h);
                None
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                chat.input.insert(chat.cursor, '\n');
                chat.cursor += 1;
                None
            }
            KeyCode::Enter => {
                if chat.input.trim() == "/quit" {
                    self.quit = true;
                    return None;
                }
                if !chat.input.trim().is_empty() {
                    Some(AppCmd::SendMessage)
                } else {
                    None
                }
            }
            KeyCode::Backspace => {
                if chat.cursor > 0 {
                    let new_cursor = cursor_left(&chat.input, chat.cursor);
                    chat.input.remove(new_cursor);
                    chat.cursor = new_cursor;
                }
                None
            }
            // Ctrl+H is the terminal-conventional backspace (0x08). Some
            // terminal emulators (including OpenBSD's) send it for Shift+Backspace.
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if chat.cursor > 0 {
                    let new_cursor = cursor_left(&chat.input, chat.cursor);
                    chat.input.remove(new_cursor);
                    chat.cursor = new_cursor;
                }
                None
            }
            KeyCode::Char(c) => {
                chat.input.insert(chat.cursor, c);
                chat.cursor += c.len_utf8();
                None
            }
            KeyCode::Tab => {
                let was_tab = chat.tab_pressed;
                chat.tab_pressed = true;
                // Field borrow splitting: self.threads is separate from self.chat.
                let threads = &self.threads;
                if let Some((start, end, candidates)) =
                    completion_candidates(&chat.input, chat.cursor, threads)
                {
                    if candidates.len() == 1 {
                        let rep = candidates[0].clone();
                        chat.input.replace_range(start..end, &rep);
                        chat.cursor = start + rep.len();
                        chat.autocomplete_hint = None;
                        chat.tab_pressed = false;
                    } else if was_tab {
                        chat.autocomplete_hint = Some(candidates.join("  "));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn on_key_contacts(&mut self, key: crossterm::event::KeyEvent) -> Option<AppCmd> {
        // Whether a separator row exists between contacts and groups.
        let has_sep = self.contacts_split > 0 && self.contacts_split < self.contacts.len();
        let total = self.contacts.len() + if has_sep { 1 } else { 0 };
        let sep = self.contacts_split; // display index of the separator row

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                Some(AppCmd::RefreshThreadList)
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                None
            }
            KeyCode::Down => {
                let cur = self.contact_list_state.selected().unwrap_or(0);
                let mut next = (cur + 1).min(total.saturating_sub(1));
                if has_sep && next == sep { next = (next + 1).min(total - 1); }
                self.contact_list_state.select(Some(next));
                None
            }
            KeyCode::Up => {
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
            KeyCode::Enter => {
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

        if self.chat.as_ref().map(|c| c.thread == update.thread).unwrap_or(false) {
            if let Some(chat) = &mut self.chat {
                chat.messages.push(*boxed);
            }
        }
    }
}

// ── Tab completion ────────────────────────────────────────────────────────────

/// Returns (replace_start_byte, replace_end_byte, candidates) or None.
///
/// Slash commands: triggered when input[..cursor] starts with '/' and has no space.
/// @mentions:      triggered when a bare '@' token ends at cursor.
fn completion_candidates(
    input: &str,
    cursor: usize,
    threads: &[ThreadEntry],
) -> Option<(usize, usize, Vec<String>)> {
    let before = &input[..cursor.min(input.len())];

    if before.starts_with('/') && !before.contains(' ') {
        let partial = &before[1..];
        // (name, needs_trailing_space_for_arg)
        let commands = [("quit", false), ("react", true), ("reply", true)];
        let mut candidates: Vec<String> = commands
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(partial))
            .map(|(cmd, has_arg)| if *has_arg { format!("/{} ", cmd) } else { format!("/{}", cmd) })
            .collect();
        candidates.sort();
        if candidates.is_empty() { return None; }
        return Some((0, cursor, candidates));
    }

    if let Some(at_pos) = before.rfind('@') {
        let partial = &before[at_pos + 1..];
        if !partial.contains(' ') {
            let partial_lower = partial.to_lowercase();
            let mut candidates: Vec<String> = threads
                .iter()
                .filter(|t| matches!(t.thread, Thread::Contact(_)))
                .filter(|t| t.name != "Note to Self")
                .filter(|t| t.name.to_lowercase().starts_with(&partial_lower))
                .map(|t| format!("@{} ", t.name))
                .collect();
            candidates.sort();
            if candidates.is_empty() { return None; }
            return Some((at_pos, cursor, candidates));
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

async fn poll_stream(stream: &mut Option<Pin<Box<dyn Stream<Item = Received>>>>) -> Option<Received> {
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

async fn execute_cmd<S: Store>(
    app: &mut App,
    manager: &mut Manager<S, Registered>,
    cmd: AppCmd,
) -> anyhow::Result<()> {
    match cmd {
        AppCmd::OpenChat { thread, name } => {
            let messages = signal::load_messages(manager, &thread).await?;
            let (delivered, read) = signal::load_receipt_state(manager, &thread)
                .await
                .unwrap_or_default();

            let own_aci = app.own_aci;
            let to_ack: Vec<u64> = messages
                .iter()
                .filter(|m| own_aci.map(|a| a != m.metadata.sender.raw_uuid()).unwrap_or(true))
                .map(|m| m.timestamp())
                .collect();

            app.open_chat(thread.clone(), name, messages, delivered, read);

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
            // Capture everything we need before mutably borrowing the manager.
            let (text, thread, quote_info) = {
                let chat = app.chat.as_mut().expect("SendMessage with no open chat");
                chat.cursor = 0;
                // Snapshot quote context from the selected message before clearing it.
                let quote_info = chat.selected_message.and_then(|idx| {
                    let msg = chat.messages.get(idx)?;
                    Some((
                        msg.timestamp(),
                        msg.metadata.sender.raw_uuid(),
                        signal::message_body(msg),
                    ))
                });
                chat.selected_message = None;
                (std::mem::take(&mut chat.input), chat.thread.clone(), quote_info)
            };

            let trimmed = text.trim();
            if !trimmed.is_empty() {
                if let Some(reply_body) = trimmed.strip_prefix("/reply").map(str::trim) {
                    if !reply_body.is_empty() {
                        if let Some((q_ts, q_author, q_text)) = quote_info {
                            signal::send_reply(manager, &thread, reply_body.to_string(), q_ts, q_author, q_text).await?;
                        }
                        // /reply with no active selection: silently drop — no orphan reply.
                    }
                    // /reply with no body: silently drop.
                } else {
                    signal::send_to_thread(manager, &thread, trimmed.to_string()).await?;
                }
                // Reload immediately — presage writes the sent message to the local
                // store before returning, so it's available here without waiting for
                // the SyncMessage echo (which may arrive on a dying stream).
                if let Ok(msgs) = signal::load_messages(manager, &thread).await {
                    if let Some(chat) = &mut app.chat {
                        chat.messages = msgs;
                    }
                }
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
    stream: Pin<Box<dyn Stream<Item = Received>>>,
) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(threads, own_aci, data_dir);
    let mut events = EventStream::new();
    let mut signal_stream: Option<Pin<Box<dyn Stream<Item = Received>>>> = Some(stream);

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
                        Some(Err(e)) => return Err(anyhow::anyhow!(e)),
                        _ => {}
                    }
                }
                event = poll_stream(&mut signal_stream) => {
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

                                if let Ok(msgs) = signal::load_messages(&manager, &thread).await {
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
                        // Stream closed — reconnect.
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
        crossterm::terminal::LeaveAlternateScreen
    )?;

    result
}
