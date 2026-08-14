// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    ffi::OsStr,
    io::{self},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::LazyLock,
};

pub static GIT_TIMESTAMP: LazyLock<String> = LazyLock::new(|| {
    if let Ok(source_date_epoch) = std::env::var("KEYOS_SOURCE_DATE_EPOCH") {
        source_date_epoch.parse::<u64>().expect("KEYOS_SOURCE_DATE_EPOCH must be an unsigned integer");
        return source_date_epoch;
    }

    let git_timestamp = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=%ct"])
        .output()
        .expect("Could not get last commit date");
    if !git_timestamp.status.success() {
        panic!("Git log unsuccesful: {:?}", git_timestamp);
    }
    String::from_utf8(git_timestamp.stdout.trim_ascii().to_vec()).unwrap()
});

fn get_target_toolchain(target: Option<&str>) -> (bool, String, Option<PathBuf>, Option<PathBuf>) {
    let mut args = vec!["--print", "sysroot"];
    if let Some(target) = target {
        args.push("--target");
        args.push(target);
    }

    let sysroot_cmd = Command::new("rustc")
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .args(&args)
        .spawn()
        .expect("could not run rustc");
    let sysroot_output = sysroot_cmd.wait_with_output().unwrap();
    let have_toolchain = sysroot_output.status.success();

    let toolchain_path = String::from_utf8(sysroot_output.stdout).unwrap().trim().to_owned();

    let target_path = target.map(|target| {
        let mut target_path = PathBuf::from(&toolchain_path);
        target_path.push("lib");
        target_path.push("rustlib");
        target_path.push(target);

        target_path
    });

    let version_path = target_path.as_ref().cloned().map(|mut target_path| {
        target_path.push("RUST_VERSION");
        target_path
    });

    (have_toolchain, toolchain_path, target_path, version_path)
}

pub(crate) fn is_target_installed(target: &str) -> bool {
    let (is_installed, _, target_path, _) = get_target_toolchain(Some(target));
    is_installed && target_path.map(|pb| pb.exists()).unwrap_or(false)
}

pub(crate) struct Cosign2 {
    config_path: Option<PathBuf>,
}

impl Cosign2 {
    pub(crate) fn new(config_path: Option<PathBuf>) -> io::Result<Self> {
        // Make sure that the cosign2 can be executed.
        Command::new("cosign2").stdout(Stdio::null()).stderr(Stdio::null()).status().inspect_err(|_| {
            eprintln!("Couldn't run `cosign2`. Is `cosign2` tool installed?");
            eprintln!("Visit https://github.com/Foundation-Devices/cosign2 for more info");
        })?;
        if let Some(ref config_path) = config_path {
            let config_path_str = config_path.to_str().expect("Path should be convertible to str");
            if !std::fs::exists(config_path_str)? {
                eprintln!("Could not find `cosign2` config at {config_path_str}");
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Could not find `cosign2` config at {config_path_str}",
                ));
            }
        }

        Ok(Self { config_path })
    }

    pub(crate) fn sign<A>(&self, args: A) -> io::Result<ExitStatus>
    where
        A: IntoIterator,
        A::Item: AsRef<OsStr>,
    {
        self.run(Cosign2Cmd::Sign, args)
    }

    #[allow(dead_code)]
    pub(crate) fn dump<A>(&self, args: A) -> io::Result<ExitStatus>
    where
        A: IntoIterator,
        A::Item: AsRef<OsStr>,
    {
        self.run(Cosign2Cmd::Dump, args)
    }

    pub(crate) fn run<A>(&self, cmd: Cosign2Cmd, args: A) -> io::Result<ExitStatus>
    where
        A: IntoIterator,
        A::Item: AsRef<OsStr>,
    {
        let mut command = Command::new("cosign2");
        let command = command.arg(&cmd.to_string()).args(args);

        let command = if let Some(ref config_path) = self.config_path {
            let config_path_str = config_path.to_str().expect("Path should be convertible to str");
            command.args(["-c", config_path_str])
        } else {
            command
        };

        command.status()
    }
}

pub(crate) enum Cosign2Cmd {
    Sign,
    Dump,
}

impl std::fmt::Display for Cosign2Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cmd = match self {
            Cosign2Cmd::Sign => "sign",
            Cosign2Cmd::Dump => "dump",
        };
        write!(f, "{}", cmd)
    }
}
