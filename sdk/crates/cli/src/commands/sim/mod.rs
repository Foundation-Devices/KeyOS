// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Sim command - build an app for hosted execution and run it in the KeyOS simulator

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use foundation_core::{app_manifest_from_config, AppConfig, ProjectContext, SdkLayout, SdkRoot};

use crate::assets::{stage_bundled_icon, stage_hardware_assets};
use crate::cargo_support::{
    configure_host_build_environment, emit_cargo_messages, emit_stderr_if_present,
    ensure_development_environment,
};
use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};

const SCREENSHOTS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_SCREENSHOTS_DIR";
const APP_ELF_ROOT_ENV: &str = "FOUNDATION_SIMULATOR_APP_ELF_ROOT";

const FATFS_IMAGE_BINARY: &str = "fatfs-image";
const FATFS_IMAGE_ENV: &str = "FOUNDATION_FATFS_IMAGE";
const SYSTEM_IMAGE: &str = "disk_system.dat";
const SYSTEM_IMAGE_SIZE: &str = "128M";

/// Execute the sim command
pub fn execute() -> Result<()> {
    println!("Building application for simulator...");
    println!("Note: Building for hosted/simulator mode (x86_64/aarch64 native)");
    println!();

    let sdk = SdkRoot::discover().map_err(|_| anyhow::anyhow!("Could not locate the Foundation SDK root. Run this command from the SDK checkout or unpacked SDK bundle."))?;

    // A source checkout builds against the Nix toolchain; an unpacked SDK bundle is
    // self-contained, so only bootstrap Nix for the repo layout.
    if matches!(sdk.layout(), SdkLayout::Repo) {
        ensure_development_environment("foundation sim")?;
    }

    // Find and read app-config.toml
    let project = ProjectContext::discover()?;
    let project_root = project.root.as_path();
    let config = &project.config;

    // Mirror hardware builds and viewer flows: make sure shared @ui sources plus any
    // build.rs-generated router/translation files are ready before the hosted build.
    prepare_project_for_build(project_root, &sdk)?;

    // Ensure the app's theme Rust is generated and current (foundation_themes::include_theme!).
    let themes_rust_dir = crate::commands::themes::ensure_project_theme(&sdk, config, project_root)?;

    // Build the app for native/hosted execution (not the ARM hardware target).
    build_for_simulator(project_root, config, sdk.root(), &themes_rust_dir)?;

    let app_id_hex = config.app_id.as_hex();
    // app-manager keys a sideloaded bundle on the bare hex app id (no `0x`); the
    // host-launch path and the in-image bundle dir must both use it.
    let sideloaded_dir = app_id_hex.trim_start_matches("0x");

    // The app-elf root mirrors the image's /keyos dir (built-ins under apps,
    // sideloaded under sideloaded-apps); app-manager execs the dev app.elf here.
    let app_elf_root = simulator_app_elf_root(&sdk);
    println!("Copying application to KeyOS SDK...");
    let dest_dir = copy_to_sdk(config, project_root, &app_elf_root.join("sideloaded-apps"), sideloaded_dir)?;

    // The dev app is read through fs like on device, so its manifest, icon, and
    // resources go into the simulator's system image (the app.elf does not; it is
    // exec'd from the host stage above).
    inject_sideloaded_app(&sdk, config, project_root, sideloaded_dir)?;

    println!("Starting KeyOS simulator...");
    launch_simulator(&sdk, &dest_dir, app_id_hex, project_root, &app_elf_root, &find_in_path)?;

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
    // `@theme` namespace → per-app generated component themes.
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

/// Copy the built app to `<apps_dir>/<dest_name>/app.elf` (plus a manifest for
/// reference). The dir is named by the bare hex app id so the host-launch path
/// resolves it; the binary is still the cargo package output.
fn copy_to_sdk(config: &AppConfig, project_root: &Path, apps_dir: &Path, dest_name: &str) -> Result<PathBuf> {
    let profile = "debug"; // Simulator always uses debug for faster iteration
    let binary_path = project_root.join("target").join(profile).join(&config.app_name);

    if !binary_path.exists() {
        anyhow::bail!("Built binary not found at: {}", binary_path.display());
    }

    let dest_dir = apps_dir.join(dest_name);
    fs::create_dir_all(&dest_dir)?;

    let dest_binary = dest_dir.join("app.elf");
    fs::copy(&binary_path, &dest_binary)
        .with_context(|| format!("Failed to copy binary to {}", dest_binary.display()))?;

    let manifest_json = generate_manifest_json(config, project_root)?;
    fs::write(dest_dir.join("manifest.json"), manifest_json)?;

    // Stage the icons next to app.elf so the simulator's app-manager serves them
    // over the same GetAppIcon IPC as hardware.
    stage_bundled_icon(config, project_root, &dest_dir)?;

    Ok(dest_dir)
}

/// Build the dev app's device-format bundle (manifest, icon, resources; no
/// app.elf) and inject it into the simulator system image under
/// `keyos/sideloaded-apps/<hex>`, where app-manager enumerates it through fs.
fn inject_sideloaded_app(
    sdk: &SdkRoot,
    config: &AppConfig,
    project_root: &Path,
    sideloaded_dir: &str,
) -> Result<()> {
    let bundle_dir = project_root.join("target").join("foundation").join("sim-sideload").join(sideloaded_dir);
    if bundle_dir.exists() {
        fs::remove_dir_all(&bundle_dir)
            .with_context(|| format!("Failed to clean sideload bundle dir {}", bundle_dir.display()))?;
    }
    fs::create_dir_all(&bundle_dir)?;
    stage_hardware_assets(config, project_root, &bundle_dir)?;
    fs::write(bundle_dir.join("manifest.json"), generate_manifest_json(config, project_root)?)?;

    let kernel_dir = simulator_kernel_dir(sdk);
    ensure_simulator_images(sdk, &kernel_dir)?;
    let system_image = kernel_dir.join(SYSTEM_IMAGE);

    println!("Injecting app into simulator system image...");
    let dest = format!("keyos/sideloaded-apps/{sideloaded_dir}");
    // Drop the old bundle first, or stale entries from a previous run linger.
    run_fatfs_image(sdk, &[OsStr::new("rm"), system_image.as_os_str(), OsStr::new(&dest)])?;
    run_fatfs_image(
        sdk,
        &[OsStr::new("cp"), system_image.as_os_str(), bundle_dir.as_os_str(), OsStr::new(&dest)],
    )
}

/// Directory the hosted kernel runs in, where `os/fs` opens `disk*.dat`.
fn simulator_kernel_dir(sdk: &SdkRoot) -> PathBuf {
    match sdk.layout() {
        SdkLayout::Repo => sdk.keyos_root().join("xous").join("kernel"),
        SdkLayout::Bundle => sdk.keyos_root().join("simulator").join("xous").join("kernel"),
    }
}

/// Host mirror of the image's `/keyos` dir, holding the binaries the simulator
/// execs (`apps/<name>/app.elf`, `sideloaded-apps/<hex>/app.elf`).
fn simulator_app_elf_root(sdk: &SdkRoot) -> PathBuf {
    match sdk.layout() {
        SdkLayout::Repo => sdk.keyos_root().join("target").join("hosted").join("keyos"),
        SdkLayout::Bundle => sdk.keyos_root().join("simulator"),
    }
}

/// Create the simulator system image if missing, so the injected dev app has a
/// volume to land in. `os/fs` mounts it instead of formatting, so any seeded
/// assets must persist; create-if-missing never clobbers a shipped image. The
/// user volume (disk.dat) is created by the simulator launcher on first run.
fn ensure_simulator_images(sdk: &SdkRoot, kernel_dir: &Path) -> Result<()> {
    fs::create_dir_all(kernel_dir)
        .with_context(|| format!("Failed to create simulator kernel dir {}", kernel_dir.display()))?;
    ensure_image(sdk, &kernel_dir.join(SYSTEM_IMAGE), SYSTEM_IMAGE_SIZE, "PRIME")
}

fn ensure_image(sdk: &SdkRoot, image: &Path, size: &str, label: &str) -> Result<()> {
    if image.exists() {
        return Ok(());
    }
    println!("Creating simulator image {}", image.display());
    run_fatfs_image(
        sdk,
        &[
            OsStr::new("create"),
            image.as_os_str(),
            OsStr::new("--size"),
            OsStr::new(size),
            OsStr::new("--label"),
            OsStr::new(label),
        ],
    )
}

fn run_fatfs_image(sdk: &SdkRoot, args: &[&OsStr]) -> Result<()> {
    let output = fatfs_image_command(sdk)?
        .args(args)
        .output()
        .with_context(|| format!("Failed to run {FATFS_IMAGE_BINARY}"))?;
    if !output.status.success() {
        bail!("{FATFS_IMAGE_BINARY} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// Resolve the `fatfs-image` helper: an explicit override, the SDK-bundled
/// binary, or a source build from the KeyOS workspace (repo layout).
fn fatfs_image_command(sdk: &SdkRoot) -> Result<Command> {
    if let Some(path) = std::env::var_os(FATFS_IMAGE_ENV) {
        return Ok(Command::new(path));
    }

    if let Some(path) = sdk.tool_path(&[FATFS_IMAGE_BINARY]) {
        return Ok(Command::new(path));
    }

    let manifest = sdk.keyos_root().join("utils").join("fatfs-image").join("Cargo.toml");
    if manifest.exists() {
        let mut command = Command::new("cargo");
        command.arg("run").arg("--quiet").arg("--manifest-path").arg(manifest).arg("--");
        return Ok(command);
    }

    bail!("{FATFS_IMAGE_BINARY} not found. Reinstall the Foundation SDK or set {FATFS_IMAGE_ENV} to the helper binary path.")
}

fn launch_simulator(
    sdk: &SdkRoot,
    staged_dir: &Path,
    app_id_hex: &str,
    project_root: &Path,
    app_elf_root: &Path,
    find_tool: &dyn Fn(&str) -> Option<PathBuf>,
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
            app_elf_root,
        );
    }

    // In repo layout, prefer `just sim` which builds the simulator from source and uses
    // the same target/apps/ directory where we staged the app. The bundled
    // foundation-simulator on PATH uses its own directory layout that won't find apps
    // staged for repo development.
    if matches!(sdk.layout(), SdkLayout::Repo) {
        if let Some(just) = find_tool("just") {
            let mut command = Command::new(&just);
            command.arg("sim");
            return run_simulator_command(
                command,
                format!("`just sim` from {}", sdk.keyos_root().display()),
                &sdk.keyos_root(),
                staged_dir,
                app_id_hex,
                None,
                app_elf_root,
            );
        }
    }

    if let Some(path) = find_tool("foundation-simulator") {
        return run_simulator_command(
            Command::new(&path),
            format!("simulator on PATH at {}", path.display()),
            sdk.root(),
            staged_dir,
            app_id_hex,
            screenshots_dir.as_deref(),
            app_elf_root,
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
    app_elf_root: &Path,
) -> Result<()> {
    command.current_dir(current_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(screenshots_dir) = screenshots_dir {
        command.env(SCREENSHOTS_DIR_ENV, screenshots_dir);
    }
    command.env(APP_ELF_ROOT_ENV, app_elf_root);
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    use foundation_core::SdkRoot;

    use super::{
        drain_pending_control_events, launch_simulator, parse_control_line, simulator_screenshots_dir,
        ControlEvent,
    };
    use crate::test_support::{link_fake_bin, make_temp_dir};

    #[test]
    fn launches_bundled_simulator_from_bundle_layout() {
        let (_sdk_dir, sdk_root) = make_bundle_sdk_root("bundle-launch");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let staged_dir = sdk_root.join("target").join("apps").join("demo");
        fs::create_dir_all(&staged_dir).unwrap();

        let app_elf_root = sdk_root.join("elf-root");
        launch_simulator(
            &sdk,
            &staged_dir,
            "0x00112233",
            &sdk_root.join("example-app"),
            &app_elf_root,
            &|_| None,
        )
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
            PathBuf::from(fs::read_to_string(sdk_root.join("simulator-app-elf-root.log")).unwrap().trim()),
            app_elf_root
        );
        assert!(fs::read_to_string(sdk_root.join("simulator-stdin.log")).unwrap().contains("run 0x00112233"));
    }

    #[test]
    fn falls_back_to_just_sim_for_repo_layout() {
        let (_repo_dir, sdk_root) = make_repo_sdk_root("repo-launch");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let staged_dir = sdk_root.join("staged-app");
        fs::create_dir_all(&staged_dir).unwrap();

        let app_elf_root = sdk_root.join("elf-root");
        let find_tool =
            |command: &str| (command == "just").then(|| Path::new(env!("FOUNDATION_FAKE_BIN")).join("just"));
        launch_simulator(
            &sdk,
            &staged_dir,
            "0xaabbccdd",
            &sdk_root.join("example-app"),
            &app_elf_root,
            &find_tool,
        )
        .unwrap();

        assert_eq!(
            fs::canonicalize(fs::read_to_string(sdk.keyos_root().join("just-sim.log")).unwrap().trim())
                .unwrap(),
            fs::canonicalize(sdk.keyos_root()).unwrap()
        );
        assert!(fs::read_to_string(sdk.keyos_root().join("just-sim-screenshots-dir.log"))
            .unwrap()
            .trim()
            .is_empty());
        assert_eq!(
            PathBuf::from(
                fs::read_to_string(sdk.keyos_root().join("just-sim-app-elf-root.log")).unwrap().trim()
            ),
            app_elf_root
        );
        assert!(fs::read_to_string(sdk.keyos_root().join("just-sim-stdin.log"))
            .unwrap()
            .contains("run 0xaabbccdd"));
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
        let (_sdk_dir, sdk_root) = make_bundle_sdk_root("bundle-screenshots");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root = sdk_root.join("demo-app");

        assert_eq!(simulator_screenshots_dir(&sdk, &project_root), Some(project_root.join("screenshots")));
    }

    #[test]
    fn keeps_existing_screenshots_location_for_repo_layout() {
        let (_repo_dir, sdk_root) = make_repo_sdk_root("repo-screenshots");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();

        assert_eq!(simulator_screenshots_dir(&sdk, &sdk_root.join("demo-app")), None);
    }

    fn make_bundle_sdk_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let root = dir.path().to_path_buf();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        link_fake_bin(&root.join("bin"));
        fs::create_dir_all(root.join("lib").join("keyos")).unwrap();
        (dir, root)
    }

    fn make_repo_sdk_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let repo_root = dir.path();
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        fs::create_dir_all(root.join("ui").join("ui")).unwrap();
        (dir, root)
    }
}

/// Generate manifest.json content
fn generate_manifest_json(config: &AppConfig, project_root: &Path) -> Result<String> {
    let manifest = app_manifest_from_config(config, config.resolved_permissions(project_root)?);
    Ok(serde_json::to_string_pretty(&manifest)?)
}
