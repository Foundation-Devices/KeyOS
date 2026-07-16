// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;
use std::time::Duration;

use crossterm::event::{KeyCode, MouseEventKind};
use log::Level;
use ratatui::{text::Line, widgets::ListState};

use super::{copy_to_clipboard, ensure_list_selection, move_selection, KeyResult, Notification};

pub struct LogViewState {
    pub entries: Vec<LogItem>,
    pub levels: BTreeSet<LogLevel>,
    pub pid_servers: BTreeSet<(u32, String)>,
    pub level_filters: BTreeSet<LogLevel>,
    pub pid_server_filters: BTreeSet<(u32, String)>,
    pub cursor_line: usize,
    pub viewport_start: usize,
    pub viewport_line_count: usize,
    pub show_pid: bool,
    pub show_server: bool,
    pub show_path: bool,
    pub modal_open: bool,
    pub modal_tab: LogConfigTab,
    pub levels_state: ListState,
    pub servers_state: ListState,
    pub columns_state: ListState,
    pub search_mode: bool,
    pub search_query: String,
    pub search_input: String,
    pub render_cache: LogRenderCache,
    pub render_cache_generation: u64,
    pub scroll_anchor: ScrollAnchor,
    pub count_prefix: Option<usize>,
}

#[derive(Clone, Copy)]
pub enum ScrollAnchor {
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct LogRenderCache {
    pub key: LogRenderCacheKey,
    pub source_len: usize,
    pub lines: Vec<CachedRenderLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRenderCacheKey {
    pub width: u16,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct CachedRenderLine {
    pub entry_idx: usize,
    pub line: Line<'static>,
}

impl Default for LogRenderCache {
    fn default() -> Self {
        Self { key: LogRenderCacheKey { width: 0, generation: u64::MAX }, source_len: 0, lines: Vec::new() }
    }
}

#[derive(Debug, Clone)]
pub enum LogItem {
    Log(LogMessage),
    Panic(PanicMessage),
    Raw(String),
}

#[derive(Debug, Clone)]
pub struct PanicMessage {
    pub pid: u32,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LogMessage {
    pub timestamp: f64,
    pub level: Level,
    pub pid: u32,
    pub server: String,
    pub path: String,
    pub message: String,
    pub is_session_separator: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LogLevel {
    Panic,
    Raw,
    Standard(Level),
}

#[derive(Clone, Copy)]
pub enum LogConfigTab {
    Levels,
    Servers,
    Columns,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Panic => "PANIC",
            Self::Raw => "RAW",
            Self::Standard(Level::Error) => "ERR",
            Self::Standard(Level::Warn) => "WRN",
            Self::Standard(Level::Info) => "INF",
            Self::Standard(Level::Debug) => "DBG",
            Self::Standard(Level::Trace) => "TRC",
        }
    }
}

impl Ord for LogLevel {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        level_sort_key(self).cmp(&level_sort_key(other)).then_with(|| self.as_str().cmp(other.as_str()))
    }
}

impl PartialOrd for LogLevel {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> { Some(self.cmp(other)) }
}

impl LogViewState {
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            levels: Default::default(),
            pid_servers: Default::default(),
            level_filters: Default::default(),
            pid_server_filters: Default::default(),
            cursor_line: 0,
            viewport_start: 0,
            viewport_line_count: 0,
            show_pid: true,
            show_server: true,
            show_path: true,
            modal_open: false,
            modal_tab: LogConfigTab::Levels,
            levels_state: Default::default(),
            servers_state: Default::default(),
            columns_state: Default::default(),
            search_mode: false,
            search_query: String::new(),
            search_input: String::new(),
            render_cache: LogRenderCache::default(),
            render_cache_generation: 0,
            scroll_anchor: ScrollAnchor::Up,
            count_prefix: None,
        }
    }

    pub fn set_viewport_line_count(&mut self, viewport_line_count: usize) {
        self.viewport_line_count = viewport_line_count;
    }

    pub fn max_cursor_line(&self, total_lines: usize) -> usize { total_lines.saturating_sub(1) }

    pub fn max_viewport_start(&self, total_lines: usize) -> usize {
        total_lines.saturating_sub(self.viewport_line_count)
    }

    pub fn is_cursor_line_at_bottom(&self, total_lines: usize) -> bool {
        total_lines == 0 || self.cursor_line >= self.max_cursor_line(total_lines)
    }

    pub fn clamp_cursor_line(&mut self, total_lines: usize) {
        self.cursor_line = self.cursor_line.min(self.max_cursor_line(total_lines));
        if total_lines == 0 {
            self.cursor_line = 0;
        }
    }

    pub fn clamp_viewport_start(&mut self, total_lines: usize) {
        self.viewport_start = self.viewport_start.min(self.max_viewport_start(total_lines));
        if total_lines == 0 {
            self.viewport_start = 0;
        }
    }

    pub fn sync_viewport_start_to_cursor_line(&mut self, total_lines: usize) {
        if self.viewport_line_count == 0 || total_lines == 0 {
            self.viewport_start = 0;
            return;
        }

        let min_visible_start = self.cursor_line.saturating_add(1).saturating_sub(self.viewport_line_count);
        let max_visible_start = self.cursor_line;

        let (min_start, max_start) = match self.scroll_anchor {
            ScrollAnchor::Up => {
                let top_anchor = self.viewport_line_count / 4;
                let anchored_start = self.cursor_line.saturating_sub(top_anchor);
                (min_visible_start, anchored_start)
            }
            ScrollAnchor::Down => {
                let bottom_anchor = self.viewport_line_count.saturating_mul(3) / 4;
                let anchored_start = self.cursor_line.saturating_sub(bottom_anchor);
                (anchored_start.max(min_visible_start), max_visible_start)
            }
        };

        self.viewport_start = self.viewport_start.clamp(min_start, max_start);
        self.viewport_start = self.viewport_start.min(self.max_viewport_start(total_lines));
    }

    pub fn selected_entry_idx(&self) -> Option<usize> {
        self.render_cache.lines.get(self.cursor_line).map(|cached_line| cached_line.entry_idx)
    }

    pub fn scroll_lines(&mut self, delta_lines: isize) {
        if delta_lines == 0 {
            return;
        }

        if delta_lines < 0 {
            self.cursor_line = self.cursor_line.saturating_sub((-delta_lines) as usize);
            self.scroll_anchor = ScrollAnchor::Up;
        } else {
            self.cursor_line = self.cursor_line.saturating_add(delta_lines as usize);
            self.scroll_anchor = ScrollAnchor::Down;
        }

        let total_lines = self.render_cache.lines.len();
        self.clamp_cursor_line(total_lines);
        self.sync_viewport_start_to_cursor_line(total_lines);
    }

    pub fn refresh_filters(&mut self) {
        let mut current_levels: BTreeSet<LogLevel> = BTreeSet::new();
        let mut current_pid_servers: BTreeSet<(u32, String)> = BTreeSet::new();
        for entry in &self.entries {
            match entry {
                LogItem::Log(log) => {
                    current_levels.insert(LogLevel::Standard(log.level));
                    current_pid_servers.insert((log.pid, log.server.clone()));
                }
                LogItem::Panic(_) => {
                    current_levels.insert(LogLevel::Panic);
                }
                LogItem::Raw(_) => {
                    current_levels.insert(LogLevel::Raw);
                }
            }
        }

        for level in current_levels {
            if self.levels.insert(level.clone()) {
                self.level_filters.insert(level);
            }
        }

        for pid_server in current_pid_servers {
            if self.pid_servers.insert(pid_server.clone()) {
                self.pid_server_filters.insert(pid_server);
            }
        }

        ensure_list_selection(&mut self.levels_state, self.levels.len());
        ensure_list_selection(&mut self.servers_state, self.pid_servers.len());
    }

    pub fn push_entry(&mut self, entry: LogItem) {
        self.extend_filters_for_entry(&entry);
        self.entries.push(entry);
    }

    pub fn entry_visible(&self, entry: &LogItem) -> bool {
        let matches_filters = match entry {
            LogItem::Log(log) => {
                self.level_filters.contains(&LogLevel::Standard(log.level))
                    && self.pid_server_filters.contains(&(log.pid, log.server.clone()))
            }
            LogItem::Panic(_) => self.level_filters.contains(&LogLevel::Panic),
            LogItem::Raw(_) => self.level_filters.contains(&LogLevel::Raw),
        };

        if !matches_filters {
            return false;
        }

        if self.search_query.is_empty() {
            return true;
        }

        entry_matches_query(entry, &self.search_query)
    }

    pub fn handle_key(&mut self, code: KeyCode) -> KeyResult {
        if self.modal_open {
            self.handle_log_columns_overlay_key(code);
            return KeyResult::consumed();
        }

        if self.search_mode {
            self.handle_search_key(code);
            return KeyResult::consumed();
        }

        // Save count before resetting on non-digit keys.
        let count = self.count_prefix.unwrap_or(1);

        if let KeyCode::Char(c @ '0'..='9') = code {
            let digit = (c as u8 - b'0') as usize;
            self.count_prefix = Some(match self.count_prefix {
                Some(n) => n * 10 + digit,
                None => digit,
            });
            return KeyResult::consumed();
        }

        self.count_prefix = None;

        match code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll_lines(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_lines(-1),
            KeyCode::PageDown => self.scroll_lines(self.viewport_line_count as isize),
            KeyCode::PageUp => self.scroll_lines(-(self.viewport_line_count as isize)),
            KeyCode::Char('f') => {
                self.modal_open = true;
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
                self.search_input = self.search_query.clone();
            }
            KeyCode::Char('y') => {
                let (text, msg) = if count > 1 {
                    let Some(text) = build_selected_rows_text(self, count) else {
                        return KeyResult::consumed().set_notify(Notification::error(
                            "Selected log not found",
                            Duration::from_secs(3),
                        ));
                    };
                    (text, format!("Copied {} logs to clipboard", count))
                } else {
                    let Some(text) = build_selected_row_text(self) else {
                        return KeyResult::consumed().set_notify(Notification::error(
                            "Selected log not found",
                            Duration::from_secs(3),
                        ));
                    };
                    (text, "Copied selected log to clipboard".to_string())
                };

                return KeyResult::consumed().set_notify(copy_to_clipboard(&text, &msg));
            }
            KeyCode::Char('Y') => {
                let text = build_filtered_text(self);
                return KeyResult::consumed()
                    .set_notify(copy_to_clipboard(&text, "Copied all logs to clipboard"));
            }
            KeyCode::Char('c') => {
                self.clear_logs();
                return KeyResult::consumed()
                    .set_notify(Notification::success("Cleared all logs", Duration::from_secs(2)));
            }
            KeyCode::Char('g') | KeyCode::Char('t') | KeyCode::Home => self.scroll_logs_to_start(),
            KeyCode::Char('G') | KeyCode::Char('b') | KeyCode::End => self.scroll_logs_to_end(),
            _ => return KeyResult::ignore(),
        }

        KeyResult::consumed()
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollDown => self.scroll_lines(1),
            MouseEventKind::ScrollUp => self.scroll_lines(-1),
            _ => {}
        }
    }

    pub fn handle_resize(&mut self) { self.invalidate_render_cache(); }

    pub fn invalidate_render_cache(&mut self) {
        self.render_cache_generation = self.render_cache_generation.wrapping_add(1);
    }

    fn extend_filters_for_entry(&mut self, entry: &LogItem) {
        match entry {
            LogItem::Log(log) => {
                let level = LogLevel::Standard(log.level);
                if self.levels.insert(level.clone()) {
                    self.level_filters.insert(level);
                    ensure_list_selection(&mut self.levels_state, self.levels.len());
                }

                let pid_server = (log.pid, log.server.clone());
                if self.pid_servers.insert(pid_server.clone()) {
                    self.pid_server_filters.insert(pid_server);
                    ensure_list_selection(&mut self.servers_state, self.pid_servers.len());
                }
            }
            LogItem::Panic(_) => {
                let level = LogLevel::Panic;
                if self.levels.insert(level.clone()) {
                    self.level_filters.insert(level);
                    ensure_list_selection(&mut self.levels_state, self.levels.len());
                }
            }
            LogItem::Raw(_) => {
                let level = LogLevel::Raw;
                if self.levels.insert(level.clone()) {
                    self.level_filters.insert(level);
                    ensure_list_selection(&mut self.levels_state, self.levels.len());
                }
            }
        }
    }

    fn filtered_entries(&self) -> Vec<&LogItem> {
        self.entries.iter().filter(|entry| self.entry_visible(entry)).collect()
    }

    fn selected_level(&self) -> Option<LogLevel> {
        let index = self.levels_state.selected()?;
        self.levels.iter().nth(index).cloned()
    }

    fn selected_pid_server(&self) -> Option<(u32, String)> {
        let index = self.servers_state.selected()?;
        self.pid_servers.iter().nth(index).cloned()
    }

    fn prev_tab(&mut self) {
        self.modal_tab = match self.modal_tab {
            LogConfigTab::Levels => LogConfigTab::Columns,
            LogConfigTab::Servers => LogConfigTab::Levels,
            LogConfigTab::Columns => LogConfigTab::Servers,
        };
    }

    fn next_tab(&mut self) {
        self.modal_tab = match self.modal_tab {
            LogConfigTab::Levels => LogConfigTab::Servers,
            LogConfigTab::Servers => LogConfigTab::Columns,
            LogConfigTab::Columns => LogConfigTab::Levels,
        };
    }

    fn move_modal_selection(&mut self, delta: isize) {
        match self.modal_tab {
            LogConfigTab::Levels => {
                move_selection(&mut self.levels_state, self.levels.len(), delta);
            }
            LogConfigTab::Servers => {
                move_selection(&mut self.servers_state, self.pid_servers.len(), delta);
            }
            LogConfigTab::Columns => {
                move_selection(&mut self.columns_state, 3, delta);
            }
        }
    }

    fn toggle_modal_selected(&mut self) {
        match self.modal_tab {
            LogConfigTab::Levels => {
                if let Some(level) = self.selected_level() {
                    if self.level_filters.contains(&level) {
                        self.level_filters.remove(&level);
                    } else {
                        self.level_filters.insert(level);
                    }
                }
            }
            LogConfigTab::Servers => {
                if let Some(key) = self.selected_pid_server() {
                    if self.pid_server_filters.contains(&key) {
                        self.pid_server_filters.remove(&key);
                    } else {
                        self.pid_server_filters.insert(key);
                    }
                }
            }
            LogConfigTab::Columns => match self.columns_state.selected().unwrap_or(0) {
                0 => self.show_pid = !self.show_pid,
                1 => self.show_server = !self.show_server,
                2 => self.show_path = !self.show_path,
                _ => {}
            },
        }
        self.invalidate_render_cache();
    }

    fn toggle_all_modal_tab(&mut self) {
        match self.modal_tab {
            LogConfigTab::Levels => {
                let all_selected = self.level_filters.len() == self.levels.len()
                    && self.levels.iter().all(|level| self.level_filters.contains(level));
                if all_selected {
                    self.level_filters.clear();
                } else {
                    self.level_filters = self.levels.clone();
                }
            }
            LogConfigTab::Servers => {
                let all_selected = self.pid_server_filters.len() == self.pid_servers.len()
                    && self.pid_servers.iter().all(|pid_server| self.pid_server_filters.contains(pid_server));
                if all_selected {
                    self.pid_server_filters.clear();
                } else {
                    self.pid_server_filters = self.pid_servers.clone();
                }
            }
            LogConfigTab::Columns => {
                let all_selected = self.show_pid && self.show_server && self.show_path;
                if all_selected {
                    self.show_pid = false;
                    self.show_server = false;
                    self.show_path = false;
                } else {
                    self.show_pid = true;
                    self.show_server = true;
                    self.show_path = true;
                }
            }
        }
        self.invalidate_render_cache();
    }

    fn select_u2f_servers(&mut self) {
        const U2F_SERVERS: &[&str] = &["nfc", "ctap_hid", "fido", "gui_app_security_keys"];
        self.pid_server_filters.clear();
        for entry in &self.pid_servers {
            if U2F_SERVERS.contains(&entry.1.as_str()) {
                self.pid_server_filters.insert(entry.clone());
            }
        }
        self.invalidate_render_cache();
    }

    fn handle_log_columns_overlay_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('f') => {
                self.modal_open = false;
            }
            KeyCode::Left | KeyCode::Char('h') => self.prev_tab(),
            KeyCode::Right | KeyCode::Char('l') => self.next_tab(),
            KeyCode::Up | KeyCode::Char('k') => self.move_modal_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_modal_selection(1),
            KeyCode::Char(' ') => self.toggle_modal_selected(),
            KeyCode::Char('a') => self.toggle_all_modal_tab(),
            KeyCode::Char('u') if matches!(self.modal_tab, LogConfigTab::Servers) => {
                self.select_u2f_servers();
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => {
                self.search_query = self.search_input.clone();
                self.search_mode = false;
                self.invalidate_render_cache();
                self.scroll_logs_to_start();
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_query.clear();
                self.search_input.clear();
                self.invalidate_render_cache();
                self.scroll_logs_to_start();
            }
            KeyCode::Char(c) => {
                self.search_input.push(c);
            }
            _ => {}
        }
    }

    fn clear_logs(&mut self) {
        self.entries.clear();
        self.levels.clear();
        self.pid_servers.clear();
        self.level_filters.clear();
        self.pid_server_filters.clear();
        self.levels_state = Default::default();
        self.servers_state = Default::default();
        self.cursor_line = 0;
        self.viewport_start = 0;
        self.invalidate_render_cache();
    }

    fn scroll_logs_to_start(&mut self) {
        self.cursor_line = 0;
        self.viewport_start = 0;
    }

    fn scroll_logs_to_end(&mut self) {
        let total_lines = self.render_cache.lines.len();
        self.cursor_line = self.max_cursor_line(total_lines);
        self.viewport_start = self.max_viewport_start(total_lines);
    }
}

fn level_sort_key(level: &LogLevel) -> u8 {
    match level {
        LogLevel::Panic => 0,
        LogLevel::Raw => 1,
        LogLevel::Standard(Level::Error) => 2,
        LogLevel::Standard(Level::Warn) => 3,
        LogLevel::Standard(Level::Info) => 4,
        LogLevel::Standard(Level::Debug) => 5,
        LogLevel::Standard(Level::Trace) => 6,
    }
}

fn format_entry_plain(entry: &LogItem) -> String {
    match entry {
        LogItem::Log(log) => format!(
            "[{:.3}] {} {} {}::{}: {}",
            log.timestamp,
            LogLevel::Standard(log.level).as_str(),
            log.pid,
            log.server,
            log.path,
            log.message
        ),
        LogItem::Panic(panic) => panic.message.clone(),
        LogItem::Raw(raw) => raw.clone(),
    }
}

fn build_filtered_text(state: &LogViewState) -> String {
    let mut lines = Vec::new();
    for entry in state.filtered_entries() {
        if let LogItem::Log(log) = entry
            && log.is_session_separator
        {
            lines.push("----- session reset -----".to_string());
        }
        lines.push(format_entry_plain(entry));
    }
    lines.join("\n")
}

fn build_selected_row_text(state: &LogViewState) -> Option<String> {
    let entry_idx = state.selected_entry_idx()?;
    state.entries.get(entry_idx).map(format_entry_plain)
}

fn build_selected_rows_text(state: &LogViewState, count: usize) -> Option<String> {
    let start_idx = state.selected_entry_idx()?;

    let lines = state.entries[start_idx..]
        .iter()
        .filter(|entry| state.entry_visible(entry))
        .take(count)
        .map(format_entry_plain)
        .collect::<Vec<_>>();

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn entry_matches_query(entry: &LogItem, query: &str) -> bool {
    match entry {
        LogItem::Log(log) => field_matches_query(&log.message, query),
        LogItem::Panic(panic) => field_matches_query(&panic.message, query),
        LogItem::Raw(raw) => field_matches_query(raw, query),
    }
}

fn field_matches_query(field: &str, query: &str) -> bool {
    ascii_case_insensitive_contains(field.as_bytes(), query.as_bytes())
}

fn ascii_case_insensitive_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }

    for start in 0..=haystack.len() - needle.len() {
        if bytes_eq_ascii_ignore_case(&haystack[start..start + needle.len()], needle) {
            return true;
        }
    }
    false
}

fn bytes_eq_ascii_ignore_case(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }

    lhs.iter().zip(rhs.iter()).all(|(left, right)| left.eq_ignore_ascii_case(right))
}
