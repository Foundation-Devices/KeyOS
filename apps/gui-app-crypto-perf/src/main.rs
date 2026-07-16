// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::OnceCell,
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::{
    signature::hazmat::PrehashSigner, Signature as P256Signature, SigningKey as P256SigningKey,
};
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use server::xous::{map_memory, MemoryFlags, MemoryRange};
use sha2::{Digest, Sha256};
use slint_keyos_platform::{app, gui_server_api::InputMessage};

crypto::use_api!();
security::use_api!();

const ONE_SECOND: Duration = Duration::from_secs(1);
const STABILITY_WINDOW: Duration = Duration::from_secs(5);
const MIN_RUNTIME: Duration = Duration::from_secs(5);
const STABILITY_THRESHOLD: f64 = 0.005;
const PAGE_SIZE: usize = 0x1000;

static CURRENT_RUN_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static UI: OnceCell<AppWindow> = OnceCell::new();
}

app!("Crypto Performance");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchmarkId {
    Sha256 = 0,
    EcdsaP256 = 1,
    BitcoinSecp256k1 = 2,
    Taproot = 3,
    TlsP256 = 4,
    JwtEs256 = 5,
}

impl BenchmarkId {
    fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Sha256),
            1 => Some(Self::EcdsaP256),
            2 => Some(Self::BitcoinSecp256k1),
            3 => Some(Self::Taproot),
            4 => Some(Self::TlsP256),
            5 => Some(Self::JwtEs256),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum MetricKind {
    BytesPerSecond,
    Operations(&'static str),
}

impl MetricKind {
    fn format(self, value: f64) -> String {
        match self {
            MetricKind::BytesPerSecond => format!("{:.2} MiB/s", value / (1024.0 * 1024.0)),
            MetricKind::Operations(unit) => format!("{value:.2} {unit}/s"),
        }
    }
}

struct BenchmarkOutcome {
    status: String,
    performance: String,
}

trait BenchmarkRunner {
    fn metric_kind(&self) -> MetricKind;
    fn run_once(&mut self, case_index: usize) -> Result<u64, String>;
}

struct ActiveBenchmark {
    run_id: u64,
    stop: Arc<AtomicBool>,
}

struct DualInputBuffer {
    memory: MemoryRange,
    offsets: [usize; 2],
    lengths: [usize; 2],
    software_inputs: [Vec<u8>; 2],
}

impl DualInputBuffer {
    fn new(a: Vec<u8>, b: Vec<u8>) -> Result<Self, String> {
        let offset0 = 0usize;
        let offset1 = align_up(a.len().max(PAGE_SIZE), PAGE_SIZE);
        let total_len = align_up(offset1 + b.len(), PAGE_SIZE);
        let mut memory = map_memory(None, None, total_len, MemoryFlags::W | MemoryFlags::POPULATE)
            .map_err(|err| format!("map_memory failed: {err:?}"))?;

        let slice = memory.as_slice_mut::<u8>();
        slice.fill(0);
        slice[offset0..offset0 + a.len()].copy_from_slice(&a);
        slice[offset1..offset1 + b.len()].copy_from_slice(&b);

        Ok(Self { memory, offsets: [offset0, offset1], lengths: [a.len(), b.len()], software_inputs: [a, b] })
    }

    fn software_input(&self, index: usize) -> &[u8] { &self.software_inputs[index] }

    fn hardware_sha256(&self, crypto: &CryptoApi, index: usize) -> Result<[u8; 32], String> {
        let start = self.offsets[index];
        let end = start + self.lengths[index];
        crypto
            .sha256(&self.memory.as_slice::<u8>()[start..end])
            .map_err(|err| format!("hardware sha256 failed: {err:?}"))
    }
}

struct Sha256HardwareRunner {
    crypto: CryptoApi,
    inputs: DualInputBuffer,
}

impl Sha256HardwareRunner {
    fn new() -> Result<Self, String> {
        Ok(Self {
            crypto: CryptoApi::default(),
            inputs: DualInputBuffer::new(patterned_bytes(0x11, 16 * 1024), patterned_bytes(0x92, 16 * 1024))?,
        })
    }
}

impl BenchmarkRunner for Sha256HardwareRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::BytesPerSecond }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let index = case_index % 2;
        let _ = self.inputs.hardware_sha256(&self.crypto, index)?;
        Ok(self.inputs.lengths[index] as u64)
    }
}

struct Sha256SoftwareRunner {
    inputs: [Vec<u8>; 2],
}

impl Sha256SoftwareRunner {
    fn new() -> Self { Self { inputs: [patterned_bytes(0x11, 16 * 1024), patterned_bytes(0x92, 16 * 1024)] } }
}

impl BenchmarkRunner for Sha256SoftwareRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::BytesPerSecond }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let index = case_index % 2;
        let _ = Sha256::digest(&self.inputs[index]);
        Ok(self.inputs[index].len() as u64)
    }
}

struct P256SoftwareRunner {
    signing_key: P256SigningKey,
    digests: [[u8; 32]; 2],
    unit: &'static str,
}

impl P256SoftwareRunner {
    fn new(unit: &'static str) -> Self {
        Self {
            signing_key: P256SigningKey::from_slice(&[0x41; 32]).expect("valid P-256 signing key"),
            digests: [digest32(b"p256-case-a"), digest32(b"p256-case-b")],
            unit,
        }
    }

    fn sign_digest(&self, digest: [u8; 32]) -> Result<[u8; 64], String> {
        let signature: P256Signature = self
            .signing_key
            .sign_prehash(&digest)
            .map_err(|err| format!("software P-256 sign failed: {err:?}"))?;
        Ok(signature.to_bytes().into())
    }
}

impl BenchmarkRunner for P256SoftwareRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations(self.unit) }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let _ = self.sign_digest(self.digests[case_index % 2])?;
        Ok(1)
    }
}

struct P256HardwareRunner {
    security: Security,
    digests: [[u8; 32]; 2],
    unit: &'static str,
}

impl P256HardwareRunner {
    fn new(unit: &'static str) -> Self {
        Self {
            security: Security::default(),
            digests: [digest32(b"p256-case-a"), digest32(b"p256-case-b")],
            unit,
        }
    }

    fn sign_digest(&self, digest: [u8; 32]) -> Result<[u8; 64], String> {
        self.security.sign_with_fido_key(digest).map_err(|err| format!("hardware P-256 sign failed: {err:?}"))
    }
}

impl BenchmarkRunner for P256HardwareRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations(self.unit) }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let _ = self.sign_digest(self.digests[case_index % 2])?;
        Ok(1)
    }
}

struct Secp256k1Runner {
    secp: Secp256k1<secp256k1::All>,
    secret_key: SecretKey,
    digests: [[u8; 32]; 2],
}

impl Secp256k1Runner {
    fn new() -> Self {
        Self {
            secp: Secp256k1::new(),
            secret_key: SecretKey::from_slice(&[0x52; 32]).expect("valid secp256k1 secret key"),
            digests: [digest32(b"secp256k1-case-a"), digest32(b"secp256k1-case-b")],
        }
    }
}

impl BenchmarkRunner for Secp256k1Runner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations("signatures") }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let message = Message::from_digest(self.digests[case_index % 2]);
        let _ = self.secp.sign_ecdsa(&message, &self.secret_key);
        Ok(1)
    }
}

struct TaprootRunner {
    secp: Secp256k1<secp256k1::All>,
    keypair: Keypair,
    digests: [[u8; 32]; 2],
}

impl TaprootRunner {
    fn new() -> Self {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[0x63; 32]).expect("valid taproot secret key");
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        Self { secp, keypair, digests: [digest32(b"taproot-case-a"), digest32(b"taproot-case-b")] }
    }
}

impl BenchmarkRunner for TaprootRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations("signatures") }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let message = Message::from_digest(self.digests[case_index % 2]);
        let _ = self.secp.sign_schnorr_no_aux_rand(&message, &self.keypair);
        Ok(1)
    }
}

enum P256Signer {
    Software(P256SigningKey),
    Hardware(Security),
}

impl P256Signer {
    fn software() -> Self {
        Self::Software(P256SigningKey::from_slice(&[0x74; 32]).expect("valid TLS/JWT software key"))
    }

    fn hardware() -> Self { Self::Hardware(Security::default()) }

    fn sign_raw(&self, digest: [u8; 32]) -> Result<[u8; 64], String> {
        match self {
            P256Signer::Software(signing_key) => {
                let signature: P256Signature = signing_key
                    .sign_prehash(&digest)
                    .map_err(|err| format!("software P-256 sign failed: {err:?}"))?;
                Ok(signature.to_bytes().into())
            }
            P256Signer::Hardware(security) => security
                .sign_with_fido_key(digest)
                .map_err(|err| format!("hardware P-256 sign failed: {err:?}")),
        }
    }
}

struct TlsRunner {
    hash_mode: HashMode,
    signer: P256Signer,
    transcripts: DualInputBuffer,
}

impl TlsRunner {
    fn new(hardware: bool) -> Result<Self, String> {
        Ok(Self {
            hash_mode: if hardware { HashMode::Hardware(CryptoApi::default()) } else { HashMode::Software },
            signer: if hardware { P256Signer::hardware() } else { P256Signer::software() },
            transcripts: DualInputBuffer::new(patterned_bytes(0x31, 2048), patterned_bytes(0x79, 2048))?,
        })
    }
}

impl BenchmarkRunner for TlsRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations("TLS sign ops") }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let index = case_index % 2;
        let digest = self.hash_mode.sha256(&self.transcripts, index)?;
        let signature = self.signer.sign_raw(digest)?;
        let der = P256Signature::from_slice(&signature)
            .map_err(|err| format!("TLS DER conversion failed: {err:?}"))?
            .to_der();
        let _ = der.as_bytes().len();
        Ok(1)
    }
}

struct JwtRunner {
    hash_mode: HashMode,
    signer: P256Signer,
    signing_inputs: DualInputBuffer,
}

impl JwtRunner {
    fn new(hardware: bool) -> Result<Self, String> {
        let header = br#"{"alg":"ES256","typ":"JWT"}"#;
        let payload_a = br#"{"iss":"keyos","sub":"perf-a","iat":1710000000,"scope":"benchmark"}"#;
        let payload_b = br#"{"iss":"keyos","sub":"perf-b","iat":1710000100,"scope":"benchmark"}"#;
        let input_a = jwt_signing_input(header, payload_a);
        let input_b = jwt_signing_input(header, payload_b);

        Ok(Self {
            hash_mode: if hardware { HashMode::Hardware(CryptoApi::default()) } else { HashMode::Software },
            signer: if hardware { P256Signer::hardware() } else { P256Signer::software() },
            signing_inputs: DualInputBuffer::new(input_a.into_bytes(), input_b.into_bytes())?,
        })
    }
}

impl BenchmarkRunner for JwtRunner {
    fn metric_kind(&self) -> MetricKind { MetricKind::Operations("tokens") }

    fn run_once(&mut self, case_index: usize) -> Result<u64, String> {
        let index = case_index % 2;
        let digest = self.hash_mode.sha256(&self.signing_inputs, index)?;
        let signature = self.signer.sign_raw(digest)?;
        let mut token = self.signing_inputs.software_input(index).to_vec();
        token.push(b'.');
        token.extend_from_slice(URL_SAFE_NO_PAD.encode(signature).as_bytes());
        let _ = token.len();
        Ok(1)
    }
}

enum HashMode {
    Software,
    Hardware(CryptoApi),
}

impl HashMode {
    fn sha256(&self, inputs: &DualInputBuffer, index: usize) -> Result<[u8; 32], String> {
        match self {
            HashMode::Software => Ok(Sha256::digest(inputs.software_input(index)).into()),
            HashMode::Hardware(crypto) => inputs.hardware_sha256(crypto, index),
        }
    }
}

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("Starting Crypto Performance");

    UI.with(|slot| {
        slot.set(ui.clone_strong()).ok();
    });

    let active_benchmark = Arc::new(Mutex::new(None::<ActiveBenchmark>));

    cx.set_input_handler({
        let active_benchmark = active_benchmark.clone();
        move |input| {
            if input.msg == InputMessage::Hidden {
                stop_active_benchmark(&active_benchmark, "Stopped");
            }
        }
    });

    ui.global::<Callbacks>().on_test_title(move |test_id| title_for_test(test_id).into());
    ui.global::<Callbacks>().on_test_description(move |test_id| description_for_test(test_id).into());
    ui.global::<Callbacks>().on_hardware_supported(move |test_id| hardware_supported(test_id));

    ui.global::<Callbacks>().on_start_benchmark({
        let active_benchmark = active_benchmark.clone();
        move |test_id, hardware| {
            let Some(benchmark_id) = BenchmarkId::from_i32(test_id) else {
                queue_with_ui(move |ui| {
                    let state = ui.global::<State>();
                    state.set_running(false);
                    state.set_status_text("Unknown benchmark".into());
                });
                return;
            };

            let run_id = CURRENT_RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;
            let stop = Arc::new(AtomicBool::new(false));

            if let Some(previous) = active_benchmark
                .lock()
                .expect("active benchmark mutex poisoned")
                .replace(ActiveBenchmark { run_id, stop: stop.clone() })
            {
                previous.stop.store(true, Ordering::SeqCst);
            }

            queue_with_ui(move |ui| {
                let state = ui.global::<State>();
                state.set_running(true);
                state.set_status_text("Running...".into());
                state.set_performance_text("Collecting samples...".into());
            });

            let active_benchmark = active_benchmark.clone();
            std::thread::spawn(move || {
                run_benchmark_thread(active_benchmark, run_id, benchmark_id, hardware, stop)
            });
        }
    });

    ui.global::<Callbacks>().on_stop_benchmark({
        let active_benchmark = active_benchmark.clone();
        move || stop_active_benchmark(&active_benchmark, "Stopped")
    });

    ui.run().expect("UI running");
}

fn run_benchmark_thread(
    active_benchmark: Arc<Mutex<Option<ActiveBenchmark>>>,
    run_id: u64,
    benchmark_id: BenchmarkId,
    hardware: bool,
    stop: Arc<AtomicBool>,
) {
    let outcome = run_benchmark_loop(run_id, benchmark_id, hardware, stop);

    if CURRENT_RUN_ID.load(Ordering::SeqCst) != run_id {
        return;
    }

    {
        let mut guard = active_benchmark.lock().expect("active benchmark mutex poisoned");
        if guard.as_ref().map(|active| active.run_id) == Some(run_id) {
            guard.take();
        }
    }

    match outcome {
        Ok(outcome) => {
            queue_with_ui_for_run(run_id, move |ui| {
                let state = ui.global::<State>();
                state.set_running(false);
                state.set_status_text(outcome.status.into());
                state.set_performance_text(outcome.performance.into());
            });
        }
        Err(err) => {
            queue_with_ui_for_run(run_id, move |ui| {
                let state = ui.global::<State>();
                state.set_running(false);
                state.set_status_text(format!("Error: {err}").into());
            });
        }
    }
}

fn run_benchmark_loop(
    run_id: u64,
    benchmark_id: BenchmarkId,
    hardware: bool,
    stop: Arc<AtomicBool>,
) -> Result<BenchmarkOutcome, String> {
    let mut benchmark = create_benchmark(benchmark_id, hardware)?;
    let metric_kind = benchmark.metric_kind();
    let start = Instant::now();
    let mut next_report = start + ONE_SECOND;
    let mut total_units = 0u64;
    let mut case_index = 0usize;
    let mut history = VecDeque::<(Duration, f64)>::new();
    let mut latest_rate = 0.0f64;

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(BenchmarkOutcome {
                status: "Stopped".to_string(),
                performance: metric_kind.format(latest_rate),
            });
        }

        total_units = total_units.saturating_add(benchmark.run_once(case_index)?);
        case_index = case_index.wrapping_add(1);

        let now = Instant::now();
        if now < next_report {
            continue;
        }

        let elapsed = now.duration_since(start);
        latest_rate = total_units as f64 / elapsed.as_secs_f64();
        let performance = metric_kind.format(latest_rate);
        let status = format!("Running {:.0}s", elapsed.as_secs_f64());

        queue_with_ui_for_run(run_id, {
            let performance = performance.clone();
            move |ui| {
                let state = ui.global::<State>();
                state.set_running(true);
                state.set_status_text(status.clone().into());
                state.set_performance_text(performance.into());
            }
        });

        history.push_back((elapsed, latest_rate));
        while history.len() > 1 {
            let second = history.get(1).copied().expect("history has at least two entries");
            if elapsed.saturating_sub(second.0) >= STABILITY_WINDOW {
                history.pop_front();
            } else {
                break;
            }
        }

        if elapsed >= MIN_RUNTIME {
            if let Some((previous_elapsed, previous_rate)) = history.front().copied() {
                if elapsed.saturating_sub(previous_elapsed) >= STABILITY_WINDOW {
                    let baseline = previous_rate.max(f64::EPSILON);
                    let delta = ((latest_rate - previous_rate).abs()) / baseline;
                    if delta <= STABILITY_THRESHOLD {
                        return Ok(BenchmarkOutcome {
                            status: format!("Stable after {:.1}s", elapsed.as_secs_f64()),
                            performance,
                        });
                    }
                }
            }
        }

        while next_report <= now {
            next_report += ONE_SECOND;
        }
    }
}

fn create_benchmark(benchmark_id: BenchmarkId, hardware: bool) -> Result<Box<dyn BenchmarkRunner>, String> {
    match (benchmark_id, hardware) {
        (BenchmarkId::Sha256, true) => Ok(Box::new(Sha256HardwareRunner::new()?)),
        (BenchmarkId::Sha256, false) => Ok(Box::new(Sha256SoftwareRunner::new())),
        (BenchmarkId::EcdsaP256, true) => Ok(Box::new(P256HardwareRunner::new("signatures"))),
        (BenchmarkId::EcdsaP256, false) => Ok(Box::new(P256SoftwareRunner::new("signatures"))),
        (BenchmarkId::BitcoinSecp256k1, false) => Ok(Box::new(Secp256k1Runner::new())),
        (BenchmarkId::Taproot, false) => Ok(Box::new(TaprootRunner::new())),
        (BenchmarkId::TlsP256, true) => Ok(Box::new(TlsRunner::new(true)?)),
        (BenchmarkId::TlsP256, false) => Ok(Box::new(TlsRunner::new(false)?)),
        (BenchmarkId::JwtEs256, true) => Ok(Box::new(JwtRunner::new(true)?)),
        (BenchmarkId::JwtEs256, false) => Ok(Box::new(JwtRunner::new(false)?)),
        _ => Err("Hardware acceleration is unavailable for this benchmark".to_string()),
    }
}

fn stop_active_benchmark(active_benchmark: &Arc<Mutex<Option<ActiveBenchmark>>>, status: &str) {
    CURRENT_RUN_ID.fetch_add(1, Ordering::SeqCst);

    if let Some(active) = active_benchmark.lock().expect("active benchmark mutex poisoned").take() {
        active.stop.store(true, Ordering::SeqCst);
    }

    let status = status.to_string();
    queue_with_ui(move |ui| {
        let state = ui.global::<State>();
        state.set_running(false);
        state.set_status_text(status.into());
    });
}

fn hardware_supported(test_id: i32) -> bool {
    matches!(
        BenchmarkId::from_i32(test_id),
        Some(BenchmarkId::Sha256 | BenchmarkId::EcdsaP256 | BenchmarkId::TlsP256 | BenchmarkId::JwtEs256)
    )
}

fn title_for_test(test_id: i32) -> &'static str {
    match BenchmarkId::from_i32(test_id) {
        Some(BenchmarkId::Sha256) => "SHA-256",
        Some(BenchmarkId::EcdsaP256) => "ECDSA (P-256)",
        Some(BenchmarkId::BitcoinSecp256k1) => "Bitcoin ECDSA (secp256k1)",
        Some(BenchmarkId::Taproot) => "Taproot (BIP340)",
        Some(BenchmarkId::TlsP256) => "TLS (P-256 signer path)",
        Some(BenchmarkId::JwtEs256) => "JWT ES256",
        None => "Unknown",
    }
}

fn description_for_test(test_id: i32) -> &'static str {
    match BenchmarkId::from_i32(test_id) {
        Some(BenchmarkId::Sha256) => "Measures raw SHA-256 throughput on alternating buffers.",
        Some(BenchmarkId::EcdsaP256) => "Measures raw P-256 ECDSA signatures per second.",
        Some(BenchmarkId::BitcoinSecp256k1) => {
            "Measures software secp256k1 ECDSA signing, matching Bitcoin message signing primitives."
        }
        Some(BenchmarkId::Taproot) => {
            "Measures software BIP340 Schnorr signatures for Taproot signing workloads."
        }
        Some(BenchmarkId::TlsP256) => {
            "Measures the TLS server-certificate signing path: hash transcript, sign with P-256, encode DER."
        }
        Some(BenchmarkId::JwtEs256) => {
            "Measures end-to-end ES256 JWT signing: build signing input, hash, sign, and base64url-encode."
        }
        None => "Unknown benchmark.",
    }
}

fn queue_with_ui(f: impl FnOnce(&AppWindow) + Send + 'static) {
    slint_keyos_platform::spawn(async move {
        UI.with(|ui| f(ui.get().expect("UI initialized")));
    })
    .detach();
}

fn queue_with_ui_for_run(run_id: u64, f: impl FnOnce(&AppWindow) + Send + 'static) {
    slint_keyos_platform::spawn(async move {
        if CURRENT_RUN_ID.load(Ordering::SeqCst) != run_id {
            return;
        }
        UI.with(|ui| f(ui.get().expect("UI initialized")));
    })
    .detach();
}

fn align_up(value: usize, alignment: usize) -> usize {
    if value == 0 {
        0
    } else {
        ((value + alignment - 1) / alignment) * alignment
    }
}

fn patterned_bytes(seed: u8, len: usize) -> Vec<u8> {
    let mut result = vec![0u8; len];
    let mut value = seed;
    for (index, byte) in result.iter_mut().enumerate() {
        value = value.wrapping_mul(37).wrapping_add(17).wrapping_add((index & 0xff) as u8);
        *byte = value ^ ((index >> 1) as u8);
    }
    result
}

fn digest32(data: &[u8]) -> [u8; 32] { Sha256::digest(data).into() }

fn jwt_signing_input(header: &[u8], payload: &[u8]) -> String {
    format!("{}.{}", URL_SAFE_NO_PAD.encode(header), URL_SAFE_NO_PAD.encode(payload))
}
