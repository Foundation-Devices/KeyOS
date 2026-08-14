// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use ngwallet::bdk_wallet::bitcoin::bip32::Xpriv;

pub(crate) struct SensitiveXpriv(pub(crate) Xpriv);

impl Drop for SensitiveXpriv {
    fn drop(&mut self) { self.0.private_key.non_secure_erase(); }
}
