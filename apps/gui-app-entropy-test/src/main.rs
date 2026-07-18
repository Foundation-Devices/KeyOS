// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Entropy Test — reads samples from a chosen TRNG source and runs a battery
//! of randomness checks (bit distribution, stuck/biased bits, byte uniformity,
//! runs, serial correlation, Shannon/min entropy).
//!
//! Statistics are accumulated incrementally so memory stays O(1) regardless of
//! the configured iteration count.

use core::fmt::Write as _;
use std::{
    cell::OnceCell,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use slint_keyos_platform::{app, gui_server_api::InputMessage};
use trng::{Trng, TrngSource};

/// Samples requested per `fill_buf` call. 1024 u32 == 4 KiB == one page (the TRNG
/// server maps a page-aligned region per call, so a page-sized batch is ideal).
const BATCH_SAMPLES: usize = 1024;
/// How often the live progress counter is pushed to the UI.
const UI_UPDATE_INTERVAL: Duration = Duration::from_millis(150);
/// p-value threshold below which a statistical test is reported as a failure
/// (the conventional NIST SP 800-22 significance level).
const SIGNIFICANCE: f64 = 0.01;
/// |z| above which a single bit position is flagged as strongly biased.
const BIAS_Z: f64 = 5.0;

/// Generation counter: bumped on every start/stop so stale worker updates from a
/// superseded run can be discarded.
static CURRENT_RUN_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static UI: OnceCell<AppWindow> = OnceCell::new();
}

app!("Entropy Test");

struct ActiveTest {
    run_id: u64,
    stop: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum TestSource {
    Mcu,
    Avalanche,
    Combined,
    AvalancheRaw,
}

impl TestSource {
    fn trng_source(self) -> Option<TrngSource> {
        match self {
            TestSource::Mcu => Some(TrngSource::Mcu),
            TestSource::Avalanche => Some(TrngSource::Avalanche),
            TestSource::Combined => Some(TrngSource::Combined),
            TestSource::AvalancheRaw => None,
        }
    }

    fn sample_bits(self) -> usize {
        match self {
            TestSource::AvalancheRaw => 12,
            _ => 32,
        }
    }

    fn is_raw_adc(self) -> bool { matches!(self, TestSource::AvalancheRaw) }
}

fn source_from_index(index: i32) -> TestSource {
    // Must match the SegmentedControl labels order in ui/app.slint:
    // 0 = "MCU", 1 = "Avalanche", 2 = "Combined", 3 = "Raw ADC".
    match index {
        0 => TestSource::Mcu,
        1 => TestSource::Avalanche,
        3 => TestSource::AvalancheRaw,
        _ => TestSource::Combined,
    }
}

fn source_name(source: TestSource) -> &'static str {
    match source {
        TestSource::Mcu => "MCU hardware TRNG",
        TestSource::Avalanche => "Avalanche (SHA-256 conditioned)",
        TestSource::Combined => "Combined (MCU XOR SHA-256 conditioned avalanche)",
        TestSource::AvalancheRaw => "Avalanche raw 12-bit ADC",
    }
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Incremental statistics over a stream of source samples.
struct Stats {
    samples: u64,
    sample_bits: usize,
    out_of_range: u64,
    /// Total number of 1 bits seen (for the monobit / frequency test).
    ones: u64,
    /// Number of 1 bits at each active sample-bit position (LSB = index 0).
    bitpos_ones: [u64; 32],
    /// Distribution of bytes for 32-bit sources, or 12-bit values for raw ADC.
    symbol_hist: Vec<u64>,
    symbols: u64,
    // Serial (lag-1) correlation accumulators over the symbol stream (ENT-style,
    // cyclic). A symbol is a byte except in raw ADC mode, where it is one sample.
    sum1: f64,
    sum2: f64,
    sumprod: f64,
    first_symbol: Option<u32>,
    last_symbol: Option<u32>,
    // Runs test: count of adjacent-bit transitions across the whole bit stream
    // (bits taken LSB-first within each sample). Runs = transitions + 1.
    transitions: u64,
    last_bit: Option<u8>,
    min_sample: u32,
    max_sample: u32,
}

impl Stats {
    fn new(source: TestSource) -> Self {
        let sample_bits = source.sample_bits();
        Self {
            samples: 0,
            sample_bits,
            out_of_range: 0,
            ones: 0,
            bitpos_ones: [0; 32],
            symbol_hist: vec![0; if source.is_raw_adc() { 4096 } else { 256 }],
            symbols: 0,
            sum1: 0.0,
            sum2: 0.0,
            sumprod: 0.0,
            first_symbol: None,
            last_symbol: None,
            transitions: 0,
            last_bit: None,
            min_sample: u32::MAX,
            max_sample: 0,
        }
    }

    fn update(&mut self, samples: &[u32]) {
        let mask = if self.sample_bits == 32 { u32::MAX } else { (1 << self.sample_bits) - 1 };
        for &sample in samples {
            if sample & !mask != 0 {
                self.out_of_range += 1;
            }
            let sample = sample & mask;
            self.samples += 1;
            self.ones += sample.count_ones() as u64;
            self.min_sample = self.min_sample.min(sample);
            self.max_sample = self.max_sample.max(sample);

            for p in 0..self.sample_bits {
                let bit = ((sample >> p) & 1) as u8;
                self.bitpos_ones[p] += bit as u64;
                if let Some(previous) = self.last_bit {
                    if bit != previous {
                        self.transitions += 1;
                    }
                }
                self.last_bit = Some(bit);
            }

            if self.sample_bits == 12 {
                self.observe_symbol(sample);
            } else {
                for byte in sample.to_le_bytes() {
                    self.observe_symbol(byte as u32);
                }
            }
        }
    }

    fn observe_symbol(&mut self, symbol: u32) {
        self.symbol_hist[symbol as usize] += 1;
        self.symbols += 1;

        let value = symbol as f64;
        self.sum1 += value;
        self.sum2 += value * value;
        if let Some(previous) = self.last_symbol {
            self.sumprod += previous as f64 * value;
        } else {
            self.first_symbol = Some(symbol);
        }
        self.last_symbol = Some(symbol);
    }

    fn finalize(&self, source: TestSource, simulated: bool) -> String {
        let samples = self.samples;
        let total_bits = samples * self.sample_bits as u64;
        let mut out = String::new();

        let _ = writeln!(out, "Source: {}", source_name(source));
        if source.is_raw_adc() {
            let _ = writeln!(out, "Samples: {} raw 12-bit ADC readings", samples);
        } else {
            let _ = writeln!(out, "Samples: {} 32-bit words ({} bytes)", samples, samples * 4);
        }
        if simulated {
            let _ = writeln!(
                out,
                "\nNOTE: running in the simulator — all sources share a deterministic PRNG, so results are NOT real entropy and per-source differences are not meaningful."
            );
        }
        if samples == 0 {
            return out;
        }

        let samples_f = samples as f64;
        let n = total_bits as f64;

        // 1. Monobit / frequency test.
        let ones = self.ones as f64;
        let prop = ones / n;
        let s_obs = (2.0 * ones - n).abs() / n.sqrt();
        let p_mono = erfc(s_obs / core::f64::consts::SQRT_2);
        let _ = writeln!(
            out,
            "\n1. Monobit frequency\n   ones = {:.3} %   p = {:.4}   [{}]",
            prop * 100.0,
            p_mono,
            if source.is_raw_adc() { "RAW DIAGNOSTIC" } else { verdict(p_mono) }
        );

        // 2. Per-position stuck / biased bits over the active sample width.
        let mut stuck: Vec<usize> = Vec::new();
        let mut biased: Vec<usize> = Vec::new();
        let mut worst_pos = 0usize;
        let mut worst_dev = 0.0f64;
        let mut worst_pct = 50.0f64;
        let mut per_bit_details = String::new();
        for p in 0..self.sample_bits {
            let o = self.bitpos_ones[p];
            let pct = o as f64 / samples_f * 100.0;
            let z = (o as f64 - samples_f / 2.0) / (samples_f / 4.0).sqrt();
            let marker = if o == 0 || o == samples {
                stuck.push(p);
                " STUCK"
            } else if z.abs() > BIAS_Z {
                biased.push(p);
                " BIAS"
            } else {
                ""
            };
            if (pct - 50.0).abs() > worst_dev {
                worst_dev = (pct - 50.0).abs();
                worst_pos = p;
                worst_pct = pct;
            }
            let _ = writeln!(per_bit_details, "   bit {p:02}: {o:>10} ones  {pct:>8.4}%  z={z:+8.3}{marker}");
        }
        let per_bit_ok = stuck.is_empty() && biased.is_empty();
        let per_bit_label = if source.is_raw_adc() {
            "RAW DIAGNOSTIC"
        } else if per_bit_ok {
            "PASS"
        } else {
            "FAIL"
        };
        let _ =
            writeln!(out, "\n2. Per-bit ({} active bits, LSB = bit 0)   [{per_bit_label}]", self.sample_bits);
        out.push_str(&per_bit_details);
        if stuck.is_empty() {
            let _ = writeln!(out, "   stuck bits: none");
        } else {
            let _ = writeln!(out, "   STUCK bits: {}", join_positions(&stuck));
        }
        if biased.is_empty() {
            let _ = writeln!(out, "   strongly biased (|z|>{:.0}): none", BIAS_Z);
        } else {
            let _ = writeln!(out, "   strongly biased (|z|>{:.0}): {}", BIAS_Z, join_positions(&biased));
        }
        let _ = writeln!(out, "   worst bit: #{} at {:.2} % ones", worst_pos, worst_pct);

        if source.is_raw_adc() {
            let (mode, max_count) = self
                .symbol_hist
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|(_, count)| *count)
                .unwrap_or((0, 0));
            let mean = self.sum1 / self.symbols as f64;
            let mcv_min_entropy = -(max_count as f64 / self.symbols as f64).log2();
            let _ = writeln!(
                out,
                "\n3. Raw ADC distribution\n   range = {}..{}   mean = {:.3}\n   most common = {} ({} samples, {:.4}%)\n   naive MCV min-entropy = {:.4} bits/sample",
                self.min_sample,
                self.max_sample,
                mean,
                mode,
                max_count,
                max_count as f64 / self.symbols as f64 * 100.0,
                mcv_min_entropy
            );
            let _ = writeln!(
                out,
                "   NOTE: MCV is descriptive only; use a raw capture with the full SP 800-90B estimator suite for formal assessment."
            );
            if self.out_of_range > 0 {
                let _ = writeln!(out, "   ERROR: {} samples had bits set above bit 11", self.out_of_range);
            }

            self.write_runs(&mut out, n, prop, 4, false);
            self.write_serial(&mut out, 5, "raw samples", false);
            return out;
        }

        let total_bytes = self.symbols;

        // 3. Byte-value uniformity (chi-square, 255 d.o.f.).
        let expected = total_bytes as f64 / 256.0;
        let mut chi2 = 0.0f64;
        for &h in self.symbol_hist.iter() {
            let d = h as f64 - expected;
            chi2 += d * d / expected;
        }
        // df = 255: mean 255, std = sqrt(2*255) ≈ 22.58.
        let chi_z = (chi2 - 255.0) / (2.0 * 255.0f64).sqrt();
        let _ = writeln!(
            out,
            "\n3. Byte uniformity (chi-square)\n   chi2 = {:.1}  (df 255, expect 255 ± 22.6, z = {:+.2})   [{}]",
            chi2,
            chi_z,
            if chi_z.abs() <= 4.0 { "PASS" } else { "FAIL" }
        );

        // 4. Runs test.
        self.write_runs(&mut out, n, prop, 4, true);

        // 5. Serial (lag-1) correlation over bytes (ENT-style, cyclic).
        self.write_serial(&mut out, 5, "bytes", true);

        // 6. Shannon and min-entropy per byte.
        let mut shannon = 0.0f64;
        let mut max_count = 0u64;
        for &h in self.symbol_hist.iter() {
            if h == 0 {
                continue;
            }
            if h > max_count {
                max_count = h;
            }
            let p = h as f64 / total_bytes as f64;
            shannon -= p * p.log2();
        }
        let min_entropy = -(max_count as f64 / total_bytes as f64).log2();
        // Coarse check: per-byte entropy near the 8.0 ideal. These metrics move
        // little under a small bias (sections 1-4 are the sensitive detectors),
        // and the min-entropy estimate is naturally lower for small samples - so
        // this verdict is only meaningful for large runs (>= ~100k samples).
        let entropy_ok = shannon >= 7.9 && min_entropy >= 7.5;
        let entropy_label = if samples < 100_000 {
            "N/A"
        } else if entropy_ok {
            "PASS"
        } else {
            "FAIL"
        };
        let _ = writeln!(
            out,
            "\n6. Entropy per byte (ideal 8.0)   [{}]\n   Shannon = {:.4}   min-entropy = {:.4}",
            entropy_label, shannon, min_entropy
        );

        out
    }

    fn write_runs(&self, out: &mut String, n: f64, prop: f64, section: usize, verdict_enabled: bool) {
        let tau = 2.0 / n.sqrt();
        if (prop - 0.5).abs() >= tau {
            let _ = writeln!(out, "\n{section}. Runs\n   n/a (monobit proportion too far from 0.5)");
            return;
        }

        let observed = self.transitions as f64 + 1.0;
        let expected = 2.0 * n * prop * (1.0 - prop);
        let denominator = 2.0 * (2.0 * n).sqrt() * prop * (1.0 - prop);
        let p = if denominator > 0.0 { erfc((observed - expected).abs() / denominator) } else { 0.0 };
        let label = if verdict_enabled { verdict(p) } else { "RAW DIAGNOSTIC" };
        let _ = writeln!(
            out,
            "\n{section}. Runs\n   runs = {}  expected ~= {:.0}   p = {:.4}   [{}]",
            observed as u64, expected, p, label
        );
    }

    fn write_serial(&self, out: &mut String, section: usize, unit: &str, verdict_enabled: bool) {
        let count = self.symbols as f64;
        let cyclic_product = self.sumprod
            + match (self.first_symbol, self.last_symbol) {
                (Some(first), Some(last)) => first as f64 * last as f64,
                _ => 0.0,
            };
        let denominator = count * self.sum2 - self.sum1 * self.sum1;
        if denominator.abs() < f64::EPSILON {
            let _ = writeln!(
                out,
                "\n{section}. Serial correlation ({unit})\n   undefined (constant data - likely stuck source)"
            );
            return;
        }

        let correlation = (count * cyclic_product - self.sum1 * self.sum1) / denominator;
        // Under H0 the coefficient has standard error 1/sqrt(n), so evaluate a
        // sample-size-adjusted z score instead of applying a fixed |r| cutoff.
        let (z, p) = serial_significance(correlation, count);
        let label = if verdict_enabled { verdict(p) } else { "RAW DIAGNOSTIC" };
        let _ = writeln!(
            out,
            "\n{section}. Serial correlation ({unit}, lag 1)\n   r = {correlation:+.5}  z = {z:.2}  p = {p:.4}   [{label}]"
        );
    }
}

fn serial_significance(correlation: f64, sample_count: f64) -> (f64, f64) {
    let z = correlation.abs() * sample_count.sqrt();
    (z, erfc(z / core::f64::consts::SQRT_2))
}

fn verdict(p: f64) -> &'static str {
    if p >= SIGNIFICANCE {
        "PASS"
    } else {
        "FAIL"
    }
}

fn join_positions(positions: &[usize]) -> String {
    let mut s = String::new();
    for (i, p) in positions.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "#{p}");
    }
    s
}

/// erf via the Abramowitz & Stegun 7.1.26 approximation (|error| < 1.5e-7).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

fn erfc(x: f64) -> f64 { 1.0 - erf(x) }

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("Starting Entropy Test");

    UI.with(|slot| {
        slot.set(ui.clone_strong()).ok();
    });

    let active: Arc<Mutex<Option<ActiveTest>>> = Arc::new(Mutex::new(None));

    // Stop the test if the app is backgrounded.
    cx.set_input_handler({
        let active = active.clone();
        move |input| {
            if input.msg == InputMessage::Hidden {
                stop_active_test(&active, "Stopped");
            }
        }
    });

    ui.global::<Callbacks>().on_start_test({
        let active = active.clone();
        move |source_index, text| {
            let source = source_from_index(source_index);

            // The text is a count in thousands of samples; tolerate stray characters.
            let thousands: u64 =
                text.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
            let total = thousands.saturating_mul(1000).min(100_000_000) as usize;
            if total == 0 {
                queue_with_ui(|ui| {
                    let state = ui.global::<State>();
                    state.set_running(false);
                    state.set_status_text("Enter a positive iteration count".into());
                });
                return;
            }

            let run_id = CURRENT_RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;
            let stop = Arc::new(AtomicBool::new(false));

            if let Some(previous) = active
                .lock()
                .expect("active test mutex poisoned")
                .replace(ActiveTest { run_id, stop: stop.clone() })
            {
                previous.stop.store(true, Ordering::SeqCst);
            }

            let total_i = total as i32;
            queue_with_ui(move |ui| {
                let state = ui.global::<State>();
                state.set_running(true);
                state.set_total(total_i);
                state.set_progress(0);
                state.set_status_text("Running…".into());
                state.set_results_text("Collecting samples…".into());
            });

            let active = active.clone();
            std::thread::spawn(move || run_test_thread(active, run_id, source, total, stop));
        }
    });

    ui.global::<Callbacks>().on_stop_test({
        let active = active.clone();
        move || stop_active_test(&active, "Stopped")
    });

    ui.run().expect("UI running");
}

fn run_test_thread(
    active: Arc<Mutex<Option<ActiveTest>>>,
    run_id: u64,
    source: TestSource,
    total: usize,
    stop: Arc<AtomicBool>,
) {
    let outcome = run_test(run_id, source, total, &stop);

    if CURRENT_RUN_ID.load(Ordering::SeqCst) != run_id {
        return; // superseded by a newer run
    }

    {
        let mut guard = active.lock().expect("active test mutex poisoned");
        if guard.as_ref().map(|a| a.run_id) == Some(run_id) {
            guard.take();
        }
    }

    match outcome {
        Ok((stats, stopped)) => {
            let report = stats.finalize(source, !cfg!(keyos));
            let samples = stats.samples;
            let progress = samples as i32;
            let status = if stopped {
                format!("Stopped at {samples} samples")
            } else {
                format!("Done - {samples} samples")
            };
            queue_with_ui_for_run(run_id, move |ui| {
                let state = ui.global::<State>();
                state.set_running(false);
                state.set_progress(progress);
                state.set_status_text(status.into());
                state.set_results_text(report.into());
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

fn run_test(
    run_id: u64,
    source: TestSource,
    total: usize,
    stop: &AtomicBool,
) -> Result<(Stats, bool), String> {
    let trng = Trng::new().map_err(|e| format!("TRNG init failed: {e:?}"))?;
    let mut stats = Stats::new(source);
    let mut buf = [0u32; BATCH_SAMPLES];
    let mut done = 0usize;
    let mut last_ui = Instant::now();
    let mut stopped = false;

    while done < total {
        if stop.load(Ordering::SeqCst) {
            stopped = true;
            break;
        }
        let n = (total - done).min(BATCH_SAMPLES);
        let slice = &mut buf[..n];
        if source.is_raw_adc() {
            trng.fill_avalanche_raw_samples(slice)
                .map_err(|e| format!("fill raw avalanche failed: {e:?}"))?;
        } else {
            trng.fill_buf(slice, source.trng_source().expect("normal source"))
                .map_err(|e| format!("fill_buf failed: {e:?}"))?;
        }
        stats.update(slice);
        done += n;

        if last_ui.elapsed() >= UI_UPDATE_INTERVAL {
            last_ui = Instant::now();
            let progress = done as i32;
            queue_with_ui_for_run(run_id, move |ui| {
                ui.global::<State>().set_progress(progress);
            });
        }
    }

    Ok((stats, stopped))
}

fn stop_active_test(active: &Arc<Mutex<Option<ActiveTest>>>, status: &str) {
    CURRENT_RUN_ID.fetch_add(1, Ordering::SeqCst);

    if let Some(active) = active.lock().expect("active test mutex poisoned").take() {
        active.stop.store(true, Ordering::SeqCst);
    }

    let status = status.to_string();
    queue_with_ui(move |ui| {
        let state = ui.global::<State>();
        state.set_running(false);
        state.set_status_text(status.into());
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_verdict_scales_with_sample_count() {
        let (_, small_sample_p) = serial_significance(0.02, 100.0);
        let (_, large_sample_p) = serial_significance(0.02, 1_000_000.0);

        assert_eq!(verdict(small_sample_p), "PASS");
        assert_eq!(verdict(large_sample_p), "FAIL");
    }

    #[test]
    fn small_sample_entropy_verdict_is_not_applicable() {
        let mut stats = Stats::new(TestSource::Mcu);
        stats.update(&[0; 32]);

        let report = stats.finalize(TestSource::Mcu, false);
        assert!(report.contains("Entropy per byte (ideal 8.0)   [N/A]"));
    }
}
