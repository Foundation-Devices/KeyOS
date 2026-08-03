// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use {
    getrandom::getrandom,
    log::info,
    random_tester::{
        ChiSquareCalculation, DynEntropyTester, MeanCalculation, MonteCarloCalculation,
        SerialCorrelationCoefficientCalculation, ShannonCalculation,
    },
    std::io::Write,
    trng::TrngSource,
};

fs::use_api!();

const RNG_SAMPLE_SIZE_KBYTES: usize = 128;

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let trng = trng::Trng::new();

    check_random_generator(|buf| getrandom(bytemuck::cast_slice_mut(buf)).unwrap(), "getrandom");
    check_random_generator(|buf| trng.fill_buf(buf, TrngSource::Combined).unwrap(), "combined");
    check_random_generator(|buf| trng.fill_buf(buf, TrngSource::Mcu).unwrap(), "mcu");
    check_random_generator(|buf| trng.fill_buf(buf, TrngSource::Avalanche).unwrap(), "avalanche");
    check_random_generator(
        |buf| {
            for chunk in buf.chunks_mut(4) {
                let sid = xous::create_server_id().unwrap();
                chunk.copy_from_slice(&sid.to_array());
            }
        },
        "kernel",
    );

    info!("TRNG test finished");
}

fn check_random_generator(fill_buffer: impl FnOnce(&mut [u32]), name: &str) {
    let tests: [(&'static str, Box<dyn DynEntropyTester>, core::ops::Range<f64>); 5] = [
        ("chi_square", Box::new(ChiSquareCalculation::default()), 0.05..1.00),
        ("mean", Box::new(MeanCalculation::default()), 127.0..128.0),
        ("monte_carlo_pi", Box::new(MonteCarloCalculation::default()), 3.1..3.2),
        ("serial_coefficient", Box::new(SerialCorrelationCoefficientCalculation::default()), -0.01..0.01),
        ("shannon_entropy", Box::new(ShannonCalculation::default()), 7.998..8.0),
    ];
    let mut buffer = [0u32; RNG_SAMPLE_SIZE_KBYTES * 1024 / 4];
    println!("Testing {name}:");
    fill_buffer(&mut buffer);
    for (name, mut test, range) in tests {
        test.update(bytemuck::cast_slice(&buffer));
        let result = test.finalize();
        println!("  {name:<20}: {result:>8.3} {}", if range.contains(&result) { "PASS" } else { "FAIL" });
    }

    let filename = format!("/keyos/rnd_{name}.bin");
    let mut f = FileSystem::default()
        .open_file(&filename, fs::Location::System, fs::OpenFlags { read: false, write: true, create: true })
        .unwrap();
    f.write_all(bytemuck::cast_slice(&buffer)).unwrap();
    println!("  Saved to user://{filename} for further analysis");
    println!();
}
