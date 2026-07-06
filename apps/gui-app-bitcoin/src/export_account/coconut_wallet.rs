// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        account_id::AccountId,
        export_account::{generic_format_with_model, WalletConnector},
        AppState, ExportCapabilities, ExportFormats, VisualFormat,
    },
    ngwallet::config::NgAccountConfig,
    slint_keyos_platform::slint::SharedString,
};

pub struct Connector;
pub static CONNECTOR: Connector = Connector;

impl WalletConnector for Connector {
    fn capabilities(&self) -> ExportCapabilities { ExportCapabilities { single: true, join_multisig: false } }

    fn formats(&self) -> ExportFormats { ExportFormats { visual: VisualFormat::UR2, file: true } }

    fn file_extension(&self, _as_multi: bool) -> String { String::from("json") }

    fn display_name(&self) -> SharedString { SharedString::from("Coconut Wallet") }

    fn connect(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        _as_multi: bool,
    ) -> Result<String, anyhow::Error> {
        // Tag the payload so Coconut Wallet can label the imported wallet "Passport Prime".
        generic_format_with_model(state, id, cfg, false, Some("passport-prime"))
    }
}
