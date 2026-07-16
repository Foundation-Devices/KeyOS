// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;

use app_manager::AppQrMatchRules;
use app_manifest::Manifest;
use serde_json::to_vec;
use xous::{AppId, PID};

use crate::launch::list_apps;

#[derive(Debug, Clone)]
pub(crate) struct AppInfo {
    id: AppId,
    elf_path: Option<String>,
    manifest: Manifest,
}

#[derive(Debug, Clone)]
pub(crate) struct RunningAppInfo {
    pub(crate) info: AppInfo,
    pub(crate) launched_by: PID,
}

#[derive(Debug, Default)]
pub(crate) struct AppRegistry {
    installed_apps: HashMap<AppId, AppInfo>,
    running_apps: HashMap<PID, RunningAppInfo>,
}

impl AppRegistry {
    pub(crate) fn scan_installed_apps(&mut self) -> anyhow::Result<()> {
        match list_apps("/keyos/apps") {
            Ok(apps_list) => {
                for (path, manifest) in apps_list {
                    let app_id = AppId(manifest.app_id);

                    if self.installed_apps.contains_key(&app_id) {
                        log::warn!(
                            "scan_installed_apps: skipping duplicate app_id=0x{}",
                            hex::encode(app_id.0)
                        );
                        continue;
                    }

                    #[cfg(not(keyos))]
                    let elf_path = path.map(|p| p.to_string_lossy().to_string());
                    #[cfg(keyos)]
                    let elf_path = path.map(|s| s.to_string());

                    self.installed_apps.insert(app_id, AppInfo { id: app_id, elf_path, manifest });
                }
                log::info!(
                    "scan_installed_apps: registry tracks {} installed apps",
                    self.installed_apps.len()
                );
            }

            Err(e) => {
                log::error!("Error listing apps: {:?}", e);
            }
        }

        Ok(())
    }

    pub(crate) fn app_name_by_id(&self, id: &AppId, locale: &str) -> Option<String> {
        self.installed_apps
            .get(id)
            .and_then(|app_info| app_info.manifest.app_name.get(&locale.to_string().into()).cloned())
    }

    pub(crate) fn app_name_by_pid(&self, pid: PID, locale: &str) -> Option<String> {
        self.running_apps
            .get(&pid)
            .and_then(|app_info| app_info.info.manifest.app_name.get(&locale.to_string().into()).cloned())
    }

    pub(crate) fn qr_match_rules(&self) -> Vec<AppQrMatchRules> {
        self.installed_apps
            .values()
            .filter(|app_info| !app_info.manifest.qr_match_rules.is_empty())
            .filter_map(|app_info| match to_vec(&app_info.manifest.qr_match_rules) {
                Ok(rules_json) if !rules_json.is_empty() => {
                    Some(AppQrMatchRules { id: (&app_info.id).into(), rules_json })
                }
                Ok(_) => None,
                Err(_) => {
                    log::warn!(
                        "qr_match_rules: failed to serialize qr_match_rules for app_id=0x{}",
                        hex::encode(app_info.id.0)
                    );
                    None
                }
            })
            .collect()
    }

    pub(crate) fn elf_path(&self, app_id: AppId) -> Option<String> {
        self.installed_apps.get(&app_id).and_then(|app_info| app_info.elf_path.clone())
    }

    pub(crate) fn register_running_app(&mut self, pid: PID, app_id: AppId, launched_by: PID) {
        self.installed_apps.get(&app_id).inspect(|app_info| {
            self.running_apps.insert(pid, RunningAppInfo { info: (*app_info).clone(), launched_by });
        });
    }

    pub(crate) fn app_id_by_pid(&self, pid: PID) -> Option<&AppId> {
        self.running_apps.get(&pid).map(|app_info| &app_info.info.id)
    }

    pub(crate) fn launched_by(&self, app_id: &AppId) -> Option<PID> {
        self.running_apps
            .values()
            .find(|app_info| app_info.info.id == *app_id)
            .map(|app_info| app_info.launched_by)
    }

    pub(crate) fn terminate_app(&mut self, pid: PID) { self.running_apps.remove(&pid); }
}
