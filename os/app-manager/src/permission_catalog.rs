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

use app_manifest::{ApprovalBehavior, Manifest, Message, RequiredSignature};
use xous::MessageId;

/// English labels for the permission subgroups (a message's full `permissionGroup` value). The
/// permission UI groups and grants at the subgroup level, so approving one covers all of its
/// messages.
// TODO(SFT-7229): localize these labels (and the permission prompt strings in gui-server).
static SUBGROUP_LABELS: &[(&str, &str)] = &[
    ("app-management.app-metadata", "App metadata"),
    ("cryptography.primitives", "Cryptographic primitives"),
    ("cryptography.hardware-hashing", "Hardware hashing"),
    ("cryptography.secret-sharing", "Secret sharing"),
    ("device-connectivity.bluetooth-status", "Bluetooth status"),
    ("device-connectivity.nfc-status", "NFC status"),
    ("device-connectivity.usb-device-status", "USB device status"),
    ("device-connectivity.usb-host-status", "USB host status"),
    ("device-secrets.app-scoped-seed", "App-scoped seed"),
    ("device-secrets.general-status", "Device identity status"),
    ("file-system.read-handles", "Read files"),
    ("file-system.write-and-mutate", "Modify files"),
    ("file-system.user-files", "User files"),
    ("file-system.usb-files", "USB files"),
    ("file-system.airlock-files", "Airlock files"),
    ("network-and-pairing.status-and-rates", "Network status and rates"),
    ("network-and-pairing.wallet-sync", "Wallet sync"),
    ("peripherals.camera-status", "Camera status"),
    ("peripherals.camera-use", "Camera use"),
    ("peripherals.feedback", "Haptics and LED"),
    ("peripherals.sensors", "Sensors"),
    ("power-and-firmware.battery-status", "Battery status"),
    ("settings.ui-essentials", "Appearance and time"),
    ("settings.device-configuration", "Device configuration"),
    ("ui-and-input.app-surface", "Screen drawing"),
    ("ui-and-input.brokered-modal", "System dialogs"),
    ("ui-and-input.keyboard-request", "On-screen keyboard"),
    ("ui-and-input.navigation-response", "Navigation"),
];

/// English labels for the top-level permission groups (the part of a subgroup before the
/// first `.`), under which the permission UI collapses the subgroups.
// TODO(SFT-7229): localize these labels too.
static GROUP_LABELS: &[(&str, &str)] = &[
    ("app-management", "Apps and publisher trust"),
    ("cryptography", "Cryptography"),
    ("device-connectivity", "Bluetooth, NFC, and USB"),
    ("device-secrets", "Device secrets and identity"),
    ("file-system", "File system"),
    ("network-and-pairing", "Network sync and pairing"),
    ("peripherals", "Peripherals"),
    ("power-and-firmware", "Power and firmware"),
    ("settings", "Settings"),
    ("ui-and-input", "Interface and navigation"),
];

/// The permission policy a server declared for one of its messages.
#[derive(Debug, Clone)]
pub(crate) struct MessageMetadata {
    subgroup: String,
    required_signature: RequiredSignature,
    approval: ApprovalBehavior,
}

impl MessageMetadata {
    /// The policy attached to a manifest message declaration, or `None` when the message
    /// carries no permission metadata (internal messages that are never user-facing).
    fn from_message(message: &Message) -> Option<Self> {
        if message.permission_group.is_none()
            && message.required_signature.is_none()
            && message.approval == ApprovalBehavior::NotUserGrantable
        {
            return None;
        }
        Some(Self {
            subgroup: message.permission_group.clone().unwrap_or_default(),
            required_signature: message.required_signature(),
            approval: message.approval,
        })
    }

    /// The full subgroup key (e.g. `cryptography.hardware-hashing`), the unit the permission UI
    /// groups and grants by.
    pub(crate) fn subgroup(&self) -> &str { self.subgroup.as_str() }

    pub(crate) fn subgroup_label(&self) -> &str {
        let key = self.subgroup();
        SUBGROUP_LABELS
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, label)| *label)
            .unwrap_or(key)
    }

    /// The top-level group: the part of the subgroup before the first `.`.
    pub(crate) fn group(&self) -> &str { self.subgroup.split('.').next().unwrap_or_default() }

    pub(crate) fn group_label(&self) -> &str {
        let key = self.group();
        GROUP_LABELS.iter().find(|(candidate, _)| *candidate == key).map(|(_, label)| *label).unwrap_or(key)
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
