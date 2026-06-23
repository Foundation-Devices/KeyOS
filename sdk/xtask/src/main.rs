// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

mod build;
mod config;
mod package;
mod submodules;
mod util;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process;

use build::{BuildArgs, CheckLayoutArgs, SmokeCheckArgs};
use config::{load, workspace_root, Result};
use package::{FinalizeArgs, PackageArgs};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let root = workspace_root();
    let config = load(&root.join("sdk-build.toml"))?;

    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };

    match command.as_str() {
        "build" => {
            let parsed = BuildArgs::parse(args.collect())?;
            build::run(&root, &config, &parsed)
        }
        "check-layout" => {
            let parsed = CheckLayoutArgs::parse(args.collect())?;
            build::check_layout(&root, &config, &parsed)
        }
        "smoke-check" => {
            let parsed = SmokeCheckArgs::parse(args.collect())?;
            build::smoke_check(&root, &config, &parsed)
        }
        "package" => {
            let parsed = PackageArgs::parse(args.collect())?;
            package::run(&root, &config, &parsed, None, parsed.verbose)
        }
        "finalize" => {
            let parsed = FinalizeArgs::parse(args.collect())?;
            package::finalize(&root, &config, &parsed)
        }
        "clean" => clean(&root),
        "check-submodules" => {
            let mut overrides = BTreeMap::new();
            submodules::apply_env_overrides(&config, &mut overrides);
            submodules::check_all(&root, &config, &overrides)?;
            println!("submodules OK");
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        other => Err(config::boxed_err(format!("unknown xtask command: {other}"))),
    }
}

fn clean(root: &Path) -> Result<()> {
    remove_if_exists(&root.join("dist"))?;
    remove_if_exists(&root.join("target"))?;
    println!("removed dist/ and target/");
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
Foundation SDK xtask

Commands:
  build [OPTIONS]
  check-layout [OPTIONS]
  smoke-check [OPTIONS]
  package [OPTIONS]
  finalize [OPTIONS]
  clean
  check-submodules

Examples:
  cargo xtask check-layout
  cargo xtask smoke-check
  cargo xtask build --target all --release
  cargo xtask build --target aarch64-apple-darwin --release --package
  cargo xtask package --target all
  cargo xtask finalize
  cargo xtask check-submodules
"
    );
}
