// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::Context;
use slint_keyos_platform::{
    gui_server_api::navigation::qrscanner::{ScanQrOptions, ScanQrResult},
    navigation::open_qr_scanner,
    slint::{ComponentHandle, ToSharedString},
    spawn_local, spawn_worker, StoredValue,
};

use crate::{
    account_id::AccountId, gui_permissions::GuiPermissions, state::AppState, tr, Animate, CheckedRanges,
    ExploreAddressParams, Navigate, NavigateOptions, TrId, VerifyAddress, VerifyAddressOptions,
    VerifyAddressState,
};

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<VerifyAddress>();

    global.on_verify_address({
        move |opt| {
            let VerifyAddressOptions { account_id, show_skip_button, show_view_all, .. } = opt;
            let Ok(Some(scan)) = open_qr_scanner::<GuiPermissions>(ScanQrOptions {
                header_title: tr::lookup_id(TrId::VerifyAddressTitle).to_string(),
                header_right_text: if show_skip_button {
                    tr::lookup_id(TrId::CommonButtonSkip).to_string()
                } else {
                    String::from("")
                },
                button_text: if show_view_all {
                    tr::lookup_id(TrId::VerifyAddressExploreAllAddresses).to_string()
                } else {
                    String::from("")
                },
                button_icon: String::from(if show_view_all { "list" } else { "" }),
                ..ScanQrOptions::default()
            })
            .inspect_err(|e| log::error!("failed to open qr scanner: {}", e)) else {
                return;
            };

            let ui = state.borrow().ui();
            let nav = ui.global::<Navigate>();
            let verify = ui.global::<VerifyAddress>();

            match scan {
                ScanQrResult::Qr { data, .. } => {
                    let Ok(address) = String::from_utf8(data)
                        .inspect_err(|e| log::error!("failed to decode qr scanner data: {}", e))
                    else {
                        return;
                    };
                    nav.invoke_verify_address(
                        crate::VerifyAddressParams { account_id: account_id.clone() },
                        NavigateOptions { animate: Animate::None, replace: false },
                    );
                    spawn_local(async move {
                        verify_address(state, address, 0, account_id.into())
                            .await
                            .inspect_err(|e| {
                                log::error!("Could not verify address: {:?}", e);
                            })
                            .ok();
                    })
                    .detach();
                }
                // cancelled
                ScanQrResult::LeftClicked => {}
                // skipped
                ScanQrResult::RightClicked => {
                    nav.invoke_verify_address(
                        crate::VerifyAddressParams { account_id: account_id.clone() },
                        NavigateOptions { animate: Animate::None, replace: false },
                    );
                    verify.set_state(VerifyAddressState::Skipped);
                }
                ScanQrResult::ButtonClicked => {
                    nav.invoke_explore_address(ExploreAddressParams { account_id }, Default::default());
                }
                action @ _ => {
                    log::error!("verify address failed: {:?}", action);
                    nav.invoke_verify_address(
                        crate::VerifyAddressParams { account_id: account_id.clone() },
                        NavigateOptions { animate: Animate::None, replace: false },
                    );
                    verify.set_state(VerifyAddressState::Invalid);
                }
            }
        }
    });

    global.on_continue_verify_address({
        move |opt, address, attempt_number| {
            let VerifyAddressOptions { account_id, .. } = opt;
            spawn_local(async move {
                verify_address(state, address.into(), attempt_number as u32, account_id.into())
                    .await
                    .inspect_err(|e| {
                        log::error!("Could not verify address: {:?}", e);
                    })
                    .ok();
            })
            .detach();
        }
    });
}

const VERIFY_ADDRESS_CHUNK_SIZE: u32 = 200;

async fn verify_address(
    state: StoredValue<AppState>,
    address: String,
    attempt_number: u32,
    account_id: String,
) -> anyhow::Result<()> {
    let account_id = account_id.parse::<AccountId>().context("invalid account id")?;

    let ui = state.borrow().ui();
    let global = ui.global::<VerifyAddress>();

    // Bluewallet adds this prefix to address QR codes
    let address = address.strip_prefix("bitcoin:").map(String::from).unwrap_or(address);
    // Bluewallet produces uppercase bech32 addresses which are invalid per BIP173
    // Normalize bech32 addresses to lowercase (legacy base58 addresses are case-sensitive)
    let address = if address.to_lowercase().starts_with("bc1") || address.to_lowercase().starts_with("tb1") {
        address.to_lowercase()
    } else {
        address
    };

    global.set_state(VerifyAddressState::Loading);
    global.set_address(address.to_shared_string());

    let account = match state.borrow_mut().store.get_account_or_fail(&account_id) {
        Ok(a) => a.clone(),
        Err(e) => {
            log::error!("failed to get account for address verification: {e:?}");
            global.set_state(VerifyAddressState::Invalid);
            return Ok(());
        }
    };

    let result = spawn_worker({
        let address = address.clone();
        async move { account.verify_address(address, attempt_number, VERIFY_ADDRESS_CHUNK_SIZE) }
    })
    .await;

    let verification_result = match result {
        Ok(result) => result,
        Err(e) => {
            log::error!("failed verify address : {e:?}, address: {address:?}");
            global.set_state(VerifyAddressState::Invalid);
            return Ok(());
        }
    };

    match verification_result.found_index {
        Some(index) => {
            global.set_state(VerifyAddressState::Success);
            global.set_index(index as i32);
        }
        None => {
            global.set_state(VerifyAddressState::Error);
            global.set_checked_ranges(CheckedRanges {
                change_start: verification_result.change_lower as i32,
                change_end: verification_result.change_upper as i32,
                receive_start: verification_result.receive_lower as i32,
                receive_end: verification_result.receive_upper as i32,
            });
            global.set_attempt_number(attempt_number as i32);
        }
    }

    Ok(())
}
