// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![feature(must_not_suspend)]
#![deny(must_not_suspend)]

use {
    crate::state::AppState,
    slint_keyos_platform::{
        app,
        gui_server_api::{navigation::qrscanner::MatchedQrResult, InputMessage},
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

        let ql_status = state.borrow().ql_status.clone();
        ql_status.ready().await;
        log::info!("QL ready, syncing accounts and passphrase");
        AppState::publish_accounts_and_passphrase(state);
        AppState::publish_fiat_preference(state);

        loop {
            ql_status.wait_until(|s| !s.bt_connected || !s.ql_paired || !s.live).await;
            ql_status.ready().await;
            log::info!("QL reconnected, re-syncing accounts and passphrase");
            AppState::publish_accounts_and_passphrase(state);
            AppState::publish_fiat_preference(state);
        }
    })
    .detach();

    spawn_local(async move {
        let mut exchange_rate = subscribe_archive::<quantum_link_permissions::QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribeExchangeRate,
        );
        while let Some(exchange_rate) = exchange_rate.next().await {
            log::info!("Received QuantumLink exchange rate: {:?}", exchange_rate);
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
                if let Some(options) = MatchedQrResult::from_slice(&nav_bytes) {
                    let MatchedQrResult { scan_result, .. } = options;
                    callbacks::reset_for_incoming_scan(state);
                    if let Err(e) = callbacks::handle_scan_result(state, scan_result) {
                        log::error!("scan failed: {e:?}");
                    }
                    return;
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
