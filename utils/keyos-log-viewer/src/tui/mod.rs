// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod log;
mod process;

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Style},
    text::Span,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};

use crate::state::{State, ViewMode};

pub fn draw(f: &mut Frame, state: &mut State) {
    let size = f.area();
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(size);
    let content_area = vertical_chunks[0];

    match state.view_mode {
        ViewMode::Logs => {
            log::render(f, content_area, &mut state.log);
        }
        ViewMode::ProcessList => {
            process::render(f, content_area, &mut state.process);
        }
    }

    render_footer(f, state, vertical_chunks[1]);
    render_notifications(f, state, size);
    if state.help_open {
        render_help_overlay(f, state, size);
    }
}

fn render_footer(f: &mut Frame, state: &State, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Min(20)])
        .split(area);

    let connection_status =
        if state.status_text.is_empty() { "disconnected".to_string() } else { state.status_text.clone() };
    let monitor_status = if state.process.monitor_enabled { "monitor: on (3s)" } else { "monitor: off" };
    let left_text = format!("{connection_status} | {monitor_status}");

    let right_text = "?: Help";

    let left = Paragraph::new(left_text).style(Style::default().fg(Color::Cyan));
    let right =
        Paragraph::new(right_text).alignment(Alignment::Right).style(Style::default().fg(Color::Gray));
    f.render_widget(left, chunks[0]);
    f.render_widget(right, chunks[1]);
}

pub struct KeybindEntry {
    pub key: &'static str,
    pub label: &'static str,
}

fn render_notifications(f: &mut Frame, state: &State, area: Rect) {
    if state.notifications.is_empty() || area.width < 12 || area.height < 3 {
        return;
    }

    let [_, notification_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(area.width.min(36))])
        .areas(area);

    let mut constraints = vec![Constraint::Length(3); state.notifications.len()];
    constraints.push(Constraint::Min(0));
    let note_areas = Layout::default()
        .direction(Direction::Vertical)
        .spacing(1)
        .constraints(constraints)
        .split(notification_area);

    for (notification, note_area) in state.notifications.iter().rev().zip(note_areas.iter().copied()) {
        let color = if notification.error { Color::Red } else { Color::Green };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .style(Style::default().bg(Color::Black));
        let paragraph = Paragraph::new(notification.message.as_str())
            .style(Style::default().fg(color))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
            .block(block);
        f.render_widget(Clear, note_area);
        f.render_widget(paragraph, note_area);
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn render_help_overlay(f: &mut Frame, state: &State, area: Rect) {
    const LOG_HELP: &[KeybindEntry] = &[
        KeybindEntry { key: "?/h", label: "Toggle help" },
        KeybindEntry { key: "q", label: "Quit" },
        KeybindEntry { key: "v", label: "Switch to process view" },
        KeybindEntry { key: "f", label: "Open log config" },
        KeybindEntry { key: "/", label: "Open search" },
        KeybindEntry { key: "Enter", label: "Apply search" },
        KeybindEntry { key: "Esc", label: "Clear search / close help" },
        KeybindEntry { key: "Up/Down/j/k", label: "Scroll logs" },
        KeybindEntry { key: "t/g", label: "Jump to top" },
        KeybindEntry { key: "b/G", label: "Jump to bottom" },
        KeybindEntry { key: "c", label: "Clear all logs" },
        KeybindEntry { key: "y", label: "Copy selected row(s) ([N]y)" },
        KeybindEntry { key: "Y", label: "Copy filtered logs" },
    ];

    const PROCESS_HELP: &[KeybindEntry] = &[
        KeybindEntry { key: "?/h", label: "Toggle help" },
        KeybindEntry { key: "q", label: "Quit" },
        KeybindEntry { key: "v", label: "Switch to logs view" },
        KeybindEntry { key: "f", label: "Open process filter/sort" },
        KeybindEntry { key: "Up/Down/j/k", label: "Select process row" },
        KeybindEntry { key: "t/g", label: "Jump to top" },
        KeybindEntry { key: "b/G", label: "Jump to bottom" },
        KeybindEntry { key: "y", label: "Copy selected process" },
        KeybindEntry { key: "Y", label: "Copy process table (md)" },
        KeybindEntry { key: "p", label: "Request process list" },
        KeybindEntry { key: "r", label: "Toggle 3s monitor" },
        KeybindEntry { key: "Esc", label: "Close help" },
    ];

    let (title, entries) = match state.view_mode {
        ViewMode::Logs => ("Help: Logs", LOG_HELP),
        ViewMode::ProcessList => ("Help: Process List", PROCESS_HELP),
    };

    render_keybind_sheet(f, area, title, entries);
}

pub fn render_keybind_sheet(f: &mut Frame, area: Rect, title: &str, entries: &'static [KeybindEntry]) {
    let overlay = centered_rect(58, 64, area);
    f.render_widget(Clear, overlay);
    f.render_widget(Block::default().style(Style::default().bg(Color::DarkGray)), overlay);
    let inner = overlay.inner(Margin { vertical: 1, horizontal: 2 });

    let rows = entries
        .iter()
        .map(|entry| {
            Row::new(vec![
                Cell::from(Span::styled(format!("  {}", entry.key), Style::default().fg(Color::Yellow))),
                Cell::from(entry.label),
            ])
        })
        .collect::<Vec<_>>();

    let table = Table::new(rows, [Constraint::Length(16), Constraint::Min(20)])
        .block(Block::default().borders(Borders::ALL).title(title))
        .column_spacing(2);
    f.render_widget(table, inner);
}
