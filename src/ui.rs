use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::Frame;
use presage::store::ContentExt;

use crate::app::{App, Mode, View};
use crate::signal;

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.view {
        View::ChatList => draw_chat_list_screen(f, app),
        View::ChatWindow => draw_chat_window_screen(f, app),
        View::ContactBrowser => draw_contact_browser_screen(f, app),
    }
}

// ── Chat list ────────────────────────────────────────────────────────────────

fn draw_chat_list_screen(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());
    draw_thread_list(f, app, chunks[0]);
    draw_status_bar(f, chunks[1], "  j/k/↑↓ navigate   PgUp/PgDn scroll   l/Enter/→ open   n new chat   Q quit");
}

fn draw_thread_list(f: &mut Frame, app: &mut App, area: Rect) {
    let max_width = area.width as usize;
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
            let text = if text.chars().count() > max_width {
                let truncated: String = text.chars().take(max_width.saturating_sub(1)).collect();
                format!("{}…", truncated)
            } else {
                text
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

// ── Contact browser ───────────────────────────────────────────────────────────

fn draw_contact_browser_screen(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let header = Paragraph::new(" New Chat")
        .style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    draw_contact_list(f, app, chunks[1]);
    draw_status_bar(f, chunks[2], "  j/k/↑↓ navigate   PgUp/PgDn scroll   l/Enter open   h/Esc back");
}

fn draw_contact_list(f: &mut Frame, app: &mut App, area: Rect) {
    let has_sep = app.contacts_split > 0 && app.contacts_split < app.contacts.len();
    let max_width = area.width as usize;

    let mut items: Vec<ListItem> = Vec::new();
    for (i, entry) in app.contacts.iter().enumerate() {
        if has_sep && i == app.contacts_split {
            let sep_text = format!("{:─^width$}", " groups ", width = max_width);
            items.push(ListItem::new(Line::from(Span::styled(
                sep_text,
                Style::default().fg(Color::DarkGray),
            ))));
        }
        let name = if entry.name.chars().count() > max_width {
            let t: String = entry.name.chars().take(max_width.saturating_sub(1)).collect();
            format!("{}…", t)
        } else {
            entry.name.clone()
        };
        items.push(ListItem::new(name));
    }

    if items.is_empty() {
        f.render_widget(
            Paragraph::new("(no contacts synced yet)")
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .scroll_padding(1);
    f.render_stateful_widget(list.block(Block::default()), area, &mut app.contact_list_state);
}

// ── Chat window ───────────────────────────────────────────────────────────────

fn draw_chat_window_screen(f: &mut Frame, app: &mut App) {
    // Normal and Command modes use a fixed 2-line input area (border + one hint/command line).
    // Insert mode grows with the draft text, accounting for word-wrap at the current width.
    let input_height = if let Some(chat) = app.chat.as_ref() {
        if matches!(chat.mode, Mode::Insert) {
            let wrap_width = f.area().width.saturating_sub(2) as usize;
            let visual_lines: usize = chat.input.split('\n')
                .map(|l| wrap_input_line(l, wrap_width).len())
                .sum::<usize>()
                .max(1);
            visual_lines as u16 + 1
        } else {
            2
        }
    } else {
        2
    };
    let att_height = if app.chat.as_ref().map(|c| !c.staged_attachments.is_empty()).unwrap_or(false) {
        1u16
    } else {
        0u16
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(att_height),
            Constraint::Length(input_height),
        ])
        .split(f.area());

    draw_chat_header(f, app, chunks[0]);
    draw_messages(f, app, chunks[1]);

    let status = chat_status_bar(app);
    draw_status_bar(f, chunks[2], &status);
    if att_height > 0 {
        draw_attachment_bar(f, app, chunks[3]);
    }
    draw_input(f, app, chunks[4]);
}

fn draw_attachment_bar(f: &mut Frame, app: &App, area: Rect) {
    let chat = match app.chat.as_ref() {
        Some(c) => c,
        None => return,
    };
    let mut spans: Vec<Span> = vec![
        Span::styled("  Att:", Style::default().fg(Color::DarkGray)),
    ];
    for (i, att) in chat.staged_attachments.iter().enumerate() {
        let label = format!(" [{}  {}]", att.kind, signal::fmt_attachment_size(att.size));
        let style = if chat.selected_attachment == Some(i) {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
    let selected = chat.selected_message;
    let highlight = Style::default().bg(Color::Blue).fg(Color::White);

    let mut lines: Vec<Line> = Vec::new();
    let mut prev_ts: Option<u64> = None;
    let mut prev_sender_id: Option<String> = None;

    // Per-message line ranges for selection highlight and auto-scroll.
    // visual_start: first line in the message's visual region (incl. separator).
    // body_end: last body line added for the message.
    let mut msg_visual_starts: Vec<usize> = Vec::with_capacity(chat.messages.len());
    let mut msg_body_ends: Vec<usize> = Vec::with_capacity(chat.messages.len());

    for (msg_idx, content) in chat.messages.iter().enumerate() {
        let ts = content.timestamp();
        let sender_uuid = content.metadata.sender.raw_uuid();
        let is_own = own_aci.map(|a| a == sender_uuid).unwrap_or(false);
        let is_selected = selected == Some(msg_idx);

        let sender_label = if is_own {
            "You".to_string()
        } else if is_group {
            chat.sender_names.get(&sender_uuid)
                .cloned()
                .unwrap_or_else(|| sender_uuid.to_string())
        } else {
            thread_name.clone()
        };

        // Record where this message's visual region begins (before separator).
        msg_visual_starts.push(lines.len());

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
            let header = Line::from(vec![
                Span::styled(sender_label.clone(), sender_style),
                Span::styled(
                    format!("  {}", fmt_ts_short(ts)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            lines.push(if is_selected { header.style(highlight) } else { header });
            prev_sender_id = Some(sender_key);
        }

        // Quote block (reply preview), rendered before the body
        if let Some((q_author_uuid, q_text)) = signal::message_quote(content) {
            let q_author = if own_aci == Some(q_author_uuid) {
                "You".to_string()
            } else if is_group {
                q_author_uuid.to_string()
            } else {
                thread_name.clone()
            };
            let q_first_line = q_text.lines().next().unwrap_or("…");
            let q_line = Line::from(Span::styled(
                format!("  > {}: {}", q_author, q_first_line),
                Style::default().fg(Color::DarkGray),
            ));
            lines.push(if is_selected { q_line.style(highlight) } else { q_line });
        }

        // Message body — word-wrapped; indent each wrapped line.
        // Append ✓/✓✓ receipt indicator on the very last wrapped line of own messages.
        let body = signal::message_body(content);
        let receipt = if is_own {
            receipt_indicator(&chat.read, &chat.delivered, ts)
        } else {
            ""
        };
        let wrap_width = (area.width as usize).saturating_sub(2);
        let wrapped: Vec<String> = body.split('\n')
            .flat_map(|l| word_wrap(l, wrap_width))
            .collect();
        let total_wrapped = wrapped.len();
        for (i, text_line) in wrapped.iter().enumerate() {
            let is_last = i + 1 == total_wrapped;
            let text = if is_last && !receipt.is_empty() {
                format!("  {}{}", text_line, receipt)
            } else {
                format!("  {}", text_line)
            };
            let line = Line::raw(text);
            lines.push(if is_selected { line.style(highlight) } else { line });
        }

        // Reaction summary, e.g. "  [2x❤️, 1x👍]"
        if let Some(rxn) = format_reactions(chat.reactions.get(&ts)) {
            let line = Line::from(Span::styled(
                format!("  {}", rxn),
                Style::default().fg(Color::DarkGray),
            ));
            lines.push(if is_selected { line.style(highlight) } else { line });
        }

        msg_body_ends.push(lines.len().saturating_sub(1));
    }

    let total = lines.len();
    let height = area.height as usize;

    // Clamp scroll so we can't scroll past the top
    let max_scroll = total.saturating_sub(height);
    if chat.scroll > max_scroll {
        chat.scroll = max_scroll;
    }

    // Auto-scroll to keep the selected message in view with 1-line context.
    // scroll_row is the first visible line; higher chat.scroll = more scrolled up.
    if let Some(sel_idx) = selected {
        if let (Some(&vis_start), Some(&body_end)) =
            (msg_visual_starts.get(sel_idx), msg_body_ends.get(sel_idx))
        {
            let scroll_row = max_scroll.saturating_sub(chat.scroll);
            if vis_start < scroll_row.saturating_add(1) {
                // Selected region is above viewport — scroll up.
                let target_row = vis_start.saturating_sub(1);
                chat.scroll = max_scroll.saturating_sub(target_row);
            } else if body_end + 1 >= scroll_row + height {
                // Selected body end is below viewport — scroll down.
                let target_row = (body_end + 2).saturating_sub(height);
                chat.scroll = max_scroll.saturating_sub(target_row);
            }
            chat.scroll = chat.scroll.min(max_scroll);
        }
    }

    let scroll_row = total.saturating_sub(height).saturating_sub(chat.scroll);
    // Track which message index is at the top of the viewport so Normal-mode
    // ↓ can start selection there rather than at index 0.
    chat.viewport_top_msg = msg_visual_starts
        .partition_point(|&s| s <= scroll_row)
        .saturating_sub(1);
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll_row as u16, 0)), area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let chat = match app.chat.as_ref() {
        Some(c) => c,
        None => return,
    };

    let block = Block::default()
        .borders(ratatui::widgets::Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    match &chat.mode {
        Mode::Normal => {
            f.render_widget(
                Paragraph::new("  -- NORMAL --")
                    .style(Style::default().fg(Color::DarkGray)),
                inner,
            );
        }
        Mode::Insert => {
            let input = chat.input.as_str();
            let cursor = chat.cursor;
            // subtract 2 for the "> " / "  " prefix
            let wrap_width = inner.width.saturating_sub(2) as usize;

            // split('\n') preserves trailing newlines as an empty final element,
            // unlike str::lines() which silently drops them.
            let display_lines: Vec<&str> = if input.is_empty() {
                vec![""]
            } else {
                input.split('\n').collect()
            };

            let before_cursor = &input[..cursor.min(input.len())];
            let cursor_parts: Vec<&str> = before_cursor.split('\n').collect();
            let cursor_line = cursor_parts.len().saturating_sub(1);
            let cursor_col = cursor_parts.last().map(|l| l.chars().count()).unwrap_or(0);

            let mut text_lines: Vec<Line> = Vec::new();
            let mut visual_row = 0usize;

            for (logical_i, line_text) in display_lines.iter().enumerate() {
                let sub_lines = wrap_input_line(line_text, wrap_width);
                let sub_count = sub_lines.len();

                for sub_j in 0..sub_count {
                    let (ref sub_text, sub_start) = sub_lines[sub_j];
                    let prefix = if visual_row == 0 {
                        Span::styled("> ", Style::default().fg(Color::DarkGray))
                    } else {
                        Span::styled("  ", Style::default())
                    };

                    if logical_i == cursor_line {
                        let next_sub_start = if sub_j + 1 < sub_count {
                            sub_lines[sub_j + 1].1
                        } else {
                            line_text.chars().count()
                        };
                        let cursor_here = if sub_j + 1 < sub_count {
                            cursor_col >= sub_start && cursor_col < next_sub_start
                        } else {
                            cursor_col >= sub_start
                        };

                        if cursor_here {
                            let col_in_sub = cursor_col - sub_start;
                            let chars: Vec<char> = sub_text.chars().collect();
                            let before: String = chars[..col_in_sub.min(chars.len())].iter().collect();
                            let cursor_char = chars.get(col_in_sub)
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| " ".to_string());
                            let after: String = chars[col_in_sub.saturating_add(1).min(chars.len())..]
                                .iter()
                                .collect();
                            let mut spans = vec![
                                prefix,
                                Span::raw(before),
                                Span::styled(cursor_char, Style::default().add_modifier(Modifier::REVERSED)),
                            ];
                            if !after.is_empty() {
                                spans.push(Span::raw(after));
                            }
                            text_lines.push(Line::from(spans));
                        } else {
                            text_lines.push(Line::from(vec![prefix, Span::raw(sub_text.clone())]));
                        }
                    } else {
                        text_lines.push(Line::from(vec![prefix, Span::raw(sub_text.clone())]));
                    }

                    visual_row += 1;
                }
            }

            f.render_widget(Paragraph::new(Text::from(text_lines)), inner);
        }
        Mode::Command(cmd) => {
            let chars: Vec<char> = cmd.chars().collect();
            let cursor_char = " "; // block at end of command (cursor always trails)
            let cmd_text: String = chars.iter().collect();
            let line = Line::from(vec![
                Span::styled(":", Style::default().fg(Color::DarkGray)),
                Span::raw(cmd_text),
                Span::styled(cursor_char, Style::default().add_modifier(Modifier::REVERSED)),
            ]);
            f.render_widget(Paragraph::new(Text::from(vec![line])), inner);
        }
    }
}

// ── Shared ────────────────────────────────────────────────────────────────────

fn chat_status_bar(app: &App) -> String {
    let chat = match app.chat.as_ref() {
        Some(c) => c,
        None => return String::new(),
    };

    if let Some(hint) = &chat.autocomplete_hint {
        return format!("  {}", hint);
    }

    match &chat.mode {
        Mode::Normal => {
            if let Some(att_idx) = chat.selected_attachment {
                if let Some(att) = chat.staged_attachments.get(att_idx) {
                    let pos = format!("{}/{}", att_idx + 1, chat.staged_attachments.len());
                    return format!(
                        "  Att [{}]  {}  {}  |  dd remove   Esc deselect   q back",
                        pos, att.kind, signal::fmt_attachment_size(att.size)
                    );
                }
            }
            if let Some(sel_idx) = chat.selected_message {
                if let Some(content) = chat.messages.get(sel_idx) {
                    let sender_uuid = content.metadata.sender.raw_uuid();
                    let is_own = app.own_aci.map(|a| a == sender_uuid).unwrap_or(false);
                    let sender = if is_own { "You".to_string() } else { chat.thread_name.clone() };
                    let ts = fmt_ts_long(content.timestamp());
                    let pos = format!("{}/{}", sel_idx + 1, chat.messages.len());
                    return format!(
                        "  [{}]  {}  ·  {}  |  r reply   e edit   d delete   : command   Esc deselect   q back",
                        pos, sender, ts
                    );
                }
            }
            "  j/k/↑↓ navigate   r reply   e edit   dd delete   : command   :upload <path>   PgUp/PgDn scroll   h/q/← back".to_string()
        }
        Mode::Insert => {
            if chat.editing.is_some() {
                return "  -- INSERT -- editing   Esc cancel   Enter send   Alt+Enter newline".to_string();
            }
            if let Some(reply_idx) = chat.reply_to {
                if let Some(content) = chat.messages.get(reply_idx) {
                    let sender_uuid = content.metadata.sender.raw_uuid();
                    let is_own = app.own_aci.map(|a| a == sender_uuid).unwrap_or(false);
                    let sender = if is_own { "You".to_string() } else { chat.thread_name.clone() };
                    return format!(
                        "  -- INSERT -- replying to {}   Esc cancel   Enter send",
                        sender
                    );
                }
            }
            "  -- INSERT --   Esc normal   Enter send   Alt+Enter newline   Tab @mention".to_string()
        }
        Mode::Command(_) => {
            "  -- COMMAND --   Enter execute   Esc cancel".to_string()
        }
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, text: &str) {
    let bar = Paragraph::new(text)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(bar, area);
}

fn receipt_indicator(read: &std::collections::HashSet<u64>, delivered: &std::collections::HashSet<u64>, ts: u64) -> &'static str {
    if read.contains(&ts) { "  ✓✓" }
    else if delivered.contains(&ts) { "  ✓" }
    else { "" }
}

/// Word-wrap `text` to at most `max_width` chars per line.
/// Splits on existing `\n` first, then breaks long paragraphs at word boundaries.
/// Words longer than `max_width` are placed on their own line and left to overflow.
fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.chars().count() <= max_width {
            lines.push(paragraph.to_string());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in paragraph.split_whitespace() {
            let wlen = word.chars().count();
            if current.is_empty() {
                current.push_str(word);
                current_len = wlen;
            } else if current_len + 1 + wlen <= max_width {
                current.push(' ');
                current.push_str(word);
                current_len += 1 + wlen;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_len = wlen;
            }
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Word-wraps one logical input line, preserving char offsets for cursor mapping.
/// Returns Vec of (visual_sub_line_text, char_start_offset_in_original).
/// Breaks at word boundaries; trailing spaces from a break are kept on the preceding sub-line.
/// Long words that exceed max_width are hard-broken.
fn wrap_input_line(text: &str, max_width: usize) -> Vec<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if max_width == 0 || n <= max_width {
        return vec![(text.to_string(), 0)];
    }
    let mut result = Vec::new();
    let mut start = 0usize;
    while start < n {
        let remaining = n - start;
        if remaining <= max_width {
            result.push((chars[start..].iter().collect(), start));
            break;
        }
        let end = start + max_width;
        // Search backwards from end-1 for a space to use as a word-boundary break.
        // The space is included in the current sub-line; next sub-line starts after it.
        let mut break_pos = end;
        let mut next_start = end;
        for j in (start..end).rev() {
            if chars[j] == ' ' {
                break_pos = j + 1;
                next_start = j + 1;
                break;
            }
        }
        result.push((chars[start..break_pos].iter().collect(), start));
        start = next_start;
    }
    if result.is_empty() {
        result.push((String::new(), 0));
    }
    result
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

fn format_reactions(map: Option<&HashMap<String, HashSet<[u8; 16]>>>) -> Option<String> {
    let map = map.filter(|m| !m.is_empty())?;
    Some(format!("[{}]", signal::fmt_reaction_pairs(map).join(", ")))
}
