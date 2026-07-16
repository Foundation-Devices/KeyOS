// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod parse;
mod state;
mod transport;
mod tui;

use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::parse::{parse_payload, ParseItem};
use crate::state::State;
use crate::transport::{start_transport_thread, TransportCommand, TransportEvent};

const MAX_EVENTS_PER_FRAME: usize = 32;

#[derive(Debug)]
pub enum AppEvent {
    Input(Event),
    Transport(TransportEvent),
}

#[derive(Parser)]
#[command(name = "keyos-log-viewer")]
#[command(about = "View keyOS logs from USB vendor interface")]
#[command(version)]
struct Args {
    /// Reconnect timeout in seconds (default: 3)
    #[arg(short, long, default_value = "3")]
    timeout: u64,
}

fn main() {
    let args = Args::parse();

    let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
    let (transport_tx, transport_rx) = mpsc::channel::<TransportCommand>();

    thread::spawn({
        let event_tx = event_tx.clone();
        move || {
            while let Ok(event) = event::read() {
                if event_tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
        }
    });

    start_transport_thread(Duration::from_secs(args.timeout), event_tx.clone(), transport_rx);

    let mut state = State::new(transport_tx.clone());
    state.log.refresh_filters();

    enable_raw_mode().unwrap();
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).unwrap();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|f| tui::draw(f, &mut state)).unwrap();
    run_loop(&event_rx, &mut state, &mut terminal);

    disable_raw_mode().unwrap();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).unwrap();
    terminal.show_cursor().unwrap();
}

fn run_loop(
    event_rx: &mpsc::Receiver<AppEvent>,
    state: &mut State,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) {
    loop {
        match event_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                if apply_event(state, event) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        for _ in 0..MAX_EVENTS_PER_FRAME {
            match event_rx.try_recv() {
                Ok(event) => {
                    if apply_event(state, event) {
                        return;
                    }
                }
                Err(_) => break,
            }
        }

        state.handle_tick();
        terminal.draw(|f| tui::draw(f, state)).unwrap();
    }
}

fn apply_event(state: &mut State, event: AppEvent) -> bool {
    match event {
        AppEvent::Input(event) => state.handle_input(event),
        AppEvent::Transport(event) => {
            match event {
                TransportEvent::Payload(payload) => {
                    if let Some(parsed) = parse_payload(&payload, &state.log.entries) {
                        match parsed {
                            ParseItem::Log(record) => state.log.push_entry(record),
                            ParseItem::ProcessSnapshot(snapshot) => {
                                state.process.handle_process_snapshot(snapshot);
                            }
                        }
                    }
                }
                TransportEvent::Status(status) => {
                    state.status_text = status;
                }
            }
            false
        }
    }
}
