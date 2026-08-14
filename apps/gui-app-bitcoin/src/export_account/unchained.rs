// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        account_id::AccountId,
        export_account::{
            unchained_bip45, unchained_bip45_ur, unchained_format, ImportedAccountDefaults, UrExport,
            WalletConnector,
        },
        state::AccountColor,
        AppState, ExportCapabilities, ExportFormats, VisualFormat,
    },
    ngwallet::config::NgAccountConfig,
    slint_keyos_platform::slint::SharedString,
};

pub struct Connector;
pub static CONNECTOR: Connector = Connector;

impl WalletConnector for Connector {
    fn capabilities(&self) -> ExportCapabilities { ExportCapabilities { single: false, join_multisig: true } }

    fn formats(&self) -> ExportFormats { ExportFormats { visual: VisualFormat::UR2, file: true } }

    fn file_extension(&self, _as_multi: bool) -> String { String::from("json") }

    fn display_name(&self) -> SharedString { SharedString::from("Unchained") }

    fn imported_account_defaults(&self) -> Option<ImportedAccountDefaults> {
        Some(ImportedAccountDefaults { label: self.display_name(), color: AccountColor::DarkBlue })
    }

    fn connect(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        as_multi: bool,
    ) -> Result<String, anyhow::Error> {
        if !as_multi {
            anyhow::bail!("Unchained only supports multisig exports");
        }

        unchained_format(state, id, cfg)
    }

    fn connect_ur(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        as_multi: bool,
    ) -> Result<Option<UrExport>, anyhow::Error> {
        if !as_multi {
            anyhow::bail!("Unchained only supports multisig exports");
        }

        let (xpub, master_fingerprint) = unchained_bip45(state, id, cfg)?;
        unchained_bip45_ur(&xpub, master_fingerprint, cfg.network).map(Some)
    }
}
