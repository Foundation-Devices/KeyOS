// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Args as ClapArgs, ValueEnum};
use colored::Colorize;

use crate::builder::{cargo, project_root};

#[derive(ClapArgs)]
pub struct Args {
    /// Flux app suite to run. Defaults to running all supported Flux apps.
    #[arg(value_enum, default_value_t = FluxAppSelection::All)]
    app: FluxAppSelection,
    /// Path to a prebuilt passport-drive binary.
    #[arg(long, value_name = "PATH")]
    passport_drive_bin: Option<PathBuf>,
    /// Do not run `cargo build -p passport-drive --release` before testing.
    #[arg(long)]
    skip_build_passport_drive: bool,
    /// Legacy launcher tap coordinate for the Ethereum tile (left).
    #[arg(long, value_parser = parse_tap_point, default_value = "124,210")]
    ethereum_tap: TapPoint,
    /// Legacy launcher tap coordinate for the Solana tile (right).
    #[arg(long, value_parser = parse_tap_point, default_value = "356,210")]
    solana_tap: TapPoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum FluxAppSelection {
    All,
    Ethereum,
    Solana,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FluxApp {
    Ethereum,
    Solana,
}

struct TestCase {
    id: &'static str,
    apdu: &'static str,
    expected: Expected,
}

enum Expected {
    SuccessLen(usize),
    /// GET_APP_AND_VERSION: SW=9000 and a `format(1) | name_len(1) | name | ...`
    /// body whose name matches. The Flux child answers this itself: its SDK's
    /// get_version reads the name and version from the generated os_registry tag,
    /// so a host session layer (e.g. Rabby's DMK) accepts the app.
    AppAndVersion {
        name: &'static str,
    },
    Challenge {
        len: usize,
        fixed_value: &'static str,
    },
    NonSuccess,
}

struct Rapdu {
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
struct TapPoint {
    x: u16,
    y: u16,
}

pub fn run(args: Args) -> Result<()> {
    let passport_drive = prepare_passport_drive(&args)?;

    preflight(&passport_drive)?;

    for &app in selected_apps(args.app) {
        let tap_point = app.tap_point(&args);
        println!("Launching {} via tap at {},{}", app.name(), tap_point.x, tap_point.y);
        tap(&passport_drive, tap_point)?;
        let pid = wait_for_app_process(&passport_drive, app, None, true)?;
        sleep(Duration::from_millis(500));

        println!("Running Flux APDU smoke tests for {}", app.name());
        for case in app.tests() {
            match run_test_case(&passport_drive, case) {
                Ok(rapdu) => print_test_pass(case, &rapdu),
                Err(err) => {
                    print_test_fail(case, &err);
                    return Err(err);
                }
            }
        }
        println!("Quitting {} PID {}", app.name(), pid);
        quit_flux_app(&passport_drive)?;
        wait_for_app_process(&passport_drive, app, Some(pid), false)?;
        sleep(Duration::from_millis(500));
    }
    println!("Flux APDU smoke tests passed");

    Ok(())
}

fn prepare_passport_drive(args: &Args) -> Result<PathBuf> {
    if let Some(path) = &args.passport_drive_bin {
        return Ok(path.clone());
    }

    if !args.skip_build_passport_drive {
        println!("Building passport-drive release binary...");
        let status = Command::new(cargo())
            .args(["build", "-p", "passport-drive", "--release"])
            .current_dir(project_root())
            .status()
            .context("failed to spawn cargo build for passport-drive")?;
        if !status.success() {
            bail!("cargo build -p passport-drive --release failed");
        }
    }

    Ok(default_passport_drive_bin())
}

fn default_passport_drive_bin() -> PathBuf {
    let target_dir =
        env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| project_root().join("target"));
    target_dir.join("release").join(format!("passport-drive{}", env::consts::EXE_SUFFIX))
}

fn selected_apps(selection: FluxAppSelection) -> &'static [FluxApp] {
    match selection {
        FluxAppSelection::All => &[FluxApp::Ethereum, FluxApp::Solana],
        FluxAppSelection::Ethereum => &[FluxApp::Ethereum],
        FluxAppSelection::Solana => &[FluxApp::Solana],
    }
}

fn parse_tap_point(s: &str) -> std::result::Result<TapPoint, String> {
    let (x, y) = s.split_once(',').ok_or_else(|| "tap point must be formatted as X,Y".to_string())?;
    let x = x.parse::<u16>().map_err(|e| format!("invalid X coordinate: {e}"))?;
    let y = y.parse::<u16>().map_err(|e| format!("invalid Y coordinate: {e}"))?;
    Ok(TapPoint { x, y })
}

fn run_passport_drive_apdu(passport_drive: &Path, apdu: &str) -> Result<Rapdu> {
    let stdout = run_passport_drive(passport_drive, &["send-apdu", apdu])?;
    parse_rapdu(&stdout)
}

fn run_test_case(passport_drive: &Path, case: &TestCase) -> Result<Rapdu> {
    let rapdu = run_passport_drive_apdu(passport_drive, case.apdu)
        .with_context(|| format!("{} APDU exchange failed", case.id))?;
    validate(case, &rapdu)?;
    Ok(rapdu)
}

fn print_test_pass(case: &TestCase, rapdu: &Rapdu) {
    let sw = rapdu.sw();
    println!("{} {:<14} len={:<3} SW={}", "✓".green().bold(), case.id, rapdu.bytes.len(), sw.green());
}

fn print_test_fail(case: &TestCase, err: &anyhow::Error) {
    eprintln!("{} {:<14} {err:#}", "✗".red().bold(), case.id);
}

fn run_passport_drive(passport_drive: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(passport_drive)
        .args(args)
        .current_dir(project_root())
        .output()
        .with_context(|| format!("failed to run {}", passport_drive.display()))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "passport-drive {} exited with {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status,
            stdout.trim_end(),
            stderr.trim_end()
        );
    }

    String::from_utf8(output.stdout).context("passport-drive stdout was not UTF-8")
}

fn preflight(passport_drive: &Path) -> Result<()> {
    println!("Checking Flux test preconditions...");

    let ports = run_passport_drive(passport_drive, &["list-ports"])?;
    if !ports.contains("VID:2c97 PID:7011") {
        bail!(
            "Flux tests must start from the Legacy Mode launcher.\n\
             Expected Passport Prime Flux/legacy USB identity VID:2c97 PID:7011.\n\
             Detected USB devices:\n{}",
            ports.trim_end()
        );
    }

    let processes = get_processes(passport_drive)?;
    if !processes.iter().any(|process| is_flux_launcher(&process.name)) {
        bail!("Flux tests must start with the Legacy Mode app running; gui-app-emu-flux is not in the process list");
    }

    let running_flux_children: Vec<_> =
        processes.iter().filter(|process| is_flux_child_process(&process.name)).collect();
    if !running_flux_children.is_empty() {
        let names = running_flux_children
            .iter()
            .map(|process| format!("{} (PID {})", process.name, process.pid))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Flux tests must start from the Legacy Mode launcher with no Ethereum/Solana app already running. \
             Close the running Flux child app first: {names}"
        );
    }

    println!("Preflight OK: Legacy VID/PID is active, launcher is running, and no Flux child app is open");
    Ok(())
}

fn is_flux_launcher(name: &str) -> bool {
    name.contains("gui-app-emu-flux") || name.contains("gui_app_emu_flux")
}

fn is_flux_child_process(name: &str) -> bool { name.contains("app-flux-") || name.contains("app_flux_") }

fn tap(passport_drive: &Path, point: TapPoint) -> Result<()> {
    run_passport_drive(passport_drive, &["tap", &point.x.to_string(), &point.y.to_string()]).map(|_| ())
}

fn get_processes(passport_drive: &Path) -> Result<Vec<ProcessInfo>> {
    let output = run_passport_drive(passport_drive, &["get-process-list"])?;
    Ok(output.lines().filter_map(ProcessInfo::parse).collect())
}

fn quit_flux_app(passport_drive: &Path) -> Result<()> {
    let rapdu = run_passport_drive_apdu(passport_drive, "b0a7000000")?;
    if rapdu.sw() != "9000" {
        bail!("BOLOS quit expected SW=9000, got SW={}", rapdu.sw());
    }
    Ok(())
}

fn wait_for_app_process(
    passport_drive: &Path,
    app: FluxApp,
    pid: Option<u16>,
    should_exist: bool,
) -> Result<u16> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let processes = get_processes(passport_drive)?;
        if let Some(process) = processes.iter().find(|process| {
            FluxApp::from_process_name(&process.name) == Some(app)
                && pid.map_or(true, |expected_pid| process.pid == expected_pid)
        }) {
            if should_exist {
                return Ok(process.pid);
            }
        } else if !should_exist {
            return Ok(pid.unwrap_or_default());
        }

        if should_exist {
            if let Some((wrong_pid, wrong_name)) = processes
                .iter()
                .find(|process| is_flux_child_process(&process.name))
                .map(|process| (process.pid, process.name.clone()))
            {
                quit_flux_app(passport_drive).with_context(|| {
                    format!("failed to quit unexpectedly launched {wrong_name} PID {wrong_pid}")
                })?;
                wait_for_pid_absent(passport_drive, wrong_pid, &wrong_name).with_context(|| {
                    format!("unexpectedly launched {wrong_name} PID {wrong_pid} did not exit")
                })?;
                bail!(
                    "Expected {} to launch, but fixed tap launched {} (PID {}). \
                     Adjust the tap coordinate option for this launcher layout.",
                    app.name(),
                    wrong_name,
                    wrong_pid
                );
            }
        }

        if Instant::now() >= deadline {
            let expectation = if should_exist { "appear" } else { "exit" };
            bail!("Timed out waiting for {} process to {expectation}", app.name());
        }
        sleep(Duration::from_millis(250));
    }
}

fn wait_for_pid_absent(passport_drive: &Path, pid: u16, name: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let processes = get_processes(passport_drive)?;
        if !processes.iter().any(|process| process.pid == pid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Timed out waiting for {name} PID {pid} to exit");
        }
        sleep(Duration::from_millis(250));
    }
}

fn parse_rapdu(stdout: &str) -> Result<Rapdu> {
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("RAPDU "))
        .with_context(|| format!("passport-drive output did not contain a RAPDU line: {stdout:?}"))?;
    let (_, hex) = line.rsplit_once(':').context("RAPDU line did not contain ':'")?;
    let bytes = parse_hex_bytes(hex.trim())?;
    if bytes.len() < 2 {
        bail!("RAPDU must include a 2-byte status word");
    }
    Ok(Rapdu { bytes })
}

fn parse_hex_bytes(hex: &str) -> Result<Vec<u8>> {
    let hex_clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if hex_clean.len() % 2 != 0 {
        bail!("hex value has an odd number of characters");
    }

    (0..hex_clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_clean[i..i + 2], 16).context("invalid hex value"))
        .collect()
}

fn validate(case: &TestCase, rapdu: &Rapdu) -> Result<()> {
    match case.expected {
        Expected::SuccessLen(len) => {
            if rapdu.sw() != "9000" {
                bail!("{} expected SW=9000, got SW={}", case.id, rapdu.sw());
            }
            if rapdu.bytes.len() != len {
                bail!("{} expected {} bytes, got {}", case.id, len, rapdu.bytes.len());
            }
        }
        Expected::AppAndVersion { name } => {
            if rapdu.sw() != "9000" {
                bail!("{} expected SW=9000, got SW={}", case.id, rapdu.sw());
            }
            let mut prefix = vec![0x01u8, name.len() as u8];
            prefix.extend_from_slice(name.as_bytes());
            if !rapdu.payload().starts_with(&prefix) {
                bail!(
                    "{} expected GET_APP_AND_VERSION for {name:?}, got payload {}",
                    case.id,
                    rapdu.payload_hex()
                );
            }
        }
        Expected::Challenge { len, fixed_value } => {
            if rapdu.sw() != "9000" {
                bail!("{} expected SW=9000, got SW={}", case.id, rapdu.sw());
            }
            if rapdu.bytes.len() != len {
                bail!("{} expected {} bytes, got {}", case.id, len, rapdu.bytes.len());
            }
            let challenge = rapdu.payload_hex();
            if challenge == fixed_value {
                bail!("{} returned fixed challenge {}", case.id, fixed_value);
            }
        }
        Expected::NonSuccess => {
            if rapdu.sw() == "9000" {
                bail!("{} expected non-9000 status, got SW=9000", case.id);
            }
        }
    }
    Ok(())
}

impl Rapdu {
    fn sw(&self) -> String {
        format!("{:02x}{:02x}", self.bytes[self.bytes.len() - 2], self.bytes[self.bytes.len() - 1])
    }

    fn payload(&self) -> &[u8] { &self.bytes[..self.bytes.len() - 2] }

    fn payload_hex(&self) -> String { self.payload().iter().map(|b| format!("{b:02x}")).collect() }
}

impl FluxApp {
    fn name(self) -> &'static str {
        match self {
            FluxApp::Ethereum => "Ethereum",
            FluxApp::Solana => "Solana",
        }
    }

    fn tap_point(self, args: &Args) -> TapPoint {
        match self {
            FluxApp::Ethereum => args.ethereum_tap,
            FluxApp::Solana => args.solana_tap,
        }
    }

    fn from_process_name(name: &str) -> Option<Self> {
        if name.contains("app-flux-ethereum") || name.contains("app_flux_ethereum") {
            Some(FluxApp::Ethereum)
        } else if name.contains("app-flux-solana") || name.contains("app_flux_solana") {
            Some(FluxApp::Solana)
        } else {
            None
        }
    }

    fn tests(self) -> &'static [TestCase] {
        match self {
            FluxApp::Ethereum => &[
                TestCase {
                    id: "ETH-APP-01",
                    apdu: "b001000000",
                    expected: Expected::AppAndVersion { name: "Ethereum" },
                },
                TestCase { id: "ETH-CONFIG-01", apdu: "e006000000", expected: Expected::SuccessLen(6) },
                TestCase {
                    id: "ETH-RNG-01",
                    apdu: "e020000000",
                    expected: Expected::Challenge { len: 6, fixed_value: "12345678" },
                },
                TestCase {
                    id: "ETH-PUBKEY-01",
                    apdu: "e002000015058000002c8000003c800000000000000000000000",
                    expected: Expected::SuccessLen(109),
                },
                TestCase {
                    id: "ETH-PUBKEY-02",
                    apdu: "e002000115058000002c8000003c800000000000000000000000",
                    expected: Expected::SuccessLen(141),
                },
                TestCase { id: "ETH-NEG-01", apdu: "0006000000", expected: Expected::NonSuccess },
            ],
            FluxApp::Solana => &[
                TestCase {
                    id: "SOL-APP-01",
                    apdu: "b001000000",
                    expected: Expected::AppAndVersion { name: "Solana" },
                },
                // 5 base config bytes (blind-sign, pubkey-display, major, minor, patch)
                // plus 2 for the Transaction Check settings the newer app reports.
                TestCase { id: "SOL-CONFIG-01", apdu: "e004000000", expected: Expected::SuccessLen(9) },
                TestCase {
                    id: "SOL-RNG-01",
                    apdu: "e020000000",
                    expected: Expected::Challenge { len: 6, fixed_value: "deadbeef" },
                },
                TestCase {
                    id: "SOL-PUBKEY-01",
                    apdu: "e00500000d038000002c800001f580000000",
                    expected: Expected::SuccessLen(34),
                },
                TestCase {
                    id: "SOL-PUBKEY-02",
                    apdu: "e005000011048000002c800001f58000000080000000",
                    expected: Expected::SuccessLen(34),
                },
                TestCase { id: "SOL-NEG-01", apdu: "e006000000", expected: Expected::NonSuccess },
            ],
        }
    }
}

struct ProcessInfo {
    pid: u16,
    name: String,
}

impl ProcessInfo {
    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        if parts.next()? != "R" {
            return None;
        }
        let pid = parts.next()?.parse().ok()?;
        parts.next()?;
        let name = parts.next()?.to_string();
        Some(Self { pid, name })
    }
}
