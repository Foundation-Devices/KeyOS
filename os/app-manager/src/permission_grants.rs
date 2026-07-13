// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use app_manager::SetAppPermissionGrantResult;
use file_backed::JsonBacked;
use fs::Location;
use serde::{Deserialize, Serialize};
use xous::{AppId, MessageId};

use crate::permission_catalog::{MessageMetadata, ServerPermissionCache};

const PERMISSION_GRANTS_PATH: &str = "permission_grants.json";

type PermissionGrantsFile = JsonBacked<StoredPermissionGrants, crate::fs_permissions::FileSystemPermissions>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionGrantState {
    Unset,
    Approved,
    Denied,
}

/// Grants keyed by app id, then by permission subgroup. Storing the subgroup rather than the
/// individual messages means a server can add, rename, or move messages within a subgroup and
/// existing grants follow without a data migration.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoredPermissionGrants {
    grants: BTreeMap<String, BTreeMap<String, bool>>,
}

#[derive(Debug, Default)]
pub(crate) struct PermissionGrantStore {
    grants: Option<PermissionGrantsFile>,
    /// Per-server message policy and id-to-name lookup, rebuilt from all manifests on each scan.
    cache: ServerPermissionCache,
}

impl PermissionGrantStore {
    /// Replace the cached per-server message policy; called after each app scan with the cache
    /// built from the system and installed manifests.
    pub(crate) fn set_server_cache(&mut self, cache: ServerPermissionCache) { self.cache = cache; }

    /// The declared policy for `message` on `server`, or `None` for an unknown or non-user-facing
    /// message.
    pub(crate) fn message_metadata(&self, server: &str, message: &str) -> Option<&MessageMetadata> {
        self.cache.message_metadata(server, message)
    }

    /// The message name declared as `id` on `server`, resolving the kernel's numeric id to a name.
    pub(crate) fn message_name_by_id(&self, server: &str, id: MessageId) -> Option<&str> {
        self.cache.message_name_by_id(server, id)
    }

    pub(crate) fn try_mount_app_data(&mut self) {
        if self.grants.is_some() {
            return;
        }

        let (grants, _restored) = PermissionGrantsFile::new(PERMISSION_GRANTS_PATH, Location::AppData);
        self.grants = Some(grants);
    }

    /// Whether `message` on `server` is approved for `app_id`: the message resolves to its
    /// subgroup and the subgroup's stored grant decides.
    pub(crate) fn is_approved(&self, app_id: AppId, server: &str, message: &str) -> bool {
        let Some(entry) = self.message_metadata(server, message) else {
            return false;
        };
        self.subgroup_grant_state(app_id, entry.subgroup()) == PermissionGrantState::Approved
    }

    pub(crate) fn subgroup_grant_state(&self, app_id: AppId, subgroup: &str) -> PermissionGrantState {
        let Some(grants) = &self.grants else {
            return PermissionGrantState::Unset;
        };
        grants
            .grants
            .get(&app_id_key(app_id))
            .and_then(|subgroups| subgroups.get(subgroup))
            .map(
                |approved| {
                    if *approved {
                        PermissionGrantState::Approved
                    } else {
                        PermissionGrantState::Denied
                    }
                },
            )
            .unwrap_or(PermissionGrantState::Unset)
    }

    pub(crate) fn set_grant(
        &mut self,
        app_id: AppId,
        subgroup: &str,
        approved: bool,
    ) -> SetAppPermissionGrantResult {
        let Some(grants) = &mut self.grants else {
            return SetAppPermissionGrantResult::StorageUnavailable;
        };

        grants.guard().grants.entry(app_id_key(app_id)).or_default().insert(subgroup.to_string(), approved);
        SetAppPermissionGrantResult::Updated
    }

    /// Remove every stored grant for `app_id`. Returns false only when the store is
    /// unavailable, so callers can abort the removal instead of leaving grants behind that
    /// would re-arm on a future sideload of the same app id.
    pub(crate) fn remove_app_grants(&mut self, app_id: AppId) -> bool {
        let Some(grants) = &mut self.grants else {
            return false;
        };
        grants.guard().grants.remove(&app_id_key(app_id));
        true
    }
}

fn app_id_key(app_id: AppId) -> String { format!("0x{app_id}") }
