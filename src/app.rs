use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use crate::signal::ThreadEntry;

pub struct App {
    pub threads: Vec<ThreadEntry>,
    pub list_state: ListState,
    pub quit: bool,
}

impl App {
    pub fn new(threads: Vec<ThreadEntry>) -> Self {
        let mut list_state = ListState::default();
        if !threads.is_empty() {
            list_state.select(Some(0));
        }
        Self { threads, list_state, quit: false }
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

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Down => {
                let next = self.selected().map(|i| i + 1).unwrap_or(0);
                self.select(next);
            }
            KeyCode::Up => {
                let prev = self.selected().and_then(|i| i.checked_sub(1)).unwrap_or(0);
                self.select(prev);
            }
            KeyCode::PageDown => {
                let next = self.selected().map(|i| i + 10).unwrap_or(0);
                self.select(next);
            }
            KeyCode::PageUp => {
                let prev = self.selected().and_then(|i| i.checked_sub(10)).unwrap_or(0);
                self.select(prev);
            }
            KeyCode::Enter => {
                // Phase 4: open chat window
            }
            KeyCode::Char('d') => {
                // Phase 4: delete with confirmation
            }
            _ => {}
        }
    }
}

pub async fn run(threads: Vec<ThreadEntry>) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(threads);
    let mut events = EventStream::new();

    let result: anyhow::Result<()> = async {
        loop {
            terminal.draw(|f| crate::ui::draw(f, &mut app))?;

            match events.next().await {
                Some(Ok(Event::Key(key))) => app.on_key(key),
                Some(Err(e)) => return Err(anyhow::anyhow!(e)),
                _ => {}
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
