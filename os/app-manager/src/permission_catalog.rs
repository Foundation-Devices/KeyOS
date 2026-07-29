// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Permission metadata for the phase-1 permission UI/enforcement path.
//!
//! The policy itself is declared on each server's manifest message entries (the
//! `permission_group`/`required_signature`/`approval` fields of [`app_manifest::Message`]), so it
//! cannot drift from the messages it describes. This module resolves those declarations out
//! of the system manifests and owns the presentation labels of the permission subgroups.

use std::collections::HashMap;
use std::sync::OnceLock;

use app_manifest::{ApprovalBehavior, Manifest, Message, RequiredSignature, RequiredType};
use xous::MessageId;

/// Each permission subgroup (a message's full `permissionGroup` value) mapped to the [`TrId`] of its
/// display name. The permission UI groups and grants at the subgroup level, so approving one covers
/// all of its messages. The names are curated: semantic renames of the subgroup key, not a
/// mechanical transform (e.g. `device-secrets.general-status` becomes "Device identity status").
///
/// [`TrId`]: crate::tr::TrId
static SUBGROUP_LABELS: &[(&str, crate::tr::TrId)] = &[
    ("app-management.app-metadata", crate::tr::TrId::PermissionAppManagementAppMetadata),
    ("cryptography.primitives", crate::tr::TrId::PermissionCryptographyPrimitives),
    ("cryptography.hardware-hashing", crate::tr::TrId::PermissionCryptographyHardwareHashing),
    ("cryptography.secret-sharing", crate::tr::TrId::PermissionCryptographySecretSharing),
    ("device-connectivity.bluetooth-status", crate::tr::TrId::PermissionDeviceConnectivityBluetoothStatus),
    ("device-connectivity.nfc-status", crate::tr::TrId::PermissionDeviceConnectivityNfcStatus),
    ("device-connectivity.usb-device-status", crate::tr::TrId::PermissionDeviceConnectivityUsbDeviceStatus),
    ("device-connectivity.usb-host-status", crate::tr::TrId::PermissionDeviceConnectivityUsbHostStatus),
    ("device-secrets.app-scoped-seed", crate::tr::TrId::PermissionDeviceSecretsAppScopedSeed),
    ("device-secrets.general-status", crate::tr::TrId::PermissionDeviceSecretsDeviceIdentityStatus),
    ("file-system.read-handles", crate::tr::TrId::PermissionFileSystemReadFiles),
    ("file-system.write-and-mutate", crate::tr::TrId::PermissionFileSystemModifyFiles),
    ("file-system.user-files", crate::tr::TrId::PermissionFileSystemUserFiles),
    ("file-system.usb-files", crate::tr::TrId::PermissionFileSystemUsbFiles),
    ("file-system.airlock-files", crate::tr::TrId::PermissionFileSystemAirlockFiles),
    ("network-and-pairing.status-and-rates", crate::tr::TrId::PermissionNetworkAndPairingStatusAndRates),
    ("network-and-pairing.wallet-sync", crate::tr::TrId::PermissionNetworkAndPairingWalletSync),
    ("peripherals.camera-status", crate::tr::TrId::PermissionPeripheralsCameraStatus),
    ("peripherals.camera-use", crate::tr::TrId::PermissionPeripheralsCameraUse),
    ("peripherals.feedback", crate::tr::TrId::PermissionPeripheralsHapticAndLed),
    ("peripherals.sensors", crate::tr::TrId::PermissionPeripheralsSensors),
    ("power-and-firmware.battery-status", crate::tr::TrId::PermissionPowerAndFirmwareBatteryStatus),
    ("settings.ui-essentials", crate::tr::TrId::PermissionSettingsAppearanceAndTime),
    ("settings.device-configuration", crate::tr::TrId::PermissionSettingsDeviceConfiguration),
    ("ui-and-input.app-surface", crate::tr::TrId::PermissionInterfaceAndNavigationScreenDrawing),
    ("ui-and-input.brokered-modal", crate::tr::TrId::PermissionInterfaceAndNavigationSystemDialogs),
    (
        "ui-and-input.control-center-appearance",
        crate::tr::TrId::PermissionInterfaceAndNavigationControlCenterAppearance,
    ),
    ("ui-and-input.keyboard-request", crate::tr::TrId::PermissionInterfaceAndNavigationOnScreenKeyboard),
    ("ui-and-input.navigation-response", crate::tr::TrId::PermissionInterfaceAndNavigationNavigation),
];

/// Each top-level permission group (the part of a subgroup before the first `.`) mapped to the
/// [`TrId`] of its display name. The permission UI collapses subgroups under these labels.
///
/// [`TrId`]: crate::tr::TrId
static GROUP_LABELS: &[(&str, crate::tr::TrId)] = &[
    ("app-management", crate::tr::TrId::PermissionAppManagementMain),
    ("cryptography", crate::tr::TrId::PermissionCryptographyMain),
    ("device-connectivity", crate::tr::TrId::PermissionDeviceConnectivityMain),
    ("device-secrets", crate::tr::TrId::PermissionDeviceSecretsMain),
    ("file-system", crate::tr::TrId::PermissionFileSystemMain),
    ("network-and-pairing", crate::tr::TrId::PermissionNetworkAndPairingMain),
    ("peripherals", crate::tr::TrId::PermissionPeripheralsMain),
    ("power-and-firmware", crate::tr::TrId::PermissionPowerAndFirmwareMain),
    ("settings", crate::tr::TrId::PermissionSettingsMain),
    ("ui-and-input", crate::tr::TrId::PermissionInterfaceAndNavigationMain),
];

/// The permission policy a server declared for one of its messages.
#[derive(Debug, Clone)]
pub(crate) struct MessageMetadata {
    subgroup: String,
    required_signature: RequiredSignature,
    required_type: Option<RequiredType>,
    approval: ApprovalBehavior,
}

impl MessageMetadata {
    /// The policy attached to a manifest message declaration, or `None` when the message
    /// carries no permission metadata (internal messages that are never user-facing).
    fn from_message(message: &Message) -> Option<Self> {
        if message.permission_group.is_none()
            && message.required_signature.is_none()
            && message.required_type.is_none()
            && message.approval == ApprovalBehavior::NotUserGrantable
        {
            return None;
        }
        Some(Self {
            subgroup: message.permission_group.clone().unwrap_or_default(),
            required_signature: message.required_signature(),
            required_type: message.required_type,
            approval: message.approval,
        })
    }

    /// Whether this message is restricted to Flux child apps. The OS decides which apps qualify
    /// (from their install location), so an ordinary app declaring it is still refused.
    pub(crate) fn requires_flux(&self) -> bool { matches!(self.required_type, Some(RequiredType::Flux)) }

    /// The full subgroup key (e.g. `cryptography.hardware-hashing`), the unit the permission UI
    /// groups and grants by.
    pub(crate) fn subgroup(&self) -> &str { self.subgroup.as_str() }

    /// The `TrId` of this subgroup's display name, or `None` for a subgroup with no catalog entry
    /// (should not happen for a message that declares a `permissionGroup`).
    fn subgroup_tr_id(&self) -> Option<crate::tr::TrId> {
        let key = self.subgroup();
        SUBGROUP_LABELS.iter().find(|(candidate, _)| *candidate == key).map(|(_, id)| *id)
    }

    /// The subgroup's display name, localized in `locale` (unknown locales fall back to English).
    /// An unmapped subgroup falls back to its raw key.
    pub(crate) fn subgroup_label(&self, locale: &str) -> &str {
        if let Some(id) = self.subgroup_tr_id() {
            return crate::tr::lookup_id_in(locale, id);
        }
        // An unknown subgroup falls back to its raw key.
        self.subgroup()
    }

    /// The top-level group: the part of the subgroup before the first `.`.
    pub(crate) fn group(&self) -> &str { self.subgroup.split('.').next().unwrap_or_default() }

    /// The top-level group's display name, localized in `locale` (unknown locales fall back to
    /// English). An unmapped group falls back to its raw key.
    pub(crate) fn group_label(&self, locale: &str) -> &str {
        let key = self.group();
        if let Some(id) = GROUP_LABELS.iter().find(|(candidate, _)| *candidate == key).map(|(_, id)| *id) {
            return crate::tr::lookup_id_in(locale, id);
        }
        key
    }

    /// Whether an app carrying `app_signature` meets this message's signature requirement.
    /// Foundation out-ranks ThirdParty: a ThirdParty message accepts any signed app, a
    /// Foundation message accepts only a Foundation-signed one. This is independent of the
    /// approval dimension below.
    pub(crate) fn signature_satisfied_by(&self, app_signature: RequiredSignature) -> bool {
        match self.required_signature {
            RequiredSignature::ThirdParty => true,
            RequiredSignature::Foundation => app_signature == RequiredSignature::Foundation,
        }
    }

    /// Auto-granted without a prompt (once the signature requirement is met).
    pub(crate) fn is_auto_allow(&self) -> bool { self.approval == ApprovalBehavior::AutoAllow }

    /// Granted only after the user approves it (once the signature requirement is met).
    pub(crate) fn is_approval_based(&self) -> bool { self.approval.is_approval_based() }
}

/// One server's message policy and id-to-name lookup.
#[derive(Debug, Default)]
struct ServerMessages {
    metadata: HashMap<String, MessageMetadata>,
    id_to_name: HashMap<MessageId, String>,
}

/// A manifest declared a server another manifest already owns; the second declaration is refused.
#[derive(Debug)]
pub(crate) struct ServerCollision(pub String);

/// Per-server message policy and id-to-name lookup, built from the system and installed
/// manifests. Building it is also the server-name collision check.
#[derive(Debug, Default)]
pub(crate) struct ServerPermissionCache {
    servers: HashMap<String, ServerMessages>,
}

impl ServerPermissionCache {
    /// Add every server this manifest declares. Fails without mutating when the manifest
    /// declares a server already present, so the caller can reject an app that shadows a system
    /// service or an earlier app.
    pub(crate) fn add_manifest(&mut self, manifest: &Manifest) -> Result<(), ServerCollision> {
        if let Some(taken) = manifest.servers.keys().find(|server| self.servers.contains_key(*server)) {
            return Err(ServerCollision(taken.clone()));
        }
        for (server, messages) in &manifest.servers {
            let entry = self.servers.entry(server.clone()).or_default();
            for (name, message) in messages {
                entry.id_to_name.insert(message.id, name.clone());
                if let Some(meta) = MessageMetadata::from_message(message) {
                    entry.metadata.insert(name.clone(), meta);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn message_metadata(&self, server: &str, message: &str) -> Option<&MessageMetadata> {
        self.servers.get(server)?.metadata.get(message)
    }

    pub(crate) fn message_name_by_id(&self, server: &str, id: MessageId) -> Option<&str> {
        self.servers.get(server)?.id_to_name.get(&id).map(String::as_str)
    }
}

pub(crate) fn system_manifests() -> &'static [Manifest] {
    #[cfg(test)]
    if let Some(manifests) = test_override::get() {
        return manifests;
    }
    static CACHE: OnceLock<Vec<Manifest>> = OnceLock::new();
    CACHE.get_or_init(|| {
        system_manifests::SYSTEM_MANIFESTS
            .iter()
            .map(|manifest_json| {
                // Built-in system manifests are generated by xtask; a parse failure is a build
                // bug, so fail loudly at startup rather than silently dropping one.
                app_manifest::try_from_bytes(manifest_json.as_bytes())
                    .unwrap_or_else(|e| panic!("built-in system manifest failed to parse: {e:?}"))
            })
            .collect()
    })
}

/// Lets a unit test stand in a set of system manifests, since the xtask-generated
/// `SYSTEM_MANIFESTS` is empty under plain `cargo test`. Per-test via a thread-local.
#[cfg(test)]
pub(crate) mod test_override {
    use std::cell::RefCell;

    use super::Manifest;

    thread_local!(static OVERRIDE: RefCell<Option<&'static [Manifest]>> = const { RefCell::new(None) });

    pub(crate) fn get() -> Option<&'static [Manifest]> { OVERRIDE.with(|slot| *slot.borrow()) }
}
