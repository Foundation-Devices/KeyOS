// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Sim command - build an app for hosted execution and run it in the KeyOS simulator

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use foundation_core::{AppConfig, AppManifest, ProjectContext, SdkLayout, SdkRoot};

use crate::assets::{copy_app_resources_to_bundle, stage_simulator_resources, APP_RESOURCES_DIR_ENV};
use crate::cargo_support::{configure_host_build_environment, emit_cargo_messages, emit_stderr_if_present};
use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};

const SCREENSHOTS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_SCREENSHOTS_DIR";
const SIMULATOR_APPS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_APPS_DIR";

/// Execute the sim command
pub fn execute() -> Result<()> {
    println!("Building application for simulator...");
    println!("Note: Building for hosted/simulator mode (x86_64/aarch64 native)");
    println!();

    let sdk = SdkRoot::discover().map_err(|_| anyhow::anyhow!("Could not locate the Foundation SDK root. Run this command from the SDK checkout or unpacked SDK bundle."))?;

    // Find and read app-config.toml
    let project = ProjectContext::discover().context("app-config.toml not found")?;
    let project_root = project.root.as_path();
    let config = &project.config;

    // Mirror hardware builds and viewer flows: make sure shared @ui sources plus any
    // build.rs-generated router/translation files are ready before the hosted build.
    prepare_project_for_build(project_root, &sdk)?;

    // Ensure the app's theme Rust is generated and current (foundation_themes::include_theme!).
    let themes_rust_dir = crate::commands::themes::ensure_project_theme(&sdk, config, project_root)?;

    // Build the app for native/hosted execution (not the ARM hardware target).
    build_for_simulator(project_root, config, sdk.root(), &themes_rust_dir)?;

    // Copy to SDK apps directory
    println!("Copying application to KeyOS SDK...");
    let dest_dir = copy_to_sdk(config, project_root, &sdk.simulator_apps_dir())?;
    let resources_dir = stage_simulator_resources(config, project_root)?;
    copy_app_resources_to_bundle(&resources_dir, &dest_dir)
        .with_context(|| format!("Failed to copy simulator resources to {}", dest_dir.display()))?;

    println!("Starting KeyOS simulator...");
    launch_simulator(&sdk, &dest_dir, config.app_id.as_hex(), project_root, &resources_dir)?;

    println!();
    println!("Application deployed and simulator started.");
    println!("Application deployed to: {}", dest_dir.display());

    Ok(())
}

/// Build the app for simulator using the native host toolchain.
fn build_for_simulator(
    project_root: &Path,
    config: &AppConfig,
    sdk_root: &Path,
    themes_rust_dir: &Path,
) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root);
    cmd.arg("build");
    cmd.arg("--package").arg(&config.app_name);
    cmd.arg("--message-format").arg("json-render-diagnostics");
    cmd.env("FOUNDATION_THEMES_RUST_DIR", themes_rust_dir);
    cmd.env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(project_root));
    // `@theme` namespace → per-app generated component themes (button_theme.slint).
    cmd.env("FOUNDATION_THEMES_SLINT_DIR", crate::commands::themes::project_theme_slint_dir(project_root));
    configure_host_build_environment(&mut cmd);

    let output = cmd.output().context("Failed to run cargo build")?;
    emit_cargo_messages(project_root, sdk_root, &output.stdout);
    emit_stderr_if_present(project_root, sdk_root, &output.stderr);

    if !output.status.success() {
        anyhow::bail!("Cargo build failed");
    }

    Ok(())
}

/// Copy the built app to the SDK simulator app directory.
fn copy_to_sdk(config: &AppConfig, project_root: &Path, apps_dir: &Path) -> Result<PathBuf> {
    // Determine source paths
    let profile = "debug"; // Simulator always uses debug for faster iteration
    let binary_path = project_root.join("target").join(profile).join(&config.app_name);

    if !binary_path.exists() {
        anyhow::bail!("Built binary not found at: {}", binary_path.display());
    }

    // Destination directory in SDK
    let dest_dir = apps_dir.join(&config.app_name);
    fs::create_dir_all(&dest_dir)?;

    // Copy binary as app.elf
    let dest_binary = dest_dir.join("app.elf");
    fs::copy(&binary_path, &dest_binary)
        .with_context(|| format!("Failed to copy binary to {}", dest_binary.display()))?;

    // Generate and write manifest.json
    let manifest_json = generate_manifest_json(config, project_root)?;
    let dest_manifest = dest_dir.join("manifest.json");
    fs::write(&dest_manifest, manifest_json)?;

    Ok(dest_dir)
}

fn launch_simulator(
    sdk: &SdkRoot,
    staged_dir: &Path,
    app_id_hex: &str,
    project_root: &Path,
    resources_dir: &Path,
) -> Result<()> {
    let screenshots_dir = simulator_screenshots_dir(sdk, project_root);

    if let Some(path) = sdk.bundled_binary("foundation-simulator") {
        return run_simulator_command(
            Command::new(&path),
            format!("bundled simulator at {}", path.display()),
            sdk.root(),
            staged_dir,
            app_id_hex,
            screenshots_dir.as_deref(),
            resources_dir,
        );
    }

    // In repo layout, prefer `just sim` which builds the simulator from source and uses
    // the same target/apps/ directory where we staged the app. The bundled
    // foundation-simulator on PATH uses its own directory layout that won't find apps
    // staged for repo development.
    if matches!(sdk.layout(), SdkLayout::Repo) && find_in_path("just").is_some() {
        let mut command = Command::new("just");
        command.arg("sim");
        return run_simulator_command(
            command,
            format!("`just sim` from {}", sdk.keyos_root().display()),
            &sdk.keyos_root(),
            staged_dir,
            app_id_hex,
            None,
            resources_dir,
        );
    }

    if let Some(path) = find_in_path("foundation-simulator") {
        return run_simulator_command(
            Command::new(&path),
            format!("simulator on PATH at {}", path.display()),
            sdk.root(),
            staged_dir,
            app_id_hex,
            screenshots_dir.as_deref(),
            resources_dir,
        );
    }

    anyhow::bail!("{}. The app is staged at {}", "Failed to start simulator", staged_dir.display());
}

/// Owns a spawned simulator [`Child`] and kills+reaps it on drop unless
/// explicitly released with [`into_inner`](ChildGuard::into_inner). Lets the
/// error paths in `run_simulator_command` clean up the process instead of
/// detaching it (the default `Child` drop behaviour).
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self { Self(Some(child)) }

    fn child(&mut self) -> &mut Child { self.0.as_mut().expect("simulator child accessed after release") }

    /// Release the child without killing it (the caller now owns its lifetime).
    fn into_inner(mut self) -> Child { self.0.take().expect("simulator child released twice") }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn run_simulator_command(
    mut command: Command,
    description: String,
    current_dir: &Path,
    staged_dir: &Path,
    app_id_hex: &str,
    screenshots_dir: Option<&Path>,
    resources_dir: &Path,
) -> Result<()> {
    command.current_dir(current_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(screenshots_dir) = screenshots_dir {
        command.env(SCREENSHOTS_DIR_ENV, screenshots_dir);
    }
    command.env(APP_RESOURCES_DIR_ENV, resources_dir);
    if let Some(apps_dir) = staged_dir.parent() {
        command.env(SIMULATOR_APPS_DIR_ENV, apps_dir);
    }
    let child =
        command.spawn().with_context(|| format!("Failed to start the simulator using {}", description))?;

    // std::process::Child only *detaches* on drop — it does not kill the
    // process. Without this guard, any early return below (failed pipe capture,
    // control-channel timeout, app-not-found / launch-failed while
    // foregrounding) would leave the simulator / `just sim` process running in
    // the background after `foundation sim` exits with an error. The guard
    // kills and reaps the child on drop; we disarm it via into_inner() only
    // once foregrounding has succeeded and we're waiting for a normal exit.
    let mut guard = ChildGuard::new(child);

    let stdout =
        guard.child().stdout.take().context("Failed to capture simulator stdout for control channel")?;
    let stderr =
        guard.child().stderr.take().context("Failed to capture simulator stderr for log streaming")?;
    let mut stdin =
        guard.child().stdin.take().context("Failed to open simulator stdin for control channel")?;

    let (control_tx, control_rx) = mpsc::channel();
    stream_simulator_stdout(stdout, control_tx);
    stream_simulator_stderr(stderr);

    wait_for_simulator_ready(guard.child(), &mut stdin, &control_rx, &description, staged_dir)?;
    request_app_foreground(guard.child(), &mut stdin, &control_rx, app_id_hex, &description, staged_dir)?;

    // Foregrounding succeeded; from here we wait for the user to exit the
    // simulator normally rather than killing it on drop.
    let mut child = guard.into_inner();
    let status = child
        .wait()
        .with_context(|| format!("Failed while waiting for the simulator using {}", description))?;

    if !status.success() {
        anyhow::bail!(
            "{} using {}. The app is staged at {}",
            "Failed to start simulator",
            description,
            staged_dir.display()
        );
    }

    Ok(())
}

fn stream_simulator_stdout(
    stdout: impl std::io::Read + Send + 'static,
    control_tx: mpsc::Sender<ControlEvent>,
) {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if let Some(event) = parse_control_line(&line) {
                        let _ = control_tx.send(event);
                    } else {
                        println!("{line}");
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn stream_simulator_stderr(stderr: impl std::io::Read + Send + 'static) {
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => eprintln!("{line}"),
                Err(_) => break,
            }
        }
    });
}

fn wait_for_simulator_ready(
    child: &mut Child,
    stdin: &mut impl Write,
    control_rx: &mpsc::Receiver<ControlEvent>,
    description: &str,
    staged_dir: &Path,
) -> Result<()> {
    // The simulator may need to compile the full OS before starting (e.g. `just sim`),
    // so allow a generous timeout for the build + boot sequence.
    let deadline = Instant::now() + Duration::from_secs(300);

    loop {
        ensure_simulator_still_running(child, description, staged_dir)?;
        send_control_command(stdin, "ping")?;

        if matches!(
            wait_for_control_event(child, control_rx, Duration::from_secs(1))?,
            Some(ControlEvent::PingOk)
        ) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "{} using {}. The simulator control channel never became ready. The app is staged at {}",
                "Failed to start simulator",
                description,
                staged_dir.display()
            );
        }
    }
}

fn request_app_foreground(
    child: &mut Child,
    stdin: &mut impl Write,
    control_rx: &mpsc::Receiver<ControlEvent>,
    app_id_hex: &str,
    description: &str,
    staged_dir: &Path,
) -> Result<()> {
    let mut unlock_prompted = false;
    let command = format!("run {app_id_hex}");
    drain_pending_control_events(control_rx);

    loop {
        ensure_simulator_still_running(child, description, staged_dir)?;
        send_control_command(stdin, &command)?;

        match wait_for_control_event(child, control_rx, Duration::from_secs(1))? {
            Some(ControlEvent::RunLaunched { .. } | ControlEvent::RunAlreadyRunning { .. }) => {
                return Ok(());
            }
            Some(ControlEvent::RunLocked) => {
                if !unlock_prompted {
                    println!("Unlock Passport to continue...");
                    unlock_prompted = true;
                }
                thread::sleep(Duration::from_secs(1));
            }
            Some(ControlEvent::RunNotReady) | Some(ControlEvent::PingOk) | None => {
                thread::sleep(Duration::from_secs(1));
            }
            Some(ControlEvent::RunAppNotFound) => {
                anyhow::bail!(
                    "{}. The simulator could not find app ID {}. The app is staged at {}",
                    "Failed to start simulator",
                    app_id_hex,
                    staged_dir.display()
                );
            }
            Some(ControlEvent::RunLaunchFailed) => {
                anyhow::bail!(
                    "{}. The simulator failed to launch app ID {}. The app is staged at {}",
                    "Failed to start simulator",
                    app_id_hex,
                    staged_dir.display()
                );
            }
        }
    }
}

fn drain_pending_control_events(control_rx: &mpsc::Receiver<ControlEvent>) {
    while control_rx.try_recv().is_ok() {}
}

fn ensure_simulator_still_running(child: &mut Child, description: &str, staged_dir: &Path) -> Result<()> {
    if let Some(status) = child.try_wait()? {
        anyhow::bail!(
            "{} using {} (exited with {}). The app is staged at {}",
            "Failed to start simulator",
            description,
            status,
            staged_dir.display()
        );
    }

    Ok(())
}

fn send_control_command(stdin: &mut impl Write, command: &str) -> Result<()> {
    stdin.write_all(command.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn wait_for_control_event(
    child: &mut Child,
    control_rx: &mpsc::Receiver<ControlEvent>,
    timeout: Duration,
) -> Result<Option<ControlEvent>> {
    let deadline = Instant::now() + timeout;

    loop {
        match control_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => return Ok(Some(event)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() || Instant::now() >= deadline {
                    return Ok(None);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlEvent {
    PingOk,
    RunLaunched { pid: usize },
    RunAlreadyRunning { pid: usize },
    RunLocked,
    RunNotReady,
    RunAppNotFound,
    RunLaunchFailed,
}

fn parse_control_line(line: &str) -> Option<ControlEvent> {
    let line = line.trim();
    if line == "ok ping proto=1 caps=run" {
        return Some(ControlEvent::PingOk);
    }
    if let Some(pid) = line.strip_prefix("ok run launched pid=").and_then(|pid| pid.parse().ok()) {
        return Some(ControlEvent::RunLaunched { pid });
    }
    if let Some(pid) = line.strip_prefix("ok run already-running pid=").and_then(|pid| pid.parse().ok()) {
        return Some(ControlEvent::RunAlreadyRunning { pid });
    }
    if line == "err run launch-failed" || line.starts_with("err run launch-failed ") {
        return Some(ControlEvent::RunLaunchFailed);
    }
    match line {
        "err run locked" => Some(ControlEvent::RunLocked),
        "err run not-ready" => Some(ControlEvent::RunNotReady),
        "err run app-not-found" => Some(ControlEvent::RunAppNotFound),
        _ => None,
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        candidate.is_file().then_some(candidate)
    })
}

fn simulator_screenshots_dir(sdk: &SdkRoot, project_root: &Path) -> Option<PathBuf> {
    match sdk.layout() {
        SdkLayout::Bundle => Some(project_root.join("screenshots")),
        SdkLayout::Repo => None,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use foundation_core::SdkRoot;

    use super::{
        drain_pending_control_events, launch_simulator, parse_control_line, simulator_screenshots_dir,
        ControlEvent, SCREENSHOTS_DIR_ENV, SIMULATOR_APPS_DIR_ENV,
    };
    use crate::assets::APP_RESOURCES_DIR_ENV;
    use crate::test_support::PROCESS_LOCK;

    #[test]
    fn launches_bundled_simulator_from_bundle_layout() {
        let _guard = PROCESS_LOCK.lock().unwrap();
        let sdk_root = make_bundle_sdk_root("bundle-launch");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let staged_dir = sdk_root.join("target").join("apps").join("demo");
        fs::create_dir_all(&staged_dir).unwrap();

        let simulator = sdk_root.join("bin").join("foundation-simulator");
        fs::write(
            &simulator,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" > \"{}\"\nprintf '%s\\n' \"${{{}:-}}\" > \"{}\"\nprintf '%s\\n' \"${{{}:-}}\" > \"{}\"\nprintf '%s\\n' \"${{{}:-}}\" > \"{}\"\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"{}\"\n  case \"$line\" in\n    ping)\n      printf 'ok ping proto=1 caps=run\\n'\n      ;;\n    run\\ *)\n      printf 'ok run launched pid=42\\n'\n      exit 0\n      ;;\n  esac\ndone\n",
                sdk_root.join("simulator.log").display(),
                SCREENSHOTS_DIR_ENV,
                sdk_root.join("simulator-screenshots-dir.log").display(),
                APP_RESOURCES_DIR_ENV,
                sdk_root.join("simulator-resources-dir.log").display(),
                SIMULATOR_APPS_DIR_ENV,
                sdk_root.join("simulator-apps-dir.log").display(),
                sdk_root.join("simulator-stdin.log").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&simulator).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&simulator, perms).unwrap();
        }

        let resources_dir =
            sdk_root.join("example-app").join("target").join("foundation").join("sim-resources");
        launch_simulator(&sdk, &staged_dir, "0x00112233", &sdk_root.join("example-app"), &resources_dir)
            .unwrap();

        assert_eq!(
            fs::canonicalize(fs::read_to_string(sdk_root.join("simulator.log")).unwrap().trim()).unwrap(),
            fs::canonicalize(&sdk_root).unwrap()
        );
        assert_eq!(
            PathBuf::from(fs::read_to_string(sdk_root.join("simulator-screenshots-dir.log")).unwrap().trim()),
            sdk_root.join("example-app").join("screenshots")
        );
        assert_eq!(
            PathBuf::from(fs::read_to_string(sdk_root.join("simulator-resources-dir.log")).unwrap().trim()),
            resources_dir
        );
        assert_eq!(
            PathBuf::from(fs::read_to_string(sdk_root.join("simulator-apps-dir.log")).unwrap().trim()),
            staged_dir.parent().unwrap()
        );
        assert!(fs::read_to_string(sdk_root.join("simulator-stdin.log")).unwrap().contains("run 0x00112233"));

        cleanup(&sdk_root);
    }

    #[test]
    fn falls_back_to_just_sim_for_repo_layout() {
        let _guard = PROCESS_LOCK.lock().unwrap();
        let _path_guard = PathGuard::capture();
        let sdk_root = make_repo_sdk_root("repo-launch");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let staged_dir = sdk.simulator_apps_dir().join("demo");
        let fake_bin = sdk_root.join("fake-bin");
        fs::create_dir_all(&staged_dir).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();

        let just = fake_bin.join("just");
        fs::write(
            &just,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"sim\" ]; then printf '%s\\n' \"$PWD\" > \"{}\"; printf '%s\\n' \"${{{}:-}}\" > \"{}\"; printf '%s\\n' \"${{{}:-}}\" > \"{}\"; printf '%s\\n' \"${{{}:-}}\" > \"{}\"; while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"{}\"; case \"$line\" in ping) printf 'ok ping proto=1 caps=run\\n' ;; run\\ *) printf 'ok run launched pid=7\\n'; exit 0 ;; esac; done; fi\nexit 1\n",
                sdk_root.join("just-sim.log").display(),
                SCREENSHOTS_DIR_ENV,
                sdk_root.join("just-sim-screenshots-dir.log").display(),
                APP_RESOURCES_DIR_ENV,
                sdk_root.join("just-sim-resources-dir.log").display(),
                SIMULATOR_APPS_DIR_ENV,
                sdk_root.join("just-sim-apps-dir.log").display(),
                sdk_root.join("just-sim-stdin.log").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&just).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&just, perms).unwrap();
        }

        let path = std::env::join_paths([fake_bin.clone()]).unwrap();
        std::env::set_var("PATH", path);

        let resources_dir =
            sdk_root.join("example-app").join("target").join("foundation").join("sim-resources");
        launch_simulator(&sdk, &staged_dir, "0xaabbccdd", &sdk_root.join("example-app"), &resources_dir)
            .unwrap();

        assert_eq!(
            fs::canonicalize(fs::read_to_string(sdk_root.join("just-sim.log")).unwrap().trim()).unwrap(),
            fs::canonicalize(sdk.keyos_root()).unwrap()
        );
        assert!(fs::read_to_string(sdk_root.join("just-sim-screenshots-dir.log")).unwrap().trim().is_empty());
        assert_eq!(
            PathBuf::from(fs::read_to_string(sdk_root.join("just-sim-resources-dir.log")).unwrap().trim()),
            resources_dir
        );
        assert_eq!(
            PathBuf::from(fs::read_to_string(sdk_root.join("just-sim-apps-dir.log")).unwrap().trim()),
            staged_dir.parent().unwrap()
        );
        assert!(fs::read_to_string(sdk_root.join("just-sim-stdin.log")).unwrap().contains("run 0xaabbccdd"));

        cleanup(sdk_root.parent().unwrap());
    }

    #[test]
    fn parses_control_responses() {
        assert_eq!(parse_control_line("ok ping proto=1 caps=run"), Some(ControlEvent::PingOk));
        assert_eq!(parse_control_line("ok run launched pid=42"), Some(ControlEvent::RunLaunched { pid: 42 }));
        assert_eq!(
            parse_control_line("ok run already-running pid=7"),
            Some(ControlEvent::RunAlreadyRunning { pid: 7 })
        );
        assert_eq!(parse_control_line("err run locked"), Some(ControlEvent::RunLocked));
        assert_eq!(parse_control_line("err run launch-failed"), Some(ControlEvent::RunLaunchFailed));
        assert_eq!(
            parse_control_line("err run launch-failed reason=PermissionDenied"),
            Some(ControlEvent::RunLaunchFailed)
        );
        assert_eq!(parse_control_line("noise"), None);
    }

    #[test]
    fn drains_stale_control_events_before_run_loop() {
        let (tx, rx) = mpsc::channel();
        tx.send(ControlEvent::PingOk).unwrap();
        tx.send(ControlEvent::PingOk).unwrap();

        drain_pending_control_events(&rx);

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn uses_app_local_screenshots_for_bundle_layout() {
        let sdk_root = make_bundle_sdk_root("bundle-screenshots");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root = sdk_root.join("demo-app");

        assert_eq!(simulator_screenshots_dir(&sdk, &project_root), Some(project_root.join("screenshots")));

        cleanup(&sdk_root);
    }

    #[test]
    fn keeps_existing_screenshots_location_for_repo_layout() {
        let sdk_root = make_repo_sdk_root("repo-screenshots");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();

        assert_eq!(simulator_screenshots_dir(&sdk, &sdk_root.join("demo-app")), None);

        cleanup(sdk_root.parent().unwrap());
    }

    struct PathGuard(Option<OsString>);

    impl PathGuard {
        fn capture() -> Self { Self(std::env::var_os("PATH")) }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(path) = &self.0 {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    fn make_bundle_sdk_root(label: &str) -> PathBuf {
        let root = make_temp_dir(label);
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("lib").join("keyos")).unwrap();
        root
    }

    fn make_repo_sdk_root(label: &str) -> PathBuf {
        let repo_root = make_temp_dir(label);
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        fs::create_dir_all(root.join("ui").join("ui")).unwrap();
        root
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-sim-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}

/// Generate manifest.json content
fn generate_manifest_json(config: &AppConfig, project_root: &Path) -> Result<String> {
    let manifest = AppManifest::from_config(config, config.resolved_permissions(project_root)?);
    Ok(serde_json::to_string_pretty(&manifest)?)
}
