// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        state::AppState,
        store::{fingerprint_for_passphrase, CreateSingleSigAccount},
        AccountView, Callbacks, EnterPassphrase, EnterPassphraseState, Navigate,
    },
    slint_keyos_platform::{
        slint::{ComponentHandle, ModelRc},
        spawn_local, spawn_worker, StoredValue,
    },
};

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<EnterPassphrase>();

    global.on_try(move |passphrase| {
        try_passphrase(state, passphrase.into());
    });

    global.on_confirm(move |passphrase| confirm_passphrase(state, passphrase.into()));

    global.on_clear_passphrase(move || AppState::apply_passphrase(state, String::new()));

    global.on_reapply(move |passphrase| AppState::apply_passphrase(state, passphrase.into()));

    global.on_switch_view(move |index| {
        let passphrase: String = {
            if index == 0 {
                String::new()
            } else {
                let app_state = state.borrow();
                let ui = app_state.ui();
                ui.global::<EnterPassphrase>().get_passphrase().into()
            }
        };
        AppState::switch_view_locally(state, passphrase);
    });

    global.on_create_initial_account(move |label| {
        let passphrase: String = state.borrow().ui().global::<EnterPassphrase>().get_passphrase().into();
        {
            let mut s = state.borrow_mut();
            let ui = s.ui();
            let cb = ui.global::<Callbacks>();
            cb.set_accounts_loading(true);
            if !s.archive_mode {
                s.skeleton_count = 1;
                s.model.clear();
                s.model.push(AccountView { skeleton: true, ..Default::default() });
                cb.set_accounts(ModelRc::from(s.model.clone()));
            }
            ui.global::<EnterPassphrase>().set_state(EnterPassphraseState::Clear);
            ui.global::<Navigate>().invoke_backward();
        }
        let handle = spawn_local(async move {
            create_initial_account(state, label.into(), passphrase).await;
        });
        state.borrow_mut().pending_passphrase_task = Some(handle);
    });
}

fn try_passphrase(state: StoredValue<AppState>, passphrase: String) {
    state.borrow().ui().global::<EnterPassphrase>().set_fingerprint_loading(true);
    let handle = spawn_local(async move {
        let fingerprint = spawn_worker({
            let passphrase = passphrase.clone();
            async move { fingerprint_for_passphrase(&passphrase) }
        })
        .await
        .unwrap_or_default();

        let app_state = state.borrow();
        let ui = app_state.ui();
        let global = ui.global::<EnterPassphrase>();
        global.set_fingerprint(fingerprint.to_string().to_uppercase().into());
        global.set_no_accounts(app_state.store.num_single_accounts(Some(fingerprint)) == 0);
        global.set_fingerprint_loading(false);
    });
    state.borrow_mut().pending_try_passphrase = Some(handle);
}

fn confirm_passphrase(state: StoredValue<AppState>, passphrase: String) {
    AppState::apply_passphrase(state, passphrase);
    let app_state = state.borrow_mut();
    let ui = app_state.ui();
    let global = ui.global::<EnterPassphrase>();
    global.set_state(EnterPassphraseState::Clear);
    ui.global::<Navigate>().invoke_backward();
}

async fn create_initial_account(state: StoredValue<AppState>, label: String, passphrase: String) {
    AppState::apply_passphrase_inner(state, passphrase).await;

    AppState::create_singlesig_account(
        state,
        CreateSingleSigAccount { label, network: ngwallet::bdk_wallet::bitcoin::Network::Bitcoin, index: 0 },
    )
    .await
    .inspect_err(|e| log::error!("failed to create single sig account: {e:?}"))
    .ok();

    {
        let mut s = state.borrow_mut();
        s.skeleton_count = 0;
        s.refresh_slint_accounts();
        s.ui().global::<crate::Callbacks>().set_accounts_loading(false);
    }
    AppState::publish_accounts_and_passphrase(state);
}
