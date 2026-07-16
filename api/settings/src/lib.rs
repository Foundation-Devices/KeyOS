// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Settings server API for reading, writing, and subscribing to settings.

pub use types::inner as global;

mod macros;
pub mod messages;
mod types;

use messages::*;
use server::{CheckedConn, CheckedPermissions, MessageAllowed};

#[macro_export]
macro_rules! use_api {
    ($settings:path, $server:path) => {
        mod settings_permissions {
            use settings::messages::*;
            use $server as server;
            pub use $settings as settings;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/settings"]
            pub struct SettingsPermissions;
        }
        type SettingsApi =
            settings_permissions::settings::SettingsApi<settings_permissions::SettingsPermissions>;
    };
    () => {
        settings::use_api!(settings, server);
    };
}

#[derive(Debug, Default, Clone)]
pub struct SettingsApi<P: CheckedPermissions> {
    pub(crate) conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> SettingsApi<P> {
    pub fn get_prime_color(&self) -> global::SystemTheme
    where
        P: MessageAllowed<GetPrimeColor>,
    {
        self.conn.send_blocking_scalar(GetPrimeColor)
    }

    pub fn flush_settings(&self)
    where
        P: MessageAllowed<FlushAll>,
    {
        self.conn.send_scalar(FlushAll { force: true })
    }

    pub fn wait_for_onboarding_complete(&self)
    where
        P: 'static,
        P: MessageAllowed<SubscribeOnboardingStatus>,
    {
        server::listen(WaitForOnboarding(self.clone()));
    }

    pub fn lookup_timezone(&self, name: String, offset_minutes: i32) -> global::TimeZone
    where
        P: MessageAllowed<LookupTimeZone>,
    {
        self.conn.send_blocking_archive(LookupTimeZone { name, offset_minutes })
    }
}

pub struct WaitForOnboarding<P: CheckedPermissions>(pub SettingsApi<P>);

impl<P: CheckedPermissions> server::ServerMessages for WaitForOnboarding<P> {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>]
    where
        Self: Sized,
    {
        &[]
    }
}

impl<P: CheckedPermissions> server::Server for WaitForOnboarding<P>
where
    P: MessageAllowed<SubscribeOnboardingStatus>,
{
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.0.server_subscribe_onboarding_status(context);
    }
}

impl<P: CheckedPermissions> server::ArchiveEventHandler<global::OnboardingStatus> for WaitForOnboarding<P>
where
    P: MessageAllowed<SubscribeOnboardingStatus>,
{
    fn handle(
        &mut self,
        msg: server::Owned<global::OnboardingStatus>,
        _sender: xous::PID,
        context: &mut server::ServerContext<Self>,
    ) {
        if msg.is_complete() {
            context.shutdown();
        }
    }
}
