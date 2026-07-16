// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::widgets::{ListState, TableState};

use self::log::LogViewState;
use self::process::ProcessViewState;
use crate::transport::TransportCommand;

pub mod log;
pub mod process;

pub struct State {
    pub log: LogViewState,
    pub process: ProcessViewState,
    pub view_mode: ViewMode,
    pub help_open: bool,
    pub status_text: String,
    transport_tx: Sender<TransportCommand>,
    pub notifications: Vec<Notification>,
}

#[derive(Clone, Copy)]
pub enum ViewMode {
    Logs,
    ProcessList,
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub error: bool,
    pub message: String,
    pub expiration: Instant,
}

impl Notification {
    pub fn success(message: impl Into<String>, ttl: Duration) -> Self {
        Self::new(false, message.into(), ttl)
    }

    pub fn error(message: impl Into<String>, ttl: Duration) -> Self { Self::new(true, message.into(), ttl) }

    fn new(error: bool, message: String, ttl: Duration) -> Self {
        Self { error, message, expiration: Instant::now() + ttl }
    }
}

#[derive(Clone, Debug, Default)]
pub struct KeyResult {
    consumed: bool,
    notify: Option<Notification>,
    transport: Option<TransportCommand>,
}

impl KeyResult {
    pub fn ignore() -> Self { Self { consumed: false, notify: None, transport: None } }

    pub fn consumed() -> Self { Self { consumed: true, notify: None, transport: None } }

    pub fn set_notify(self, notification: Notification) -> Self {
        Self { consumed: self.consumed, transport: self.transport, notify: Some(notification) }
    }

    pub fn set_transport(self, cmd: TransportCommand) -> Self {
        Self { consumed: self.consumed, transport: Some(cmd), notify: self.notify }
    }
}

impl State {
    pub fn new(transport_tx: Sender<TransportCommand>) -> Self {
        Self {
            log: LogViewState::new(),
            process: ProcessViewState::new(),
            view_mode: ViewMode::Logs,
            help_open: false,
            status_text: String::new(),
            transport_tx,
            notifications: Vec::new(),
        }
    }

    pub fn handle_input(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => return self.handle_key_input(key),
            Event::Mouse(event) => match self.view_mode {
                ViewMode::Logs => self.log.handle_mouse(event.kind),
                ViewMode::ProcessList => self.process.handle_mouse(event.kind),
            },
            Event::Resize(_, _) => match self.view_mode {
                ViewMode::Logs => self.log.handle_resize(),
                ViewMode::ProcessList => (),
            },
            _ => (),
        }
        false
    }

    fn handle_key_input(&mut self, key: KeyEvent) -> bool {
        if matches!(key.kind, KeyEventKind::Release) {
            return false;
        }
        if matches!(key.code, KeyCode::Char('?') | KeyCode::Char('h')) {
            self.help_open = !self.help_open;
            return false;
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc) {
                self.help_open = false;
            }
            return false;
        }

        let mut key_result = match self.view_mode {
            ViewMode::Logs => self.log.handle_key(key.code),
            ViewMode::ProcessList => self.process.handle_key(key.code),
        };

        if let Some(command) = key_result.transport.take() {
            let _ = self.transport_tx.send(command);
        }

        if let Some(notification) = key_result.notify.take() {
            self.notifications.push(notification);
        }

        if key_result.consumed {
            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('v') => {
                self.toggle_view_mode();
            }
            _ => (),
        }
        false
    }

    pub fn handle_tick(&mut self) {
        let now = Instant::now();
        self.notifications.retain(|notification| now < notification.expiration);

        if let Some(command) = self.process.handle_tick(now) {
            let _ = self.transport_tx.send(command);
        }
    }

    fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Logs => ViewMode::ProcessList,
            ViewMode::ProcessList => ViewMode::Logs,
        };
    }
}

fn ensure_list_selection(list_state: &mut ListState, len: usize) {
    match (list_state.selected(), len) {
        (_, 0) => list_state.select(None),
        (None, _) => list_state.select(Some(0)),
        (Some(selected), _) if selected >= len => list_state.select(Some(len - 1)),
        _ => {}
    }
}

fn move_selection(list_state: &mut ListState, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        list_state.select(None);
        return None;
    }
    let current = list_state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
    list_state.select(Some(next));
    Some(next)
}

fn move_table_selection(table_state: &mut TableState, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        table_state.select(None);
        return None;
    }
    let current = table_state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
    table_state.select(Some(next));
    Some(next)
}

fn copy_to_clipboard(text: &str, success_message: &str) -> Notification {
    match exec_copy_to_clipboard(text) {
        Ok(()) => Notification::success(success_message, Duration::from_secs(3)),
        Err(err) => Notification::error(err, Duration::from_secs(3)),
    }
}

fn exec_copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|err| format!("Clipboard init failed: {err}"))?;
    clipboard.set_text(text).map_err(|err| format!("Clipboard write failed: {err}"))
}
