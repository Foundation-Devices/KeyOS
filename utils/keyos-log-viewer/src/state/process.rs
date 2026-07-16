// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::Ordering;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, MouseEventKind};
use ratatui::widgets::{ListState, TableState};

use super::{
    copy_to_clipboard, ensure_list_selection, move_selection, move_table_selection, KeyResult, Notification,
};
use crate::transport::TransportCommand;

const PROCESS_MONITOR_INTERVAL: Duration = Duration::from_secs(3);

pub struct ProcessViewState {
    pub table_state: TableState,
    pub snapshot: Option<SnapshotState>,
    pub usage_history: VecDeque<UsageSample>,
    pub monitor_enabled: bool,
    pub modal_open: bool,
    pub modal_tab: ProcessConfigTab,
    pub process_names: Vec<String>,
    pub process_name_filters: HashSet<String>,
    pub process_names_state: ListState,
    pub sort_state: ListState,
    pub sort_by: ProcessSortField,
    pub sort_desc: bool,
    pub next_monitor_request_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct SnapshotState {
    pub snapshot: ProcessListSnapshot,
    pub received_at: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct UsageSample {
    pub at: Instant,
    pub cpu: u8,
    pub ram: u8,
}

#[derive(Clone, Copy)]
pub enum ProcessConfigTab {
    Processes,
    Sort,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortField {
    Pid,
    Ppid,
    Process,
    State,
    Cpu,
    Ram,
    Connections,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessListSnapshot {
    pub rows: Vec<ProcessRow>,
    pub cpu_usage_percent: u8,
    pub ram_usage_percent: u8,
    pub ram_used_kb: u32,
    pub ram_total_kb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    pub ppid: u32,
    pub process: String,
    pub state: String,
    pub cpu_percent: u8,
    pub ram_kb: u32,
    pub connections: u32,
}

impl ProcessViewState {
    pub fn new() -> Self {
        let mut process_names_state = ListState::default();
        process_names_state.select(Some(0));
        let mut sort_state = ListState::default();
        sort_state.select(Some(0));
        Self {
            table_state: TableState::default(),
            snapshot: None,
            usage_history: VecDeque::new(),
            monitor_enabled: false,
            modal_open: false,
            modal_tab: ProcessConfigTab::Processes,
            process_names: Vec::new(),
            process_name_filters: HashSet::new(),
            process_names_state,
            sort_state,
            sort_by: ProcessSortField::Pid,
            sort_desc: false,
            next_monitor_request_at: None,
        }
    }

    pub fn refresh_process_filters(&mut self) {
        let Some(snapshot_state) = &self.snapshot else {
            self.process_names.clear();
            self.process_name_filters.clear();
            self.process_names_state.select(None);
            return;
        };

        let old_names_len = self.process_names.len();
        let old_filters = self.process_name_filters.clone();
        let had_all_selected = old_names_len > 0 && old_filters.len() == old_names_len;

        let mut names =
            snapshot_state.snapshot.rows.iter().map(|row| row.process.clone()).collect::<Vec<_>>();
        names.sort();
        names.dedup();

        self.process_names = names;
        self.process_name_filters.clear();
        if old_names_len == 0 || had_all_selected {
            self.process_name_filters = self.process_names.iter().cloned().collect();
        } else {
            for name in old_filters {
                if self.process_names.contains(&name) {
                    self.process_name_filters.insert(name);
                }
            }
        }
        ensure_list_selection(&mut self.process_names_state, self.process_names.len());
    }

    pub fn visible_rows(&self) -> Vec<&ProcessRow> {
        let Some(snapshot_state) = &self.snapshot else {
            return Vec::new();
        };

        let mut rows =
            snapshot_state.snapshot.rows.iter().filter(|row| self.row_visible(row)).collect::<Vec<_>>();

        rows.sort_by(|left, right| {
            let ordering = match self.sort_by {
                ProcessSortField::Pid => left.pid.cmp(&right.pid),
                ProcessSortField::Ppid => left.ppid.cmp(&right.ppid),
                ProcessSortField::Process => left.process.cmp(&right.process),
                ProcessSortField::State => left.state.cmp(&right.state),
                ProcessSortField::Cpu => left.cpu_percent.cmp(&right.cpu_percent),
                ProcessSortField::Ram => left.ram_kb.cmp(&right.ram_kb),
                ProcessSortField::Connections => left.connections.cmp(&right.connections),
            };
            let ordering = if self.sort_desc { ordering.reverse() } else { ordering };
            if ordering == Ordering::Equal {
                left.pid.cmp(&right.pid)
            } else {
                ordering
            }
        });
        rows
    }

    pub fn handle_process_snapshot(&mut self, snapshot: ProcessListSnapshot) {
        const MAX_USAGE_SAMPLES: usize = 120;

        let received_at = Instant::now();
        if self.usage_history.len() >= MAX_USAGE_SAMPLES {
            self.usage_history.pop_front();
        }
        self.usage_history.push_back(UsageSample {
            at: received_at,
            cpu: snapshot.cpu_usage_percent,
            ram: snapshot.ram_usage_percent,
        });
        self.snapshot = Some(SnapshotState { snapshot, received_at });
        self.refresh_process_filters();
    }

    pub fn handle_key(&mut self, code: KeyCode) -> KeyResult {
        if self.modal_open {
            self.handle_process_filter_overlay_key(code);
            return KeyResult::consumed();
        }

        match code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll_processes(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_processes(-1),
            KeyCode::Char('p') => {
                return KeyResult::consumed().set_transport(TransportCommand::RequestProcessList);
            }
            KeyCode::Char('r') => {
                let mut result = KeyResult::consumed();
                if self.toggle_monitor(Instant::now()) {
                    result = result.set_transport(TransportCommand::RequestProcessList);
                }
                return result;
            }
            KeyCode::Char('f') => {
                self.modal_open = true;
                self.refresh_process_filters();
                self.sort_state.select(Some(self.sort_by.index()));
            }
            KeyCode::Char('y') => {
                let Some(text) = build_selected_process_markdown(self) else {
                    return KeyResult::consumed().set_notify(Notification::error(
                        "Selected process not found",
                        Duration::from_secs(3),
                    ));
                };
                return KeyResult::consumed().set_notify(copy_to_clipboard(&text, "Copied selected process"));
            }
            KeyCode::Char('Y') => {
                let text = build_process_table_markdown(&self.visible_rows());
                return KeyResult::consumed().set_notify(copy_to_clipboard(&text, "Copied process table"));
            }
            KeyCode::Char('g') | KeyCode::Char('t') | KeyCode::Home => self.scroll_processes_to_start(),
            KeyCode::Char('G') | KeyCode::Char('b') | KeyCode::End => self.scroll_processes_to_end(),
            _ => return KeyResult::ignore(),
        }

        KeyResult::consumed()
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollDown => self.scroll_processes(1),
            MouseEventKind::ScrollUp => self.scroll_processes(-1),
            _ => {}
        }
    }

    pub fn handle_tick(&mut self, now: Instant) -> Option<TransportCommand> {
        if !self.monitor_enabled {
            self.next_monitor_request_at = None;
            return None;
        }

        if self.next_monitor_request_at.is_none_or(|next_request| now >= next_request) {
            self.next_monitor_request_at = Some(now + PROCESS_MONITOR_INTERVAL);
            Some(TransportCommand::RequestProcessList)
        } else {
            None
        }
    }

    fn filtered_row_count(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot.rows.iter().filter(|row| self.row_visible(row)).count())
            .unwrap_or(0)
    }

    fn scroll_processes(&mut self, delta: isize) {
        let len = self.filtered_row_count();
        move_table_selection(&mut self.table_state, len, delta);
    }

    fn prev_tab(&mut self) {
        self.modal_tab = match self.modal_tab {
            ProcessConfigTab::Processes => ProcessConfigTab::Sort,
            ProcessConfigTab::Sort => ProcessConfigTab::Processes,
        };
    }

    fn next_tab(&mut self) {
        self.modal_tab = match self.modal_tab {
            ProcessConfigTab::Processes => ProcessConfigTab::Sort,
            ProcessConfigTab::Sort => ProcessConfigTab::Processes,
        };
    }

    fn move_modal_selection(&mut self, delta: isize) {
        match self.modal_tab {
            ProcessConfigTab::Processes => {
                move_selection(&mut self.process_names_state, self.process_names.len(), delta);
            }
            ProcessConfigTab::Sort => {
                move_selection(&mut self.sort_state, 7, delta);
            }
        }
    }

    fn toggle_modal_selected(&mut self) {
        match self.modal_tab {
            ProcessConfigTab::Processes => {
                if let Some(i) = self.process_names_state.selected() {
                    let name = &self.process_names[i];
                    if self.process_name_filters.contains(name) {
                        self.process_name_filters.remove(name);
                    } else {
                        self.process_name_filters.insert(name.clone());
                    }
                }
            }
            ProcessConfigTab::Sort => {
                let selected_idx = self.sort_state.selected().unwrap_or(0).min(6);
                let selected_field = ProcessSortField::from_index(selected_idx);
                if self.sort_by == selected_field {
                    self.sort_desc = !self.sort_desc;
                } else {
                    self.sort_by = selected_field;
                    self.sort_desc = false;
                }
            }
        }
    }

    fn toggle_all_modal_tab(&mut self) {
        match self.modal_tab {
            ProcessConfigTab::Processes => {
                let all_selected = self.process_name_filters.len() == self.process_names.len()
                    && self
                        .process_names
                        .iter()
                        .all(|process_name| self.process_name_filters.contains(process_name));
                if all_selected {
                    self.process_name_filters.clear();
                } else {
                    self.process_name_filters = self.process_names.iter().cloned().collect();
                }
            }
            ProcessConfigTab::Sort => {
                self.sort_by = ProcessSortField::Pid;
                self.sort_desc = false;
                self.sort_state.select(Some(self.sort_by.index()));
            }
        }
    }

    fn handle_process_filter_overlay_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('f') => {
                self.modal_open = false;
            }
            KeyCode::Left | KeyCode::Char('h') => self.prev_tab(),
            KeyCode::Right | KeyCode::Char('l') => self.next_tab(),
            KeyCode::Up | KeyCode::Char('k') => self.move_modal_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_modal_selection(1),
            KeyCode::Char(' ') => {
                self.toggle_modal_selected();
                self.table_state.select(Some(0));
            }
            KeyCode::Char('a') => {
                self.toggle_all_modal_tab();
                self.table_state.select(Some(0));
            }
            _ => {}
        }
    }

    fn scroll_processes_to_start(&mut self) {
        let len = self.filtered_row_count();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        self.table_state.select(Some(0));
    }

    fn scroll_processes_to_end(&mut self) {
        let len = self.filtered_row_count();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        self.table_state.select(Some(len - 1));
    }

    fn row_visible(&self, row: &ProcessRow) -> bool {
        self.process_name_filters.is_empty() || self.process_name_filters.contains(&row.process)
    }

    fn toggle_monitor(&mut self, now: Instant) -> bool {
        self.monitor_enabled = !self.monitor_enabled;
        if self.monitor_enabled {
            self.next_monitor_request_at = Some(now + PROCESS_MONITOR_INTERVAL);
            true
        } else {
            self.next_monitor_request_at = None;
            false
        }
    }
}

fn build_selected_process_markdown(state: &ProcessViewState) -> Option<String> {
    let selected_idx = state.table_state.selected()?;
    let rows = state.visible_rows();
    let row = rows.get(selected_idx)?;
    Some(build_process_table_markdown(&[*row]))
}

fn build_process_table_markdown(rows: &[&ProcessRow]) -> String {
    use std::fmt::{Display, Write};

    const PID_W: usize = 5;
    const PPID_W: usize = 5;
    const STATE_W: usize = 10;
    const CPU_W: usize = 4;
    const RAM_W: usize = 8;
    const CONN_W: usize = 5;

    #[allow(clippy::too_many_arguments)]
    fn process_table_row(
        out: &mut String,
        process_w: usize,
        pid: impl Display,
        ppid: impl Display,
        process: impl Display,
        state: impl Display,
        cpu: impl Display,
        ram: impl Display,
        conn: impl Display,
    ) {
        writeln!(
            out,
            "| {pid:>pid_w$} | {ppid:>ppid_w$} | {process:<process_w$} | {state:<state_w$} | {cpu:>cpu_w$} | {ram:>ram_w$} | {conn:>conn_w$} |",
            pid_w = PID_W,
            ppid_w = PPID_W,
            state_w = STATE_W,
            cpu_w = CPU_W,
            ram_w = RAM_W,
            conn_w = CONN_W,
        ).unwrap();
    }
    let process_w = rows.iter().map(|row| row.process.len()).max().unwrap_or(0).max("Process".len());

    let mut out = String::new();
    process_table_row(&mut out, process_w, "PID", "PPID", "Process", "State", "CPU", "RAM (KB)", "Conn");
    let width = out.len() - 1;
    out.push_str(&"-".repeat(width));
    out.push('\n');

    for row in rows {
        process_table_row(
            &mut out,
            process_w,
            row.pid,
            row.ppid,
            row.process.as_str(),
            row.state.as_str(),
            format!("{}%", row.cpu_percent),
            row.ram_kb,
            row.connections,
        );
    }

    out
}

impl ProcessSortField {
    pub fn label(&self) -> &'static str {
        match self {
            ProcessSortField::Pid => "PID",
            ProcessSortField::Ppid => "PPID",
            ProcessSortField::Process => "Process",
            ProcessSortField::State => "State",
            ProcessSortField::Cpu => "CPU",
            ProcessSortField::Ram => "RAM",
            ProcessSortField::Connections => "Connections",
        }
    }

    fn index(&self) -> usize {
        match self {
            ProcessSortField::Pid => 0,
            ProcessSortField::Ppid => 1,
            ProcessSortField::Process => 2,
            ProcessSortField::State => 3,
            ProcessSortField::Cpu => 4,
            ProcessSortField::Ram => 5,
            ProcessSortField::Connections => 6,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => ProcessSortField::Pid,
            1 => ProcessSortField::Ppid,
            2 => ProcessSortField::Process,
            3 => ProcessSortField::State,
            4 => ProcessSortField::Cpu,
            5 => ProcessSortField::Ram,
            6 => ProcessSortField::Connections,
            _ => ProcessSortField::Pid,
        }
    }
}
