use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    draw_chat_list(f, app, chunks[0]);
    draw_status_bar(f, chunks[1]);
}

fn draw_chat_list(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .threads
        .iter()
        .map(|t| {
            let prefix = if t.unread { "* " } else { "  " };
            let text = match t.last_preview.as_deref() {
                Some(p) if !p.is_empty() => {
                    let first_line = p.lines().next().unwrap_or(p);
                    format!("{}{}: {}", prefix, t.name, first_line)
                }
                _ => format!("{}{}", prefix, t.name),
            };
            ListItem::new(text)
        })
        .collect();

    let empty_msg;
    let list = if items.is_empty() {
        empty_msg = vec![ListItem::new("(no chats)")];
        List::new(empty_msg)
    } else {
        List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .scroll_padding(1)
    };

    let list = list.block(Block::default());
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_status_bar(f: &mut Frame, area: ratatui::layout::Rect) {
    let bar = Paragraph::new("  ↑↓ navigate   PgUp/PgDn scroll   Enter open   Q quit")
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(bar, area);
}
