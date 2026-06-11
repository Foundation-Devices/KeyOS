// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use xous::CID;

use crate::GuiServerError;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), GuiServerError>)]
pub struct RegisterAppMessage(pub crate::RegisterApp);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), GuiServerError>)]
pub struct RegisterControlCenter {
    pub cid: CID,
    pub height: usize,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), GuiServerError>)]
pub struct RegisterKeyboard {
    pub cid: CID,
    pub height: usize,
}

/// A message that claims a privileged role. Restricts [`crate::GuiApi::register_with_role`]
/// to the role-claim messages, so it can't be used to send arbitrary scalars.
pub trait RoleClaim: server::BlockingScalar + Default {}

macro_rules! declare_claim {
    ($name:ident) => {
        #[derive(Debug, Default, server::Message)]
        #[response(())]
        pub struct $name;

        impl RoleClaim for $name {}
    };
}

declare_claim!(ClaimLauncherRole);
declare_claim!(ClaimSettingsRole);
declare_claim!(ClaimOnboardingRole);
declare_claim!(ClaimSwitcherRole);
declare_claim!(ClaimLockScreenRole);
declare_claim!(ClaimAlertsRole);
