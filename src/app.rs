use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{Store, Thread};
use presage::libsignal_service::content::Content;
use presage::libsignal_service::prelude::Uuid;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::signal::{self, ThreadEntry};

pub enum View {
    ChatList,
    ChatWindow,
}

pub struct ChatState {
    pub thread: Thread,
    pub thread_name: String,
    pub messages: Vec<Content>,
    pub scroll: usize,
    pub viewport_height: u16,
    pub input: String,
    pub cursor: usize,           // byte offset into `input`
    pub selected_message: Option<usize>, // index into `messages`
}

pub enum AppCmd {
    OpenChat(usize),
    SendMessage,
}

pub struct App {
    pub threads: Vec<ThreadEntry>,
    pub list_state: ListState,
    pub quit: bool,
    pub view: View,
    pub chat: Option<ChatState>,
    pub own_aci: Option<Uuid>,
}

impl App {
    pub fn new(threads: Vec<ThreadEntry>, own_aci: Option<Uuid>) -> Self {
        let mut list_state = ListState::default();
        if !threads.is_empty() {
            list_state.select(Some(0));
        }
        Self { threads, list_state, quit: false, view: View::ChatList, chat: None, own_aci }
    }

    pub fn open_chat(&mut self, thread: Thread, thread_name: String, messages: Vec<Content>) {
        self.chat = Some(ChatState {
            thread,
            thread_name,
            messages,
            scroll: 0,
            viewport_height: 0,
            input: String::new(),
            cursor: 0,
            selected_message: None,
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
            KeyCode::Enter => self.selected().map(AppCmd::OpenChat),
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
                }
            } else {
                self.view = View::ChatList;
                self.chat = None;
            }
            return None;
        }

        let chat = self.chat.as_mut()?;
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
            KeyCode::Char(c) => {
                chat.input.insert(chat.cursor, c);
                chat.cursor += c.len_utf8();
                None
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

async fn next_signal(rx: &mut Option<mpsc::Receiver<Received>>) -> Option<Received> {
    match rx {
        Some(r) => {
            let v = r.recv().await;
            if v.is_none() {
                *rx = None;
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
        AppCmd::OpenChat(idx) => {
            let thread = app.threads[idx].thread.clone();
            let name = app.threads[idx].name.clone();
            let messages = signal::load_messages(manager, &thread).await?;
            app.open_chat(thread, name, messages);
        }
        AppCmd::SendMessage => {
            let (text, thread) = {
                let chat = app.chat.as_mut().expect("SendMessage with no open chat");
                chat.cursor = 0;
                (std::mem::take(&mut chat.input), chat.thread.clone())
            };
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                signal::send_to_thread(manager, &thread, trimmed).await?;
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
    mut manager: Manager<S, Registered>,
    rx: mpsc::Receiver<Received>,
) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(threads, own_aci);
    let mut events = EventStream::new();
    let mut rx = Some(rx);

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
                event = next_signal(&mut rx) => {
                    if let Some(received) = event {
                        app.on_signal(received);
                        // Reload chat messages from the store on any incoming event
                        // so new messages (including our own echoed sends) appear immediately.
                        if matches!(app.view, View::ChatWindow) {
                            if let Some(thread) = app.chat.as_ref().map(|c| c.thread.clone()) {
                                if let Ok(msgs) = signal::load_messages(&manager, &thread).await {
                                    if let Some(chat) = &mut app.chat {
                                        chat.messages = msgs;
                                    }
                                }
                            }
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
