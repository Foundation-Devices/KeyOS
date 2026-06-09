// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::Context;
use slint_keyos_platform::{
    app,
    gui_server_api::navigation::filepicker::{Location, SelectFileOptions},
    navigation::select_file,
    slint::{ModelRc, ToSharedString, VecModel},
    spawn_local, subscribe_archive, StoredValue,
};
use update::messages::ProgressUpdate;

update::use_api!();

app!("Update Test");

use gui_permissions::GuiPermissions;

struct AppState {
    ui: AppWindow,
    update: UpdateApi,
    files: Vec<(String, fs::Location)>,
}

impl AppState {
    fn slint_state(&self) -> State<'_> { self.ui.global::<State>() }

    fn append_files(&mut self, files: Vec<(String, fs::Location)>) {
        let all_files = vec![std::mem::take(&mut self.files), files].concat();
        self.set_files(all_files);
    }

    fn set_files(&mut self, files: Vec<(String, fs::Location)>) {
        let files_view = ModelRc::new(VecModel::from_iter(
            files.iter().map(|(path, _)| path).map(|path| path.to_shared_string()),
        ));

        self.slint_state().set_update_files(files_view);
        self.files = files;
    }
}

fn app_main(_cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state =
        StoredValue::new(AppState { update: UpdateApi::default(), ui: ui.clone_strong(), files: vec![] });

    {
        let state = state.borrow();
        let slint_state = state.slint_state();

        if state.update.update_status().needs_continue {
            slint_state.set_update_state(UpdateState::NeedsContinue);
        } else if state.update.check_update_applied() {
            slint_state.set_update_state(UpdateState::Success);
            state.update.clear_update_applied();
        }
    }

    ui.global::<State>().on_select_files(move || {
        let mut state = state.borrow_mut();
        match select_file::<GuiPermissions>(SelectFileOptions::default().with_multiple_selection_mode(true)) {
            Ok(Some(result)) => {
                if result.files.is_empty() {
                    log::info!("files is empty");
                    return;
                }

                let files = result
                    .files
                    .into_iter()
                    .map(|(path, location)| {
                        let location = match location {
                            Location::Internal => fs::Location::User,
                            Location::Airlock => fs::Location::Airlock,
                            Location::External => fs::Location::Usb,
                        };

                        (path, location)
                    })
                    .collect::<Vec<_>>();

                state.append_files(files);
            }
            Ok(None) => log::info!("no file selected"),
            Err(e) => {
                log::error!("failed to select file {e:?}");
            }
        }
    });

    ui.global::<State>().on_shift_file(move |index, increment| {
        let mut state = state.borrow_mut();
        let mut files = std::mem::take(&mut state.files);
        let index = index as usize;
        let new_index = if increment { index + 1 } else { index - 1 };
        if files.get(index).is_some() && files.get(new_index).is_some() {
            files.swap(index, new_index);
        }
        state.set_files(files);
    });

    ui.global::<State>().on_remove_file(move |index| {
        let index = index as usize;
        let mut state = state.borrow_mut();
        let mut files = std::mem::take(&mut state.files);
        if files.get(index).is_some() {
            files.remove(index);
        }
        state.set_files(files);
    });

    ui.global::<State>().on_perform_update(move || {
        perform_update(state).inspect_err(|e| log::error!("{e:?}")).ok();
    });

    ui.global::<State>().on_continue_update(move || {
        let state = state.borrow();
        state.slint_state().set_update_state(UpdateState::Loading);
        state.update.continue_update();
    });

    spawn_local(async move {
        let mut events = subscribe_archive::<update_permissions::UpdatePermissions, _>(
            update::messages::SubscribeUpdateProgress,
        );

        while let Some(update) = events.next().await {
            match update {
                ProgressUpdate::InstallProgress(progress) => {
                    let percent = progress.completion_percentage();
                    let time_remaining = progress.estimate_time_remaining_secs();

                    let time_str = if time_remaining > 60 {
                        format!("{}m {}s", time_remaining / 60, time_remaining % 60)
                    } else {
                        format!("{}s", time_remaining)
                    };

                    state.borrow().slint_state().set_update_progress_percent(percent as f32);
                    state.borrow().slint_state().set_update_estimated_time_remaining(time_str.into());
                }
                ProgressUpdate::Rebooting => {}
                ProgressUpdate::Done => {}
                ProgressUpdate::InstallError(error) => {
                    log::error!("failed to apply update {error:?}");
                    state.borrow().slint_state().set_update_state(UpdateState::Failure);
                }
                ProgressUpdate::DownloadError(error) => {
                    log::error!("failed to download update {error:?}");
                    state.borrow().slint_state().set_update_state(UpdateState::Failure);
                }
                ProgressUpdate::DownloadProgress(_) => {}
                ProgressUpdate::DownloadComplete => {}
            }
        }
    })
    .detach();

    ui.run().expect("UI running");
}

fn perform_update(state: StoredValue<AppState>) -> anyhow::Result<()> {
    if state.borrow().slint_state().get_update_state() == UpdateState::Loading {
        anyhow::bail!("update already in progress");
    }

    let mut system_files = vec![];

    let fs = FileSystem::default();

    for (path, location) in std::mem::take(&mut state.borrow_mut().files) {
        if location != fs::Location::System {
            // Copy file to system partition
            let system_path =
                format!("/keyos/tmp_update_{}", path.split('/').last().unwrap_or("release.bin"));

            let mut src = fs
                .open_file(&path, location, fs::OpenFlags { read: true, write: false, create: false })
                .context("Failed to open source file")?;

            let mut dst = fs
                .open_file(
                    &system_path,
                    fs::Location::System,
                    fs::OpenFlags { read: true, write: true, create: true },
                )
                .context("Failed to create destination file")?;

            std::io::copy(&mut src, &mut dst).context("Failed to copy file")?;

            system_files.push(system_path);
        }
    }

    let state = state.borrow();
    state.slint_state().set_update_state(UpdateState::Loading);
    state.update.start_update(system_files);

    Ok(())
}
