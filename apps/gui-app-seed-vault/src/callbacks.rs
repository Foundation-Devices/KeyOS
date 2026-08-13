// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        error::ToValidationString,
        error::VaultError,
        gui_permissions::GuiPermissions,
        seed::SeedType,
        state::{AppState, PendingImport},
        tr, CallbackResult, Callbacks, ImportedSeedInfo, SeedCallbacks, SeedStatus, SeedView, SeedViewType,
        TrId,
    },
    anyhow::Context,
    bip85_extended::bip39::Mnemonic,
    nostr::{FromBech32, ToBech32},
    ordered_table::CardSortMode,
    slint_keyos_platform::{
        gui_server_api::navigation::qrscanner::{ScanQrOptions, ScanQrResult},
        navigation::open_qr_scanner,
        slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel},
        StoredValue,
    },
};

pub fn init_callbacks(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let callbacks = ui.global::<Callbacks>();

    callbacks.on_validate_new_label({
        move |label: SharedString| {
            let app_state = state.borrow();
            if let Err(e) = app_state.validate_new_label(label.to_string()) {
                return e.to_validation_string().into();
            }

            SharedString::new()
        }
    });

    callbacks.on_validate_new_index({
        move |seed_index: SharedString, view_type: SeedViewType| {
            let app_state = state.borrow();

            let seed_type =
                match SeedType::from_view_type(view_type, Some(seed_index.into()), None, None, None, None) {
                    Ok(st) => st,
                    Err(e) => return e.to_validation_string().into(),
                };

            if let Err(e) = app_state.validate_new_index(seed_type) {
                return e.to_validation_string().into();
            }

            SharedString::new()
        }
    });

    callbacks.on_save(move |seed_view: SeedView| state.borrow_mut().save_from_view(seed_view, None, None));

    callbacks.on_import_nsec({
        move |seed_view: SeedView, nsec| {
            let result = state.borrow_mut().save_from_view(seed_view, Some(nsec), None);

            if result.success {
                state.borrow_mut().pending_import = None;
            }

            result
        }
    });

    callbacks.on_import_seed_entropy({
        move |seed_view: SeedView, seed_entropy| {
            let result = state.borrow_mut().save_from_view(seed_view, None, Some(seed_entropy));
            if result.success {
                state.borrow_mut().pending_import = None;
            }
            result
        }
    });

    callbacks.on_set_archive_mode({
        move |archive_mode| {
            let mut app_state = state.borrow_mut();
            app_state.archive_mode = archive_mode;
            app_state.update_accounts();
        }
    });

    callbacks.on_set_sort_mode({
        move |sort_mode| {
            let mut app_state = state.borrow_mut();
            app_state.set_sort_mode(CardSortMode::from(sort_mode as usize));
            app_state.update_accounts();
        }
    });

    callbacks.on_move_position({
        move |index, up| {
            let mut app_state = state.borrow_mut();
            // Ignores errors, nothing happens
            let _ = app_state.move_position(index, up);
            app_state.update_accounts();
        }
    });

    callbacks.on_search({
        move |text| {
            let mut app_state = state.borrow_mut();
            app_state.search_text = text.to_string().to_lowercase();
            app_state.update_accounts();
        }
    });

    callbacks.on_get_next_index_string({
        move |seed_type: SeedViewType| {
            let app_state = state.borrow();
            format!("{}", app_state.get_next_index(seed_type)).into()
        }
    });

    callbacks.on_validate_edit_label({
        move |index, new_label| {
            let mut app_state = state.borrow_mut();
            if let Err(e) = app_state.validate_edit_label(index, new_label.into()) {
                return e.to_validation_string().into();
            }

            SharedString::new()
        }
    });

    callbacks.on_edit_indexed({
        move |index, new_label, new_color| {
            let mut app_state = state.borrow_mut();

            if let Err(e) = app_state.edit_indexed(index, new_label.into(), new_color as u8) {
                return CallbackResult::from(e);
            }

            app_state.update_accounts();
            CallbackResult::success()
        }
    });

    callbacks.on_edit_password({
        move |index, new_label, new_account, new_password, new_color| {
            let mut app_state = state.borrow_mut();

            if let Err(e) = app_state.edit_password(
                index,
                new_label.into(),
                new_account.into(),
                new_password.into(),
                new_color as u8,
            ) {
                return CallbackResult::from(e);
            }

            app_state.update_accounts();
            CallbackResult::success()
        }
    });

    callbacks.on_set_archived({
        move |index, archived| {
            let mut app_state = state.borrow_mut();

            if let Err(e) = app_state.set_archived(index, archived) {
                log::error!("{}", e);
                return;
            }

            app_state.update_accounts();
        }
    });

    callbacks.on_delete({
        move |index| {
            let mut app_state = state.borrow_mut();

            if let Err(e) = app_state.delete(index) {
                log::error!("{}", e);
                return;
            }

            app_state.update_accounts();
        }
    });

    callbacks.on_get_words({
        move |index| {
            let app_state = state.borrow();

            let words = app_state
                .get_entry_mnemonic(index)
                .map(|m| m.words().map(SharedString::from).collect::<Vec<SharedString>>())
                .unwrap_or_else(|e| {
                    log::error!("Could not get seed words: {:?}", e);
                    Vec::new()
                });

            ModelRc::new(VecModel::from(words))
        }
    });

    callbacks.on_get_standard_seed_qr({
        move |index| {
            let app_state = state.borrow();

            app_state.get_standard_seed_qr(index).unwrap_or_else(|e| {
                log::error!("Could not get seed qr: {:?}", e);
                Image::default()
            })
        }
    });

    callbacks.on_get_compact_seed_qr({
        move |index| {
            let app_state = state.borrow();

            app_state.get_compact_seed_qr(index).unwrap_or_else(|e| {
                log::error!("Could not get compact seed qr: {:?}", e);
                Image::default()
            })
        }
    });

    callbacks.on_get_npub({
        move |index| {
            let app_state = state.borrow();

            app_state.get_npub(index).map(SharedString::from).unwrap_or_else(|e| {
                log::error!("Could not get npub: {:?}", e);
                SharedString::new()
            })
        }
    });

    callbacks.on_get_nsec({
        move |index| {
            let app_state = state.borrow();

            app_state.get_nsec(index).map(SharedString::from).unwrap_or_else(|e| {
                log::error!("Could not get nsec: {:?}", e);
                SharedString::new()
            })
        }
    });

    callbacks.on_get_fingerprint({
        move |index| {
            let app_state = state.borrow();

            app_state.get_fingerprint(index).map(SharedString::from).unwrap_or_else(|e| {
                log::error!("Could not get fingerprint: {:?}", e);
                SharedString::new()
            })
        }
    });

    callbacks.on_scan_qr({
        move || {
            let opts = ScanQrOptions {
                header_title: tr::lookup_id(TrId::ImportItemTitle).into(),
                message: tr::lookup_id(TrId::ImportItemContent).into(),
                header_left_icon: String::from("chevron-left"),
                ..ScanQrOptions::default()
            };

            let scan = match open_qr_scanner::<GuiPermissions>(opts) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    log::info!("Nothing returned from qr scanner");
                    return;
                }
                Err(e) => {
                    log::error!("Error while scanning QR: {:?}", e);
                    return;
                }
            };

            match scan {
                ScanQrResult::Qr { data, .. } => {
                    state.borrow_mut().handle_qr_input(data);
                }
                ScanQrResult::LeftClicked => (),
                _ => {
                    log::error!("QR scan failed: unexpected result type");
                }
            }
        }
    });

    callbacks.on_generate_password({
        move |index, length| {
            let app_state = state.borrow();
            let index = index.trim().parse::<u32>().unwrap_or(0);
            let length = length.trim().parse::<u32>().unwrap_or(0);

            app_state.generate_password(index, length).map(SharedString::from).unwrap_or_else(|e| {
                log::error!("Could not generate password: {:?}", e);
                SharedString::new()
            })
        }
    });

    seed_quiz::init_seed_callbacks!(ui);

    callbacks.on_get_seed_fingerprint({
        move |words| {
            let app_state = state.borrow();
            words_to_mnemonic(words)
                .and_then(|mnemonic| app_state.get_seed_fingerprint(&mnemonic))
                .map(SharedString::from)
                .unwrap_or_else(|e| {
                    log::error!("Could not get fingerprint from seed words: {:?}", e);
                    SharedString::new()
                })
        }
    });

    callbacks.on_entropy_to_words({
        move |entropy| {
            entropy_to_words(entropy).unwrap_or_else(|e| {
                log::error!("Could not convert entropy to mnemonic: {:?}", e);
                ModelRc::new(VecModel::from(Vec::new()))
            })
        }
    });

    callbacks.on_words_to_entropy({
        move |words| {
            words_to_entropy(words).unwrap_or_else(|e| {
                log::error!("Could not convert words to mnemonic: {:?}", e);
                SharedString::new()
            })
        }
    });

    callbacks.on_nsec_to_npub({
        move |nsec| {
            nsec_to_npub(nsec).unwrap_or_else(|e| {
                log::error!("Could not convert nsec to npub: {:?}", e);
                SharedString::new()
            })
        }
    });

    callbacks.on_get_pending_nostr_key({
        move || match state.borrow().pending_import {
            Some(PendingImport::NostrKey(ref key)) => key.as_str().into(),
            _ => SharedString::default(),
        }
    });

    callbacks.on_get_pending_imported_seed({
        move || match state.borrow().pending_import {
            Some(PendingImport::ImportedSeed { ref entropy, ref fingerprint }) => ImportedSeedInfo {
                entropy: entropy.as_str().into(),
                fingerprint: fingerprint.as_str().into(),
            },
            _ => ImportedSeedInfo::default(),
        }
    });

    callbacks.on_get_seed_details(move |index: i32| state.borrow().get_seed_view_by_index(index));

    callbacks.on_set_pending_imported_seed({
        move |entropy: SharedString, fingerprint: SharedString| {
            state.borrow_mut().pending_import = Some(PendingImport::ImportedSeed {
                entropy: entropy.to_string(),
                fingerprint: fingerprint.to_string(),
            });
        }
    });

    callbacks.on_clear_pending_import({
        move || {
            state.borrow_mut().pending_import.take();
        }
    });
}

// Use inner functions to enable ? early returns
fn words_to_mnemonic(words: ModelRc<SharedString>) -> Result<Mnemonic, VaultError> {
    let words = words.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if words.iter().any(|word| word.trim().is_empty()) {
        return Err(anyhow::anyhow!("Seed words cannot be empty").into());
    }
    let mnemonic_str = words.join(" ");
    Ok(Mnemonic::parse_normalized(&mnemonic_str).context("Could not parse seed words")?)
}

fn words_to_entropy(words: ModelRc<SharedString>) -> Result<SharedString, VaultError> {
    let mnemonic = words_to_mnemonic(words)?;
    Ok(hex::encode_upper(mnemonic.to_entropy()).into())
}

fn entropy_to_words(entropy: SharedString) -> Result<ModelRc<SharedString>, VaultError> {
    let entropy = hex::decode(entropy).context("Could not decode entropy")?;
    let mnemonic =
        Mnemonic::from_entropy(entropy.as_slice()).context("Could not convert entropy to mnemonic")?;
    let words = mnemonic.words().map(SharedString::from).collect::<Vec<SharedString>>();
    Ok(ModelRc::new(VecModel::from(words)))
}

fn nsec_to_npub(nsec: SharedString) -> Result<SharedString, VaultError> {
    let secret = nostr::SecretKey::from_bech32(&nsec).context("Could not get nostr secret from nsec")?;
    let key = nostr::Keys::new(secret);
    Ok(key.public_key().to_bech32().map(SharedString::from).context("Could not build nostr npub")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_model(phrase: &str) -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(phrase.split_whitespace().map(SharedString::from).collect::<Vec<_>>()))
    }

    #[test]
    fn accepts_complete_12_and_24_word_seeds() {
        let twelve_words =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let twenty_four_words =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

        assert!(words_to_mnemonic(words_model(twelve_words)).is_ok());
        assert!(words_to_mnemonic(words_model(twenty_four_words)).is_ok());
    }

    #[test]
    fn rejects_incomplete_24_word_seed() {
        let mut words =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .split_whitespace()
                .map(SharedString::from)
                .collect::<Vec<_>>();
        words.resize(24, SharedString::new());

        assert!(words_to_mnemonic(ModelRc::new(VecModel::from(words))).is_err());
    }
}
