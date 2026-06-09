// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint_keyos_platform::{
    settings::global::OnboardingStatus,
    slint::{self, ComponentHandle},
    StoredValue,
};

use crate::{state::AppState, FileSystem, FwUpdateError, FwUpdateState, Navigate, OnboardingCallbacks};

pub struct DebugAction {
    pub label: &'static str,
    pub action: Box<dyn Fn() + 'static>,
}

pub fn init(state: StoredValue<AppState>) {
    let actions: Vec<DebugAction> = vec![
        // Quick Setup
        DebugAction {
            label: "Quick Setup",
            action: Box::new(move || {
                log::info!("Quick setting up");

                let mut seed_bytes = [0u8; 32];
                getrandom::getrandom(&mut seed_bytes).ok();
                let seed = security::Seed::from_bytes(&seed_bytes);

                if let Err(e) = state.borrow().security.set_seed_and_pin(
                    seed,
                    "123456".to_string(),
                    security::PinEntryMode::Pin,
                ) {
                    log::error!("Error setting pin and seed: {e:?}");
                    return;
                }
                FileSystem::default().format_encrypted_volume();
                state.borrow().settings.set_onboarding_status(OnboardingStatus::Complete);
                log::info!("Quick setup done");
                state.borrow_mut().finished = true;
                state
                    .borrow()
                    .gui
                    .switch_to_launcher()
                    .inspect_err(|e| log::warn!("failed to switch to launcher: {e:?}"))
                    .ok();
            }),
        },
        // Exit (Debug)
        DebugAction {
            label: "Exit (Debug)",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let cb = ui.global::<OnboardingCallbacks>();
                cb.invoke_finish_onboarding();
            }),
        },
        // Master Key Erased
        DebugAction {
            label: "Master Key Erased",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_master_key_deleted_main(Default::default());
            }),
        },
        // Welcome & Setup
        DebugAction {
            label: "Welcome",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_welcome(Default::default());
            }),
        },
        // Connect
        DebugAction {
            label: "Scan QR",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_scan_qr(Default::default());
            }),
        },
        DebugAction {
            label: "Check Envoy",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_check_envoy(Default::default());
            }),
        },
        // Update
        DebugAction {
            label: "Update Device",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_update_device(Default::default());
            }),
        },
        DebugAction {
            label: "Update Progress - Download",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let cb = ui.global::<OnboardingCallbacks>();
                cb.set_fw_update_state(FwUpdateState::Downloading);
                let nav = ui.global::<Navigate>();
                nav.invoke_update_progress(Default::default());
            }),
        },
        DebugAction {
            label: "Update Progress - Receive",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let cb = ui.global::<OnboardingCallbacks>();
                cb.set_fw_update_state(FwUpdateState::Receiving);
                let nav = ui.global::<Navigate>();
                nav.invoke_update_progress(Default::default());
            }),
        },
        DebugAction {
            label: "Update Progress - Installing",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let cb = ui.global::<OnboardingCallbacks>();
                cb.set_fw_update_state(FwUpdateState::Installing);
                let nav = ui.global::<Navigate>();
                nav.invoke_update_progress(Default::default());
            }),
        },
        DebugAction {
            label: "Update Progress - Failed",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let cb = ui.global::<OnboardingCallbacks>();
                cb.set_fw_update_state(FwUpdateState::Failed);
                cb.set_fw_update_error(FwUpdateError::DownloadFailed);
                let nav = ui.global::<Navigate>();
                nav.invoke_update_progress(Default::default());
            }),
        },
        // Set Pin
        DebugAction {
            label: "Set Pin",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_set_pin_info(Default::default());
            }),
        },
        // Master Seed
        DebugAction {
            label: "Master Seed - Start",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_master_seed(Default::default());
            }),
        },
        DebugAction {
            label: "Create Master Seed",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_create_master_seed(Default::default());
            }),
        },
        // Seed - Recover
        DebugAction {
            label: "Restore Seed - Start",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_restore_seed(Default::default());
            }),
        },
        DebugAction {
            label: "Restore Keycard",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_restore_keycard_backup(Default::default());
            }),
        },
        DebugAction {
            label: "Enter Backup Code",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_enter_backup_code(Default::default());
            }),
        },
        DebugAction {
            label: "Enter Backup Words",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_enter_backup_words(Default::default());
            }),
        },
        DebugAction {
            label: "Restore Seed Words",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_restore_seed_words(Default::default());
            }),
        },
        // Backup - Manual
        DebugAction {
            label: "Manual Backup",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_manual_backup(Default::default());
            }),
        },
        DebugAction {
            label: "Manual Keycard Backup",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_manual_keycard_backup(Default::default());
            }),
        },
        DebugAction {
            label: "Manual Backup Seed",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_manual_backup_seed(Default::default());
            }),
        },
        DebugAction {
            label: "Verify Backup Seed Words",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_verify_seed_words(Default::default());
            }),
        },
        // Backup - Magic
        DebugAction {
            label: "Magic Backup",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_magic_backup(Default::default());
            }),
        },
        DebugAction {
            label: "Creating Magic Backup",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_creating_magic_backup(Default::default());
            }),
        },
        DebugAction {
            label: "Backup Created",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_backup_created(Default::default());
            }),
        },
        // Connect Wallet
        DebugAction {
            label: "Connect Wallet",
            action: Box::new(move || {
                let ui = state.borrow().ui();
                let nav = ui.global::<Navigate>();
                nav.invoke_connect_wallet(Default::default());
            }),
        },
    ];

    let labels: Vec<slint::SharedString> = actions.iter().map(|a| a.label.into()).collect();

    let ui = state.borrow().ui();
    let cb = ui.global::<OnboardingCallbacks>();
    cb.set_debug_actions(slint::ModelRc::new(slint::VecModel::from(labels)));

    cb.on_run_debug_action(move |index: i32| {
        if let Some(action) = actions.get(index as usize) {
            (action.action)();
        }
    });
}
