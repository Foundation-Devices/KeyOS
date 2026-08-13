// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use server::{
    permission_set, CheckedPermissions, MessageAllowed, ScalarEventHandler, Server, ServerContext,
    ServerMessages,
};
use settings::{
    global::SystemTheme,
    messages::{GetSystemTheme, SetSystemTheme, SubscribeSystemTheme},
    SettingsApi,
};
use slint::ComponentHandle;

use crate::MainWindow;

permission_set!(
    /// Permissions for reading and writing the system light/dark theme.
    pub trait ThemePermissions { GetSystemTheme, SetSystemTheme }
);

/// Set the system theme
pub fn set_system_theme<P: ThemePermissions>(api: &SettingsApi<P>, is_dark: bool) {
    let theme = if is_dark { SystemTheme::Dark } else { SystemTheme::Light };
    api.set_system_theme(theme);
}

/// Get the current system theme
pub fn get_system_theme<P: ThemePermissions>(api: &SettingsApi<P>) -> bool {
    matches!(api.get_system_theme(), SystemTheme::Dark)
}

pub fn setup<P>(window: &MainWindow, api: SettingsApi<P>)
where
    P: ThemePermissions + MessageAllowed<SubscribeSystemTheme>,
{
    window.set_is_dark_theme(get_system_theme(&api));

    let subscriber = ThemeSubscriber { window: window.as_weak(), api: api.clone() };
    std::thread::spawn(move || server::listen(subscriber));

    window.on_theme_set(move |is_dark| {
        set_system_theme(&api, is_dark);
    });
}

/// Anonymous os/settings subscriber that mirrors system theme changes into the
/// control-panel window.
struct ThemeSubscriber<P: CheckedPermissions> {
    window: slint::Weak<MainWindow>,
    api: SettingsApi<P>,
}

impl<P: CheckedPermissions> ServerMessages for ThemeSubscriber<P> {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] { &[] }
}

impl<P: CheckedPermissions + MessageAllowed<SubscribeSystemTheme>> Server for ThemeSubscriber<P> {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.api.server_subscribe_system_theme(context);
    }
}

impl<P: CheckedPermissions + MessageAllowed<SubscribeSystemTheme>> ScalarEventHandler<SystemTheme>
    for ThemeSubscriber<P>
{
    fn handle(&mut self, theme: SystemTheme, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        let window = self.window.clone();
        let is_dark = matches!(theme, SystemTheme::Dark);
        slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                window.set_is_dark_theme(is_dark);
            }
        })
        .ok();
    }
}
