// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use quantum_link::SendMessageError;
use security::{messages::RawPin, PinEntryMode};

pub mod erase;
pub mod keycard_backup;
pub mod keycard_restore;
pub mod seed_challenge;
pub mod setup_seed;

use bt::BtAddr;
use slint_keyos_platform::{
    slint::{self, ComponentHandle},
    TaskHandle,
};

use crate::{
    AppWindow, BluetoothApi, GuiApi, QlStatus, QuantumLinkApi, RouteOption, RouteState, Security,
    SettingsApi, UpdateApi,
};

pub struct AppState {
    pub ui: slint::Weak<AppWindow>,
    pub gui: Arc<GuiApi>,

    pub bt_address: BtAddr,
    pub ql_status: QlStatus,

    pub bluetooth: BluetoothApi,
    pub settings: SettingsApi,
    pub security: Security,
    pub quantum: QuantumLinkApi,
    pub update: UpdateApi,

    pub pending_set_pin: Option<PendingPin>,

    // Flow handling keycard backups (if started).
    pub keycard_backup: Option<keycard_backup::KeycardBackupFlow>,
    // Flow handling manual keycard restore (if started).
    pub keycard_restore: Option<keycard_restore::KeycardRestoreFlow>,

    pub notify_onboarding_state: Option<TaskHandle<Result<(), SendMessageError>>>,
    pub notify_update_progress: Option<TaskHandle<Result<(), SendMessageError>>>,
    pub ql_status_monitor: Option<TaskHandle<()>>,

    pub finished: bool,
}

#[derive(Clone)]
pub struct PendingPin {
    pub pin: RawPin,
    pub pin_entry: PinEntryMode,
}

impl AppState {
    pub fn new(ui: slint::Weak<AppWindow>, gui: Arc<GuiApi>, security: Security) -> Self {
        AppState {
            ui,
            gui,

            bt_address: [0; 6].into(),
            ql_status: QlStatus::new(slint_keyos_platform::worker().clone()),

            security,
            bluetooth: BluetoothApi::default(),
            settings: SettingsApi::default(),
            quantum: QuantumLinkApi::default(),
            update: UpdateApi::default(),

            pending_set_pin: None,

            keycard_backup: None,
            keycard_restore: None,

            notify_onboarding_state: None,
            notify_update_progress: None,
            ql_status_monitor: None,

            finished: false,
        }
    }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }

    // cancel background tasks when the user navigates away.
    pub fn cancel_tasks(&mut self) {
        let ui = self.ui();
        let active_route = ui.global::<RouteState>().get_active();
        if active_route != RouteOption::ManualKeycardBackup
            && active_route != RouteOption::CreatingMagicBackup
            && self.keycard_backup.take().is_some()
        {
            log::info!("Cancelled keycard backup flow {active_route:?}");
        }
        if active_route != RouteOption::RestoreKeycardBackup
            && active_route != RouteOption::RestoreMagicBackup
            && self.keycard_restore.take().is_some()
        {
            log::info!("Cancelled keycard restore flow {active_route:?}");
        }
    }

    pub fn get_pending_pin(&self) -> anyhow::Result<PendingPin> {
        self.pending_set_pin.clone().ok_or_else(|| anyhow::anyhow!("No pending pin"))
    }

    pub fn try_get_seed(&self) -> anyhow::Result<security::Seed> {
        self.security
            .seed()
            .map_err(|e| anyhow::anyhow!("Failed to get seed from security service: {:?}", e))?
            .ok_or_else(|| anyhow::anyhow!("No seed"))
    }

    pub fn try_get_mnemonic_words(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.try_get_seed()?.to_mnemonic_words()?)
    }

    pub fn clear_pending_set_pin(&mut self) { self.pending_set_pin = None; }
}
