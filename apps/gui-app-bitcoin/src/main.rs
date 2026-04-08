// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![feature(must_not_suspend)]
#![deny(must_not_suspend)]

use {
    crate::{psbt_signing::PendingPsbt, state::AppState},
    quantum_link::PairingEvent,
    slint_keyos_platform::{
        app,
        gui_server_api::{
            navigation::bitcoin::{BitcoinAction, OpenBitcoinOptions},
            InputMessage,
        },
        spawn_local, subscribe_archive, StoredValue,
    },
};

mod account_id;
mod bitcoin_settings;
mod callbacks;
mod create_account;
mod enter_passphrase;
mod export_account;
mod load;
mod message_signing;
mod psbt_signing;
mod state;
mod store;
mod verify_address;

quantum_link::use_api!();
security::use_api!();

app!("Bitcoin Wallet");
fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.config.enable_swipe_back.set(false);

    let state = StoredValue::new(AppState::new(ui.as_weak()));

    spawn_local(async move {
        AppState::load_active_accounts(state)
            .await
            .inspect_err(|e| log::error!("failed to load accounts {e:?}"))
            .ok();
        AppState::publish_accounts(state);
    })
    .detach();

    spawn_local(async move {
        let mut exchange_rate = subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribeExchangeRate,
        );
        while let Some(exchange_rate) = exchange_rate.next().await {
            log::info!("Received quantum link exchange rate: {:?}", exchange_rate);
            let mut state = state.borrow_mut();
            state.settings.guard().exchange_rate = exchange_rate.into();
        }
    })
    .detach();

    spawn_local(async move {
        let mut events = subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribeAccountUpdate,
        );
        while let Some(update) = events.next().await {
            AppState::process_account_update(state, update)
                .await
                .inspect(|e| log::error!("failed to import account update {e:?}"))
                .ok();
        }
    })
    .detach();

    spawn_local(async move {
        let mut pairing_events = subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribePairingEvent,
        );
        while let Some(pairing_event) = pairing_events.next().await {
            if let PairingEvent::PairingComplete { .. } = pairing_event {
                log::info!("Quantum link re-paired, re-syncing passphrase state and accounts");
                AppState::resync_with_envoy(state);
            }
        }
    })
    .detach();

    bitcoin_settings::init_settings(state);
    callbacks::init_callbacks(state);
    verify_address::init(state);
    create_account::init(state);
    export_account::init(state);
    psbt_signing::init(state);
    message_signing::init(state);
    enter_passphrase::init(state);

    cx.set_input_handler({
        let gui_api = cx.gui.clone();
        move |input| {
            if input.msg == InputMessage::NavigationFocused {
                let Ok(Some(nav_bytes)) = gui_api.navigate_pending() else {
                    log::error!("Navigation focused but no pending nav request");
                    return;
                };

                let Some(options) = OpenBitcoinOptions::from_slice(&nav_bytes) else {
                    log::error!("Failed to parse OpenBitcoinOptions from nav request");
                    return;
                };

                match options.action {
                    BitcoinAction::Scan => {
                        // Use a limited scope to drop globals after resetting state
                        {
                            let ui = state.borrow().ui();
                            let sign_global = ui.global::<SignPsbt>();
                            // TODO: find a more robust way to reset SignPsbt State
                            sign_global.set_state(SignPsbtState::Idle);
                            sign_global.set_origin(PsbtOriginView::Qr);
                            sign_global.set_pending_psbt(PsbtView::default());
                            sign_global.set_show_account_not_found_modal(false);
                            sign_global.set_is_multisig_account(false);
                            sign_global.set_account_index("".into());
                            sign_global.set_show_account_archived_modal(false);
                            sign_global.set_file_save_state(FileSaveState::Idle);
                            sign_global.set_saved_file_path("".into());
                            sign_global.set_show_cant_sign_modal(false);
                            sign_global.set_needed_fingerprint("".into());
                            sign_global.set_found_fingerprints("".into());

                            let account_global = ui.global::<CreateAccount>();
                            // TODO: find a more robust way to reset CreateAccount State
                            account_global.set_state(CreateAccountState::Idle);
                            account_global.set_pending_multisig_account(MultiSigView::default());
                            account_global.set_new_account_id("".into());
                            account_global.set_prefilled_mode(false);
                            account_global.set_prefilled_index("".into());
                            account_global.set_prefilled_network(Network::Bitcoin);

                            // Navigate backward until we reach the home page
                            let navigate = ui.global::<Navigate>();
                            while navigate.get_has_backward() {
                                navigate.invoke_backward_animate(Animate::None);
                            }
                        }

                        // Reset AppState pending fields
                        {
                            let mut state_mut = state.borrow_mut();
                            state_mut.pending_multisig = None;
                            state_mut.pending_singlesig = None;
                            state_mut.pending_psbt = PendingPsbt::None;
                            state_mut.pending_archived_account_id = None;
                        }

                        if let Err(e) = callbacks::execute_scan(state, true) {
                            log::error!("scan failed: {e:?}");
                        }
                    }
                }
            }
        }
    });

    ui.run().expect("UI running");
}

pub fn log_ms<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let res = f();
    log::info!("{label} took {}ms", start.elapsed().as_millis());
    res
}

pub fn get_timestamp_in_milliseconds() -> String {
    let current_system_time = std::time::SystemTime::now();
    let duration = current_system_time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    duration.as_millis().to_string()
}
