// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Utilities for QuantumLink pairing flow.

use std::time::Duration;

use app_manager::messages::LaunchAppBlocking;
use bt::{messages::GetBtAddr, BluetoothApi, BtAddr};
use power_manager::messages::StatusSubscribe;
use quantum_link::{
    foundation_api::status::TimezoneResponse, messages::*, QlStatus, QuantumLinkApi, SendMessageError,
};
use server::{CheckedPermissions, MessageAllowed};
use settings::{
    global::SystemTheme,
    messages::{GetDeviceName, GetPrimeColor, LookupTimeZone, SetTimeZone},
    SettingsApi,
};
use slint_keyos_platform::{
    sleep,
    slint::{ModelRc, SharedString},
    spawn_local, subscribe_scalar, try_async_scalar, TaskHandle,
};
use update::MIN_UPDATE_BATTERY_PERCENT;

/// Poll for BLE address and invoke callback once obtained (retries every second).
pub fn on_ble_address<B, F>(mut bluetooth: BluetoothApi<B>, callback: F)
where
    B: CheckedPermissions + MessageAllowed<GetBtAddr> + 'static,
    F: FnOnce(BtAddr) + 'static,
{
    spawn_local(async move {
        loop {
            match bluetooth.get_bt_addr() {
                Ok(addr) => {
                    callback(addr);
                    break;
                }
                Err(_) => {
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
    .detach();
}

/// Generate static QR code data for initial pairing.
pub fn static_qr<S>(settings: &SettingsApi<S>, ble_address: &BtAddr, onboarding_complete: bool) -> String
where
    S: CheckedPermissions + MessageAllowed<GetPrimeColor> + MessageAllowed<GetDeviceName>,
{
    let ble_address_hex = ble_address.to_hex_string();
    let colorway = match settings.get_prime_color() {
        SystemTheme::Dark => 1,
        _ => 0,
    };
    let onboarding_status: u8 = onboarding_complete.into();
    let device_name = settings.get_device_name().0;
    let device_name = urlencoding::encode(&device_name);
    format!(
        "https://qr.foundation.xyz/?p={ble_address_hex}&c={colorway}&o={onboarding_status}&n={device_name}"
    )
}

/// Generate animated QR code parts for XID document.
pub fn animated_qr<Q>(quantum_link: &QuantumLinkApi<Q>) -> ModelRc<SharedString>
where
    Q: CheckedPermissions + MessageAllowed<GetXidDocument>,
{
    let xid = quantum_link.xid_document();
    slint_keyos_platform::qrcode::encode_qr_parts("envelope", xid, 300)
}

/// Start polling connection status and call callback when BT connected state changes.
pub fn on_bt_state_change<P: 'static>(
    mut ql: QlStatus<P>,
    mut on_state_change: impl FnMut(bool) + 'static,
) -> TaskHandle<()> {
    spawn_local(async move {
        while let Some(status) = ql.next().await {
            on_state_change(status.bt_connected);
        }
    })
}

pub fn on_update_sufficient_battery<P, F>(on_status_change: F) -> TaskHandle<()>
where
    P: CheckedPermissions + MessageAllowed<StatusSubscribe> + 'static,
    F: Fn(bool) + 'static,
{
    spawn_local(async move {
        let mut events = subscribe_scalar::<P, _>(StatusSubscribe);
        while let Some(power_status) = events.next().await {
            on_status_change(power_status.battery_percent >= MIN_UPDATE_BATTERY_PERCENT)
        }
    })
}

pub fn sync_system_timezone<PQ, PS>(
    settings: SettingsApi<PS>,
    ql: QlStatus<PQ>,
    mut error: impl FnMut(SendMessageError) + Send + 'static,
) -> TaskHandle<()>
where
    PQ: CheckedPermissions + MessageAllowed<EnvoyTimezone> + 'static,
    PS: CheckedPermissions
        + MessageAllowed<SetTimeZone>
        + MessageAllowed<LookupTimeZone>
        + MessageAllowed<SetTimeZone>
        + 'static,
{
    // envoy 2.2.0 will not support this.
    // avoids worst case of infinite retry impacting onboarding.
    spawn_local(async move {
        let mut retries = 3;
        let TimezoneResponse { zone, offset_minutes } = loop {
            match ql.send_ql_archive(quantum_link::messages::EnvoyTimezone).await {
                Ok(response) => break response,
                Err(e) => {
                    error(e);
                    retries -= 1;
                    if retries == 0 {
                        return;
                    }
                }
            }
        };
        let tz = settings.lookup_timezone(zone, offset_minutes);
        settings.set_time_zone(tz);
    })
}

pub async fn launch_bitcoin_app<P>() -> Result<server::xous::PID, app_manager::AppManagerError>
where
    P: CheckedPermissions + MessageAllowed<LaunchAppBlocking> + 'static,
{
    let bitcoin_app_id = app_manager::decode_app_id_str("0x426974636f696e2057616c6c65740000").unwrap();
    try_async_scalar::<P, _>(app_manager::messages::LaunchAppBlocking(bitcoin_app_id))
        .await
        .map_err(|_| app_manager::AppManagerError::InternalError)?
}
