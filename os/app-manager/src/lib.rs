// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use log::{debug, error, info};
use server::{
    ArchiveEventSubscriber, ArchiveEventSubscriptionHandler, BlockingArchiveHandler, BlockingScalar,
    BlockingScalarHandler, MessageId as _, ScalarHandler, Server, ServerContext,
};
use xous::{AppId, SystemEvent, PID};

mod launch;
mod registry;
mod system_messages;

use app_manager::{AppEvent, LaunchError};
use app_manager::{GetAppName, GetQrMatchRules, LaunchApp, LaunchAppBlocking, SubscribeAppEvents};
use system_messages::{ChildCrashed, Disconnected};

use crate::launch::launch_app;
use crate::registry::AppRegistry;

pub fn listen() { server::listen(AppManagerServer::new().unwrap()) }

#[derive(server::Server)]
#[name = "os/app-manager"]
pub struct AppManagerServer {
    app_event_subscribers: Vec<ArchiveEventSubscriber<AppEvent>>,
    app_registry: AppRegistry,
    panic_message_buf: xous::MemoryRange,
}

impl Default for AppManagerServer {
    fn default() -> Self {
        let panic_message_buf =
            xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
                .expect("Failed to allocate panic message buffer");

        Self {
            app_event_subscribers: Vec::default(),
            app_registry: AppRegistry::default(),
            panic_message_buf,
        }
    }
}

impl BlockingArchiveHandler<GetQrMatchRules> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: GetQrMatchRules,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<app_manager::AppQrMatchRules> {
        self.app_registry.qr_match_rules()
    }
}

impl Server for AppManagerServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.app_registry.scan_installed_apps().expect("Failed to scan installed apps");

        xous::register_system_event_handler(SystemEvent::ChildTerminated, context.sid(), ChildCrashed::ID)
            .expect("Failed to register child terminated handler");
        xous::register_system_event_handler(SystemEvent::Disconnected, context.sid(), Disconnected::ID)
            .expect("Failed to register disconnected handler");
    }
}

impl AppManagerServer {
    pub fn new() -> anyhow::Result<Self> { Ok(Self::default()) }
}

impl BlockingScalarHandler<LaunchAppBlocking> for AppManagerServer {
    fn handle(
        &mut self,
        LaunchAppBlocking(app_id): LaunchAppBlocking,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <LaunchAppBlocking as BlockingScalar>::Response {
        info!("PID {sender} is launching app 0x{}", hex::encode(app_id.0));

        let pid = self.launch_app(app_id, sender)?;
        Ok(pid)
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeAppEvents> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: SubscribeAppEvents,
        subscriber: ArchiveEventSubscriber<AppEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        debug!("New app event subscriber: {:?}", subscriber);

        self.app_event_subscribers.push(subscriber);
        Ok(())
    }
}

impl ScalarHandler<LaunchApp> for AppManagerServer {
    fn handle(&mut self, LaunchApp(app_id): LaunchApp, sender: PID, _context: &mut ServerContext<Self>) {
        info!("PID {sender} is asynchronously launching app 0x{}", hex::encode(app_id.0));
        if let Err(e) = self.launch_app(app_id, sender) {
            if let Some(s) = self.app_event_subscribers.iter().find(|s| s.pid() == sender) {
                let event = AppEvent::LaunchError { app_id: (&app_id).into(), error: e };
                if s.send(&event).is_err() {
                    error!("Failed to send launch error to subscriber PID {sender}");
                }
            }
        }
    }
}

impl BlockingArchiveHandler<GetAppName> for AppManagerServer {
    fn handle(
        &mut self,
        msg: GetAppName,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<String> {
        match msg {
            GetAppName::ByAppId { id, locale } => self.app_registry.app_name_by_id(&id.into(), &locale),
            GetAppName::ByPid { pid, locale } => self.app_registry.app_name_by_pid(pid, &locale),
        }
    }
}

impl ScalarHandler<ChildCrashed> for AppManagerServer {
    fn handle(
        &mut self,
        ChildCrashed(exit_code): ChildCrashed,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Some(app_id) = self.app_registry.app_id_by_pid(sender) else {
            error!("Failed to get app ID for PID {sender}");
            return;
        };

        let Some(launched_by) = self.app_registry.launched_by(app_id) else {
            error!("Failed to find launched_by PID for app ID 0x{}", hex::encode(app_id.0));
            return;
        };

        let event = AppEvent::AppCrashed {
            app_id: app_id.into(),
            pid: sender,
            launched_by,
            exit_code,
            panic_message: if exit_code != 0 { self.read_panic_message(sender) } else { None },
        };
        self.app_event_subscribers.retain(|s| s.send(&event).is_ok());

        self.app_registry.terminate_app(sender);
    }
}

impl ScalarHandler<Disconnected> for AppManagerServer {
    fn handle(&mut self, _: Disconnected, sender: PID, _context: &mut ServerContext<Self>) {
        self.app_event_subscribers.retain(|s| s.pid() != sender);
    }
}

impl AppManagerServer {
    fn launch_app(&mut self, app_id: AppId, sender: PID) -> Result<PID, LaunchError> {
        let app_id_str = hex::encode(app_id.0);
        debug!("Launching app with ID: 0x{}", app_id_str);

        #[cfg(keyos)]
        let elf_path = self.app_registry.elf_path(app_id).ok_or(LaunchError::UnknownAppId)?;
        #[cfg(not(keyos))]
        let elf_path = self
            .app_registry
            .elf_path(app_id)
            .map(std::path::PathBuf::from)
            .ok_or(LaunchError::UnknownAppId)?;
        let pid = launch_app(&app_id, &elf_path)?;
        self.app_registry.register_running_app(pid, app_id, sender);

        debug!("Notifying app launch for app ID: 0x{}", app_id_str);
        let event = AppEvent::AppLaunched { app_id: (&app_id).into(), pid, launched_by: sender };
        self.app_event_subscribers.retain(|s| s.send(&event).is_ok());

        Ok(pid)
    }

    fn read_panic_message(&mut self, child_pid: PID) -> Option<String> {
        if let Ok((panic_pid, panic_size)) = xous::get_panic_message(self.panic_message_buf) {
            if xous::PID::new(panic_pid) == Some(child_pid) {
                return String::from_utf8(self.panic_message_buf.as_slice()[..panic_size].to_owned()).ok();
            }
            log::debug!("Panic message PID mismatch: expected {child_pid}, got {panic_pid}");
        }
        None
    }
}
