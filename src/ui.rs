use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::Frame;
use presage::store::ContentExt;

use crate::app::{App, View};
use crate::signal;

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.view {
        View::ChatList => draw_chat_list_screen(f, app),
        View::ChatWindow => draw_chat_window_screen(f, app),
    }
}

// ── Chat list ────────────────────────────────────────────────────────────────

fn draw_chat_list_screen(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());
    draw_thread_list(f, app, chunks[0]);
    draw_status_bar(f, chunks[1], "  ↑↓ navigate   PgUp/PgDn scroll   Enter open   Q quit");
}

fn draw_thread_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .threads
        .iter()
        .map(|t| {
            let prefix = if t.unread { "* " } else { "  " };
            let text = match t.last_preview.as_deref() {
                Some(p) if !p.is_empty() => {
                    let collapsed = p.lines().collect::<Vec<_>>().join(" ");
                    format!("{}{}: {}", prefix, t.name, collapsed)
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

    f.render_stateful_widget(list.block(Block::default()), area, &mut app.list_state);
}

// ── Chat window ───────────────────────────────────────────────────────────────

fn draw_chat_window_screen(f: &mut Frame, app: &mut App) {
    let input_lines = app
        .chat
        .as_ref()
        .map(|c| c.input.lines().count().max(1) as u16)
        .unwrap_or(1);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(input_lines + 1), // +1 for top border
        ])
        .split(f.area());

    draw_chat_header(f, app, chunks[0]);
    draw_messages(f, app, chunks[1]);
    draw_status_bar(f, chunks[2], "  ↑↓ scroll   PgUp/PgDn   Esc back   Enter send   Shift+Enter newline");
    draw_input(f, app, chunks[3]);
}

fn draw_chat_header(f: &mut Frame, app: &App, area: Rect) {
    let name = app.chat.as_ref().map(|c| c.thread_name.as_str()).unwrap_or("");
    let header = Paragraph::new(format!(" {}", name))
        .style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD));
    f.render_widget(header, area);
}

fn draw_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let chat = match app.chat.as_mut() {
        Some(c) => c,
        None => return,
    };

    chat.viewport_height = area.height;

    if chat.messages.is_empty() {
        let p = Paragraph::new("(no messages yet)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }

    let own_aci = app.own_aci;
    let is_group = matches!(chat.thread, presage::store::Thread::Group(_));
    let thread_name = chat.thread_name.clone();

    let mut lines: Vec<Line> = Vec::new();
    let mut prev_ts: Option<u64> = None;
    let mut prev_sender_id: Option<String> = None;

    for content in &chat.messages {
        let ts = content.timestamp();
        let sender_uuid = content.metadata.sender.raw_uuid();
        let is_own = own_aci.map(|a| a == sender_uuid).unwrap_or(false);

        let sender_label = if is_own {
            "You".to_string()
        } else if is_group {
            sender_uuid.to_string()
        } else {
            thread_name.clone()
        };

        // Timestamp separator when gap > 1 hour
        if let Some(prev) = prev_ts {
            if ts.saturating_sub(prev) > 3_600_000 {
                let sep = format!("── {} ──", fmt_ts_long(ts));
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    format!("{:^width$}", sep, width = area.width as usize),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::raw(""));
                prev_sender_id = None;
            }
        }
        prev_ts = Some(ts);

        // Sender block header when sender changes
        let sender_key = format!("{}/{}", sender_label, is_own);
        if prev_sender_id.as_deref() != Some(&sender_key) {
            if prev_sender_id.is_some() {
                lines.push(Line::raw(""));
            }
            let sender_style = if is_own {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            };
            lines.push(Line::from(vec![
                Span::styled(sender_label.clone(), sender_style),
                Span::styled(
                    format!("  {}", fmt_ts_short(ts)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            prev_sender_id = Some(sender_key);
        }

        // Message body — indent each line
        let body = signal::message_body(content);
        for text_line in body.lines() {
            lines.push(Line::from(format!("  {}", text_line)));
        }
    }

    let total = lines.len();
    let height = area.height as usize;

    // Clamp scroll so we can't scroll past the top
    let max_scroll = total.saturating_sub(height);
    if chat.scroll > max_scroll {
        chat.scroll = max_scroll;
    }

    let scroll_row = total.saturating_sub(height).saturating_sub(chat.scroll) as u16;

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text).scroll((scroll_row, 0));
    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let input = app.chat.as_ref().map(|c| c.input.as_str()).unwrap_or("");
    let block = Block::default()
        .borders(ratatui::widgets::Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut text_lines: Vec<Line> = Vec::new();
    let display_lines: Vec<&str> = if input.is_empty() {
        vec![""]
    } else {
        input.lines().collect()
    };
    for (i, line) in display_lines.iter().enumerate() {
        let prefix = if i == 0 {
            Span::styled("> ", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled("  ", Style::default())
        };
        text_lines.push(Line::from(vec![prefix, Span::raw(*line)]));
    }
    let paragraph = Paragraph::new(Text::from(text_lines));
    f.render_widget(paragraph, inner);
}

// ── Shared ────────────────────────────────────────────────────────────────────

fn draw_status_bar(f: &mut Frame, area: Rect, text: &str) {
    let bar = Paragraph::new(text)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(bar, area);
}

fn fmt_ts_short(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

fn fmt_ts_long(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}
