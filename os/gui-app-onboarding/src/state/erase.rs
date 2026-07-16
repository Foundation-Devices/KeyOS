// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use slint_keyos_platform::{
    async_archive, sleep,
    slint::{ComponentHandle, ModelRc, VecModel},
    StoredValue,
};

use crate::state::AppState;
use crate::{
    security_permissions::SecurityPermissions, EraseGlobal, Navigate, NavigateOptions, OnboardingCallbacks,
    PowerManagerApi, StepModel,
};

const REBOOT_DELAY: Duration = Duration::from_secs(3);

pub(crate) fn init_erase_callbacks(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();

    ui.global::<EraseGlobal>().set_progress(0.);
    ui.global::<OnboardingCallbacks>().on_erase_device(move || {
        let ui = state.borrow().ui();
        let navigate = ui.global::<Navigate>();
        navigate.invoke_master_key_deleted_erasing(NavigateOptions { replace: true, ..Default::default() });

        let Some(step_list) = make_step_list(EraseProgressKind::Erasing, true, false) else { return };
        ui.global::<EraseGlobal>().set_erasing_steps(step_list);

        slint_keyos_platform::spawn_local(async move {
            log::info!("Erasing everything");
            let ui = state.borrow().ui();
            let global = ui.global::<EraseGlobal>();

            let lockout_result = futures_lite::future::or(
                async {
                    crate::erase_system_state();
                    async_archive::<SecurityPermissions, _>(security::messages::Lockout {
                        lockout_options: security::LockoutOptions::erase_all(),
                        reboot: false,
                    })
                    .await
                },
                async {
                    // Since we don't get actual progress from Security,
                    // fake a progress for around 3 seconds
                    let mut progress = 0.0;
                    loop {
                        global.set_progress(progress);
                        sleep(Duration::from_millis(100)).await;
                        progress = (progress + 0.03).min(0.95);
                    }
                },
            )
            .await;

            if let Err(e) = lockout_result {
                log::error!("Lockout error: {e:?}");

                if let Some(list) = make_step_list(EraseProgressKind::Erasing, false, true) {
                    global.set_progress(1.);
                    global.set_erasing_steps(list);
                }

                return;
            }

            global.set_progress(1.);
            if let Some(list) = make_step_list(EraseProgressKind::Rebooting, true, false) {
                global.set_erasing_steps(list);
            }

            sleep(REBOOT_DELAY).await;

            log::info!("Rebooting");
            let power_manager_api = PowerManagerApi::default();
            power_manager_api.reboot();
        })
        .detach();
    });
}

#[derive(Debug, Copy, Clone)]
enum EraseProgressKind {
    Erasing = 0,
    Rebooting,
}

struct Step {
    label: String,
    completed_label: Option<String>,
    error: bool,
    in_progress: bool,
    completed: bool,
}

impl Step {
    fn new(
        label: String,
        completed_label: Option<String>,
        error: bool,
        in_progress: bool,
        completed: bool,
    ) -> Self {
        Self { label, completed_label, error, in_progress, completed }
    }
}

impl From<&Step> for StepModel {
    fn from(value: &Step) -> Self {
        StepModel {
            label: if !value.completed {
                value.label.clone().into()
            } else {
                value.completed_label.clone().unwrap_or_else(|| value.label.clone()).into()
            },
            error: value.error,
            completed: value.completed,
            in_progress: value.in_progress,
            icon: "arrow-right".into(),
        }
    }
}

static LAST_STEP_HASH: AtomicU32 = AtomicU32::new(0);

fn make_step_list(
    progress_kind: EraseProgressKind,
    in_progress: bool,
    error: bool,
) -> Option<ModelRc<StepModel>> {
    use crate::tr::lookup;

    let hash = progress_hash(progress_kind, in_progress, error);
    if LAST_STEP_HASH.load(Ordering::Relaxed) == hash {
        return None; // No change, no need to update
    }

    LAST_STEP_HASH.store(hash, Ordering::Relaxed);

    let mut steps = [
        Step::new(
            lookup("masterKeyDeletedErasing.erasingPassport"),
            Some(lookup("masterKeyDeletedErasing.passportErased")),
            false,
            false,
            false,
        ),
        Step::new(lookup("masterKeyDeletedErasingSuccess.restarting"), None, false, false, false),
    ];

    let step_num = progress_kind as usize;

    // Mark all steps up to the current step as completed
    for step in steps.iter_mut().take(step_num) {
        step.in_progress = false;
        step.completed = true;
    }

    // Mark the current step accordingly
    steps[step_num].in_progress = in_progress;
    steps[step_num].completed = !in_progress && !error;
    steps[step_num].error = error;

    // Mark the future steps as not started
    for step in steps.iter_mut().skip(step_num + 1) {
        step.in_progress = false;
        step.completed = false;
        step.error = false;
    }

    Some(ModelRc::new(VecModel::from(steps.iter().map(Into::into).collect::<Vec<_>>())))
}

fn progress_hash(progress_kind: EraseProgressKind, in_progress: bool, error: bool) -> u32 {
    u32::from_le_bytes([progress_kind as u8, in_progress as u8, error as u8, 1])
}
