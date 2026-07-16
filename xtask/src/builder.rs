// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    fs::{self, File},
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use anyhow::Context;
use app_manifest::Manifest;
use cargo_metadata::semver::Version;

use crate::utils::{Cosign2, GIT_TIMESTAMP};
use crate::xous_arguments::XousArguments;
use crate::{tags, BuildArgs};

/// An override to `.cargo/config.toml`-provided `RUSTFLAGS` for when PIC/PIE is enabled for the compilation.
const RUSTFLAGS_OVERRIDE_PIC: &str =
    "--cfg keyos -C relocation-model=pic -C link-arg=-pie -Z stack-protector=strong -Zunstable-options";
pub(crate) const KEYOS_APPS_DIR: &str = "keyos/apps";
/// Host staging dir (sibling of `keyos/apps`) for the per-app-id built-in icons the image
/// builder copies into `keyos/common/app-icons`. Not part of the on-device tree.
pub(crate) const APP_ICONS_DIR: &str = "keyos/app-icons";
pub(crate) const FLUX_PARENT_APP_DIR: &str = "gui-app-emu-flux";
pub(crate) const FLUX_APPS_DIR: &str = "keyos/apps/gui-app-emu-flux/apps";

static METADATA: LazyLock<cargo_metadata::Metadata> =
    LazyLock::new(|| cargo_metadata::MetadataCommand::new().exec().unwrap());

fn filesystem_app_dir_name(crate_name: &str) -> &str {
    match crate_name {
        "gui-app-emu-flux-server" => FLUX_PARENT_APP_DIR,
        _ => crate_name,
    }
}

#[derive(Debug, Copy, Clone)]
pub enum SigningMode {
    None,
    Developer,

    #[allow(dead_code)]
    Official,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrateSpec {
    /// name of the crate
    Local(String),
    /// crates.io: (name of crate, version)
    CratesIo(String, String),
    /// a prebuilt package: (name of executable, URL for download)
    Prebuilt(String, String),
    /// a prebuilt binary, done using command line tools
    BinaryFile(String),
}
impl CrateSpec {
    pub fn name(&self) -> &str {
        match self {
            CrateSpec::Local(s) => s,
            CrateSpec::CratesIo(n, _v) => n,
            CrateSpec::Prebuilt(n, _u) => n,
            CrateSpec::BinaryFile(path) => path,
        }
    }
}

impl From<&str> for CrateSpec {
    fn from(spec: &str) -> CrateSpec {
        // remote crates are specified as "name@version", i.e. "xous-names@0.9.9"
        if spec.contains('@') {
            let (name, version) = spec.split_once('@').expect("couldn't parse crate specifier");
            CrateSpec::CratesIo(name.to_string(), version.to_string())
        // prebuilt crates are specified as "name#url"
        // i.e. "espeak-embedded#https://ci.betrusted.io/job/espeak-embedded/lastSuccessfulBuild/artifact/target/riscv32imac-unknown-xous-elf/release/"
        } else if spec.contains('#') {
            let (name, url) = spec.split_once('#').expect("couldn't parse crate specifier");
            CrateSpec::Prebuilt(name.to_string(), url.to_string())
        // local files are specified as paths, which, at a minimum include one directory separator "/" or "\"
        // i.e. "./local_file"
        // Note that this is after a test for the '#' character, so that it disambiguate URL slashes
        // It does mean that files with a '#' character in them are mistaken for URL coded paths, and '@' as
        // remote crates.
        } else if spec.contains('/') || spec.contains('\\') {
            CrateSpec::BinaryFile(spec.to_string())
        } else {
            CrateSpec::Local(spec.to_string())
        }
    }
}
impl From<&String> for CrateSpec {
    fn from(value: &String) -> Self { CrateSpec::from(value as &str) }
}

pub(crate) struct Builder {
    loader_features: Vec<String>,
    kernel_features: Vec<String>,
    /// crates that are installed in the xous.img, each one running in its own separate process space
    services: Vec<CrateSpec>,
    /// Apps aren't present in the OS image, instead they reside in the `keyos/apps` folder on the
    /// filesystem. The `gui-app-launcher` service is responsible for locating the apps and running them
    /// on user's demand. Aside from that, the KeyOS kernel treats apps and services identically.
    apps: Vec<CrateSpec>,
    /// Flux apps aren't present in the OS image, instead they reside under
    /// `keyos/apps/gui-app-emu-flux/apps` on the filesystem. App-manager locates and runs them on
    /// demand. The `gui-app-emu-flux` app provides the UI launcher and API server that Flux child apps
    /// use while running. Aside from that, the keyOS kernel treats flux apps, regular apps, and services
    /// identically.
    flux_apps: Vec<CrateSpec>,
    features: Vec<String>,
    target: Option<String>,
    profile: Profile,
    ci: bool,
    reproducible: bool,
}

enum Profile {
    // hw target
    Release,
    Hosted,
}

impl Profile {
    fn as_str(&self) -> &'static str {
        match self {
            Profile::Release => "release",
            Profile::Hosted => "hosted",
        }
    }
}

pub(crate) struct BuildResult {
    target: Option<String>,
    services: Vec<CrateSpec>,
    built_services: Vec<String>,
    built_kernel: String,
    built_loader: Option<String>,
    built_loader_bin: Option<PathBuf>,
    built_xous_img: Option<PathBuf>,
}

impl Builder {
    pub fn new(args: BuildArgs) -> Builder {
        let mut features = Vec::new();
        let mut loader_features = Vec::new();
        let mut kernel_features = Vec::new();

        let target;
        if args.hosted {
            target = None;
            if args.integration_test {
                kernel_features.push("integration-test".into());
            }
        } else {
            target = Some(crate::TARGET_TRIPLE_KEYOS.to_string());

            // Modify the behavior of the gui-server when building the recovery OS image
            if args.is_recovery {
                for service in &["gui-server", "fs-server", "gui-app-control-center"] {
                    if !args.services.contains(&service.to_string()) {
                        panic!("Recovery OS image must include `{}` service", service);
                    }
                }

                // Add recovery-os feature to services to modify their behavior
                features.push("recovery-os".to_string());
            }
        }

        if args.verbose_kernel {
            kernel_features.push("debug-print".into());
        }

        if args.log_serial {
            kernel_features.push("log-serial".into());
        }

        if args.verbose_loader {
            loader_features.push("debug-print".into());
        }

        if args.production_firmware {
            kernel_features.push("production".into());
            features.push("production".into());
        }

        if args.with_systemview {
            kernel_features.push("trace-systemview".into());
        }

        Builder {
            loader_features,
            kernel_features,
            services: args.services.iter().map(CrateSpec::from).collect(),
            apps: args.apps.iter().map(CrateSpec::from).collect(),
            flux_apps: args.flux_apps.iter().map(CrateSpec::from).collect(),
            features,
            target,
            profile: if args.hosted { Profile::Hosted } else { Profile::Release },
            ci: args.ci,
            // production_firmware implies reproducible
            reproducible: args.reproducible || args.production_firmware,
        }
    }

    pub fn hosted() -> Builder {
        Builder::new(BuildArgs {
            services: Vec::new(),
            apps: Vec::new(),
            flux_apps: Vec::new(),
            hosted: true,
            verbose_loader: false,
            verbose_kernel: false,
            log_serial: false,
            log_usb_debug: false,
            log_usb_file: false,
            with_systemview: false,
            integration_test: false,
            is_recovery: false,
            ci: false,
            reproducible: false,
            production_firmware: false,
        })
    }

    pub fn images_path() -> PathBuf {
        let path = "target/armv7a-unknown-xous-elf/release/images";
        fs::create_dir_all(path).unwrap();
        path.parse().unwrap()
    }

    fn get_target_root(&self) -> PathBuf {
        let mut root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| project_root().join("target"));
        root = match self.target {
            Some(ref t) => root.join(t),
            None => root,
        };
        root.join(self.profile.as_str())
    }

    fn get_apps_path(&self) -> PathBuf { self.get_target_root().join(KEYOS_APPS_DIR) }

    fn get_flux_apps_path(&self) -> PathBuf { self.get_target_root().join(FLUX_APPS_DIR) }

    /// Create base cargo command with environment variables
    fn base_cargo_command(&self) -> Command {
        let mut command = Command::new(cargo());
        command.current_dir(project_root());

        // disable incremental compilation for reproducible builds
        if self.reproducible {
            command.env("CARGO_PROFILE_RELEASE_INCREMENTAL", "false");
        }

        command
    }

    /// Build local crates with custom configurations for gui-app packages
    fn build_local_crates(
        &self,
        packages: &[&str],
        features: &Vec<String>,
        target: &Option<&str>,
        target_path: &str,
        is_pic: bool,
    ) -> Vec<String> {
        // for reproducible builds, build each package separately to avoid feature unification
        // https://github.com/rust-lang/cargo/blob/9fa462fe3a81e07e0bfdcc75c29d312c55113ebb/src/doc/src/reference/resolver.md?plain=1#L331
        if self.reproducible && packages.len() > 1 {
            return packages
                .iter()
                .flat_map(|pkg| self.build_local_crates(&[pkg], features, target, target_path, is_pic))
                .collect();
        }

        let mut artifacts = Vec::<String>::new();
        let mut local_args = vec!["build", "--profile", self.profile.as_str()];

        // Set target if specified
        if let Some(t) = target {
            local_args.push("--target");
            local_args.push(t);
        }

        // Add packages and collect declared features
        let mut declared_features = BTreeSet::new();
        for pkg in packages {
            local_args.push("--package");
            local_args.push(pkg);
            artifacts.push(format!("{}/{}", target_path, pkg));
            declared_features.extend(get_package_declared_features(pkg));
        }

        // Add features that are declared
        if !features.is_empty() {
            for feature in features {
                if declared_features.contains(feature) {
                    local_args.push("--features");
                    local_args.push(feature);
                } else {
                    println!("Not using feature '{feature}' for build");
                }
            }
        }

        let mut command = self.base_cargo_command();

        // Apply custom configurations for gui-app packages
        for pkg in packages {
            self.apply_gui_app_config(&mut command, pkg);
        }

        // Override RUSTFLAGS for PIC builds (for keyos builds)
        if is_pic && target.is_some() {
            command.env("RUSTFLAGS", RUSTFLAGS_OVERRIDE_PIC);
        }

        command.env("SOURCE_DATE_EPOCH", GIT_TIMESTAMP.clone());
        command.args(local_args);

        println!("    Command: cargo: {command:?}");

        let status = command.status().expect("Running Cargo failed");
        if !status.success() {
            panic!("Local build failed");
        }

        artifacts
    }

    /// apply custom configurations for gui-app packages
    fn apply_gui_app_config(&self, command: &mut Command, pkg: &str) {
        if !pkg.starts_with("gui-app") {
            return;
        }

        let profile = self.profile.as_str();
        if matches!(self.profile, Profile::Hosted) {
            if self.ci {
                command.env("CARGO_PROFILE_HOSTED_DEBUG", "0").env("CARGO_PROFILE_HOSTED_OPT_LEVEL", "0");
            } else {
                command
                    .args(["--config", &format!("profile.{profile}.package.{pkg}.codegen-units=256")])
                    .args(["--config", &format!("profile.{profile}.package.{pkg}.opt-level=0")])
                    .args(["--config", &format!("profile.{profile}.package.{pkg}.debug=false")]);
            }
        } else {
            let codegen_units = if self.reproducible { 1 } else { 256 };
            command
                .args(["--config", &format!("profile.{profile}.package.{pkg}.codegen-units={codegen_units}")])
                .args(["--config", &format!("profile.{profile}.package.{pkg}.opt-level='s'")])
                .args(["--config", &format!("profile.{profile}.package.{pkg}.debug=false")]);
        }
    }

    /// Build remote crates (from crates.io)
    fn build_remote_crates(
        &self,
        packages: &[(&str, &str)],
        features: &Vec<String>,
        target: &Option<&str>,
        target_path: &str,
    ) -> Vec<String> {
        let mut artifacts = Vec::<String>::new();
        let mut remote_args = vec!["install", "--target-dir", "target"];
        remote_args.push("--root");
        remote_args.push(target_path);

        if let Some(t) = target {
            remote_args.push("--target");
            remote_args.push(t);
        }

        if !features.is_empty() {
            for feature in features {
                remote_args.push("--features");
                remote_args.push(feature);
            }
        }

        for (name, version) in packages {
            // Emit debug info
            print!("    Command: cargo");
            for &arg in remote_args.iter() {
                print!(" {}", arg);
            }
            println!(" {} {}", name, version);

            // Build
            let status = self
                .base_cargo_command()
                .args([&remote_args[..], &[name, "--version", version].to_vec()[..]].concat())
                .status()
                .expect("Running Cargo failed for remote package");
            if !status.success() {
                panic!("Remote build failed");
            }
            artifacts.push(format!("{}bin/{}", target_path, name));
        }

        artifacts
    }

    /// Updated build_crates method that delegates to specialized methods
    fn build_crates(
        &self,
        packages: &[CrateSpec],
        features: &Vec<String>,
        target: &Option<&str>,
        is_pic: bool,
    ) -> Vec<String> {
        let target_path = self.get_target_root().to_string_lossy().into_owned();
        let mut artifacts = Vec::<String>::new();

        let local_pkgs: Vec<&str> = packages
            .iter()
            .filter_map(|pkg| match pkg {
                CrateSpec::Local(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();

        // Build local packages
        if !local_pkgs.is_empty() {
            artifacts.extend(self.build_local_crates(&local_pkgs, features, target, &target_path, is_pic));
        }

        let remote_pkgs: Vec<(&str, &str)> = packages
            .iter()
            .filter_map(|pkg| match pkg {
                CrateSpec::CratesIo(name, version) => Some((name.as_str(), version.as_str())),
                _ => None,
            })
            .collect();

        // Build remote packages
        if !remote_pkgs.is_empty() {
            artifacts.extend(self.build_remote_crates(&remote_pkgs, features, target, &target_path));
        }

        artifacts
    }

    pub fn build_local_crate(&self, crate_name: &str) -> String {
        let target = self.target.as_deref();
        self.build_crates(&[CrateSpec::Local(crate_name.to_string())], &self.features, &target, true)
            .remove(0)
    }

    /// Execute the configured build task. This handles dispatching all configurations,
    /// including renode, hosted, and hardware targets.
    pub fn build(self, signing_mode: SigningMode) -> BuildResult {
        if self.services.is_empty() && self.apps.is_empty() && self.flux_apps.is_empty() {
            panic!("No services were specified. Nothing was built");
        }

        let target = self.target.as_deref();
        // ------ build the services ------

        self.update_nameserver_system_manifests();

        // If we are `cargo xtask run`-ing sandbox test, we need to build the worker first.
        // It does not need to be bundled, it's included as bytes in the test binary.
        if self.services.iter().any(|s| s.name() == "sandbox-test") {
            let worker_artifacts = self.build_crates(
                &[CrateSpec::Local("sandbox-test-worker".to_string())],
                &self.features,
                &target,
                false,
            );
            let worker_elf = &worker_artifacts[0];
            self.strip_elf(worker_elf, &format!("{worker_elf}.strip"));
        }
        let built_services = self.build_crates(&self.services, &self.features, &target, true);

        // ------ build and bundle the filesystem apps ------
        // Hosted apps are raw host binaries launched by path; signing would only
        // wrap them in a cosign2 header that create_process can't spawn.
        let sign_apps = self.target.is_some() && !self.ci;
        // Built-in app icons are staged per-app-id here and copied into keyos/common by the
        // image builder. Wipe once: build_and_bundle_apps runs twice (apps then flux) and both
        // append, so wiping inside it would erase the main apps' icons.
        let app_icons_dir = self.get_target_root().join(APP_ICONS_DIR);
        fs::remove_dir_all(&app_icons_dir).ok();
        let apps_path = self.get_apps_path();
        self.build_and_bundle_apps(&apps_path, &self.apps, sign_apps, signing_mode, &app_icons_dir);

        // ------ build and bundle the filesystem flux ------
        let flux_apps_path = self.get_flux_apps_path();
        self.build_and_bundle_apps(&flux_apps_path, &self.flux_apps, sign_apps, signing_mode, &app_icons_dir);

        // ------ build the kernel ------
        let built_kernel = self
            .build_crates(
                &[CrateSpec::Local("keyos-kernel".to_string())],
                &self.kernel_features,
                &target,
                false,
            )
            .remove(0);
        let mut built_loader = None;
        let mut built_loader_bin = None;
        let mut built_xous_img = None;

        // ------ create kernel + loader + params image ------
        if self.target.is_some() {
            // ------ build the loader ------
            let loader = self
                .build_crates(
                    &[CrateSpec::Local("loader".to_string())],
                    &self.loader_features,
                    &target,
                    false,
                )
                .remove(0);

            // --------- package up and sign a binary image ----------
            let output_bundle = self.create_image(&built_kernel, &built_services);
            println!();
            println!("Kernel+Init bundle is available at {}", output_bundle.display());

            let mut loader_bin = output_bundle.parent().unwrap().to_owned();
            loader_bin.push("loader.bin");
            Command::new("arm-none-eabi-objcopy")
                .current_dir(project_root())
                .args([
                    "-O",
                    "binary",
                    // We want the zeroes in the file, so we don't add them manually later.
                    "--set-section-flags",
                    ".bss=alloc,load,contents",
                    &loader,
                    loader_bin.to_str().unwrap(),
                ])
                .status()
                .unwrap();

            built_loader = Some(loader);
            built_loader_bin = Some(loader_bin);
            built_xous_img = Some(output_bundle);
        }
        let result = BuildResult {
            target: self.target,
            services: self.services,
            built_services,
            built_kernel,
            built_loader,
            built_loader_bin,
            built_xous_img,
        };
        // Emit the hosted-mode services manifest so the SDK packager (and any
        // other `build --hosted` consumer) can stage it for the simulator kernel.
        if result.target.is_none() {
            result.write_hosted_services_manifest();
            crate::bootimage::build_hosted_disk_images();
        }
        result
    }

    fn create_image(&self, kernel: &str, built_services: &[String]) -> PathBuf {
        let mut args = XousArguments::default();

        let kernel = crate::elf::read_program(kernel).expect("unable to read kernel");

        let mut pid = 2;
        assert_eq!(built_services.len(), self.services.len());
        for (service_path, service_desc) in built_services.iter().zip(self.services.iter()) {
            let CrateSpec::Local(service_crate) = service_desc else {
                panic!("Only local services are supported for the initial bundle");
            };
            let program_name = std::path::Path::new(service_path)
                .file_stem()
                .expect("program had no name")
                .to_str()
                .expect("program name is not valid utf-8")
                .to_string();
            let stripped_name = format!("{service_path}.strip");
            let manifest: Manifest = load_manifest(service_crate);
            self.strip_elf(service_path, &stripped_name);
            args.add(tags::BinaryElf::new(
                pid,
                program_name,
                xous::AppId(manifest.app_id),
                std::fs::read(stripped_name).expect("Couldn't read stripped elf file"),
            ));
            if !manifest.memory.is_empty() {
                args.add(tags::MemoryPermission::new(pid, &manifest.memory));
            }
            if !manifest.syscall.is_empty() {
                args.add(tags::SyscallPermission::new(pid, &manifest.syscall));
            }
            pid += 1;
        }

        let xkrn = tags::XousKernel::new(
            kernel.text_offset,
            kernel.text_size,
            kernel.data_offset,
            kernel.data_size,
            kernel.bss_size,
            kernel.entry_point,
            kernel.program,
        );
        args.add(xkrn);

        let output_filename = self.get_target_root().join("xous.img");

        let f = std::fs::File::create(&output_filename).unwrap();
        args.write(&f).expect("Couldn't write to args");
        println!("Kernel arguments: {args}");
        println!("Image created in file {output_filename:?}");

        output_filename
    }

    pub fn build_and_bundle_apps(
        &self,
        apps_dir: &Path,
        apps: &Vec<CrateSpec>,
        sign_apps: bool,
        signing_mode: SigningMode,
        app_icons_dir: &Path,
    ) {
        let apps_dir_str = apps_dir.to_str().unwrap();
        println!("Cleaning `{apps_dir_str:}` directory");
        fs::remove_dir_all(apps_dir).ok();

        println!("Bundling apps to `{apps_dir_str:}`");
        let target = self.target.as_deref();
        let app_bins = self.build_crates(apps, &self.features, &target, true);

        println!("App names: {:#?}", app_bins);

        struct AppInfo {
            app_name: String,
            elf_path: PathBuf,
            manifest_path: PathBuf,
        }
        let mut app_data = vec![];
        for (app_src, app_bin) in apps.iter().zip(app_bins) {
            let app_name = app_src.name().to_string();

            println!("Bundling app {}", app_name);

            let out_elf_dir = apps_dir.join(filesystem_app_dir_name(&app_name));
            fs::create_dir_all(&out_elf_dir).unwrap();

            let elf_path = out_elf_dir.join("app.elf");
            self.strip_elf(&app_bin, elf_path.as_os_str().to_str().unwrap());

            let mut manifest: Manifest = load_manifest(&app_name);
            if matches!(app_src, CrateSpec::Local(_)) {
                let icon_dest = app_icons_dir.join(format!("{}.bin", hex::encode(manifest.app_id)));
                stage_bundled_icon(&app_name, &icon_dest);
            }
            manifest.file_hashes = bundle_file_hashes(&out_elf_dir);
            let manifest_path = out_elf_dir.join("manifest.json");
            serde_json::to_writer(
                fs::File::create(&manifest_path).expect("Couldn't open target manifest file"),
                &manifest,
            )
            .expect("Json serialization failed");

            app_data.push(AppInfo { app_name, elf_path, manifest_path });
        }

        if sign_apps && !matches!(signing_mode, SigningMode::None) {
            let cosign2_config_path = project_root().join("cosign2.toml");
            let cosign2 = Cosign2::new(Some(cosign2_config_path))
                .context("Creating cosign2 command")
                .expect("Could not create cosign2 command");

            // Crate base args that each app will share.
            let mut args = vec!["--in-place"];
            match signing_mode {
                SigningMode::None => panic!("invalid signing mode"),
                SigningMode::Developer => args.push("--developer"),
                SigningMode::Official => {}
            };

            // Sign app.elf and the manifest separately. fileHashes was taken from the unsigned elf,
            // so signing the elf doesn't invalidate it and the two signatures are independent.
            for data in &app_data {
                let app_version = crate_version(&data.app_name).to_string();
                for path in [&data.elf_path, &data.manifest_path] {
                    let mut args = args.clone();
                    let path_str = path.to_str().unwrap();
                    args.extend_from_slice(&["-i", path_str]);
                    args.extend_from_slice(&["--binary-version", &app_version]);

                    println!("Signing `{path_str}` with `cosign2`");
                    let exit_status = cosign2
                        .sign(args)
                        .context("Running cosign2 command")
                        .expect("Could not run cosign2 command");
                    if !exit_status.success() {
                        panic!("Failed to sign {}", data.app_name);
                    }
                }
            }
        } else {
            println!("[!] App signing was skipped");
        }
    }

    fn update_nameserver_system_manifests(&self) {
        let is_recovery = self.features.contains(&String::from("recovery-os"));
        let mut manifests = Vec::new();
        let mut message_names = HashSet::<(String, String)>::new();
        let mut manifest_error = false;
        for service in &self.services {
            let CrateSpec::Local(service) = service else { continue };
            let manifest = load_manifest(&service);
            let app_name = manifest.app_name_en();
            for (server_name, messages) in manifest.servers.iter() {
                for message_name in messages.keys() {
                    if !message_names.insert((server_name.clone(), message_name.clone())) {
                        println!(
                            "[!] Manifest error in {app_name} (0x{}): duplicate message {}:{}",
                            hex::encode(manifest.app_id),
                            server_name,
                            message_name
                        );
                        manifest_error = true;
                    };
                }
            }
            manifests.push(manifest);
        }
        for manifest in &mut manifests {
            let app_name = manifest.app_name_en();

            for (server_name, messages) in manifest.permissions.iter_mut() {
                if server_name == "template" {
                    println!(
                        "[!] Manifest error in {app_name} (0x{}): template(s) {messages:?} do not exist.",
                        hex::encode(manifest.app_id),
                    );
                    manifest_error = true;
                    continue;
                }
                // We need to remove unknown messages from the manifest, or else nameserver will panic on
                // start.
                messages.retain(|message_name| {
                    if !message_names.contains(&(server_name.clone(), message_name.clone())) {
                        if is_recovery {
                            println!(
                                "Manifest warning in {app_name} (0x{}): message {}:{} does not exist. Removing.",
                                hex::encode(manifest.app_id), server_name, message_name
                            );
                        } else {
                            println!(
                                "[!] Manifest error in {app_name} (0x{}): message {}:{} does not exist.",
                                hex::encode(manifest.app_id), server_name, message_name
                            );
                            manifest_error = true;
                        }
                        false
                    } else {
                        true
                    }
                })
            }
        }
        if manifest_error {
            panic!("There were errors in the manifest files");
        }

        let system_manifests_path = get_crate_dir("system-manifests").join("src/system_manifests.rs");
        let mut f = File::create(system_manifests_path).unwrap();
        writeln!(f, "// THIS IS A GENERATED FILE, DO NOT EDIT").unwrap();
        writeln!(f, "// Generated by xtask").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "pub const SYSTEM_MANIFESTS: &[&str] = &[").unwrap();
        for manifest in manifests {
            writeln!(f, "    {:?},", serde_json::to_string(&manifest).expect("Json serialization failed"))
                .unwrap();
        }
        writeln!(f, "];").unwrap();
    }

    /// Strip an ELF with the toolchain matching the build target: the KeyOS cross
    /// strip for hardware, the host strip for hosted binaries.
    fn strip_elf(&self, elf_in_path: &str, stripped_path: &str) {
        println!("Stripping {elf_in_path:}");

        let mut command;
        if self.target.is_some() {
            command = Command::new("arm-none-eabi-strip");
            command.arg("--strip-unneeded");
        } else {
            command = Command::new("strip");
        }
        command.args([elf_in_path, "-o", stripped_path]);

        if !command.status().unwrap().success() {
            panic!("{} failed", command.get_program().to_string_lossy());
        }
    }
}

impl BuildResult {
    /// Write the hosted-mode `services.json` manifest next to the built kernel
    /// and return its path. Shared by `run()` (which then execs the kernel) and
    /// by the hosted `build` path, so the SDK packager can stage the identical
    /// manifest instead of re-deriving syscall masks.
    pub fn write_hosted_services_manifest(&self) -> PathBuf {
        let mut services: Vec<app_manifest::HostedService> = vec![];
        for service in self.built_services.iter() {
            let name = Path::new(service).file_name().unwrap().to_str().unwrap();
            let manifest = load_manifest(name);
            let system = crate::SYSTEM_SERVICES_HOSTED.contains(&name)
                || crate::MANDATORY_SYSTEM_SERVICES_HOSTED.contains(&name);
            // Built binaries carry a `.exe` suffix on Windows.
            let service_path = if cfg!(windows) && !service.ends_with(".exe") {
                format!("{service}.exe")
            } else {
                service.clone()
            };
            if let Some(existing) = services.iter().find(|s| s.app_id == manifest.app_id) {
                let service_a =
                    existing.path.rsplit_once('/').map(|(_, name)| name).unwrap_or(existing.path.as_str());
                let service_b =
                    service_path.rsplit_once('/').map(|(_, name)| name).unwrap_or(service_path.as_str());
                panic!(
                    "Error: Both {} and {} have app ID 0x{}",
                    service_a,
                    service_b,
                    hex::encode(manifest.app_id)
                );
            }
            services.push(app_manifest::HostedService {
                path: service_path,
                app_id: manifest.app_id,
                syscalls: tags::permission::syscall_mask(&manifest.syscall),
                system,
            });
        }

        // Write the manifest next to where cargo actually emits the hosted
        // binaries. A packager (the SDK) overrides CARGO_TARGET_DIR, so the
        // kernel + services land there rather than next to `built_kernel` (which
        // is computed relative to the repo's own target dir).
        let services_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(|dir| PathBuf::from(dir).join("hosted"))
            .unwrap_or_else(|| Path::new(&self.built_kernel).parent().unwrap().to_path_buf());
        std::fs::create_dir_all(&services_dir).ok();
        let services_path = services_dir.join("services.json");
        serde_json::to_writer_pretty(File::create(&services_path).unwrap(), &services).unwrap();
        services_path
    }

    /// Run the built kernel. Can only be called after calling build().
    pub fn run(self, gdb: &str) {
        if self.target.is_none() {
            // hosted mode doesn't specify a cross-compilation target!
            // throw a warning if prebuilts are specified for hosted mode
            for item in &self.services {
                if let CrateSpec::Prebuilt(name, _) = item {
                    println!("Warning! Pre-built binaries not supported for hosted mode ({})", name)
                }
            }
            let services_path = self.write_hosted_services_manifest();

            println!("Starting hosted mode...");
            println!("    Command: {} {}", self.built_kernel, services_path.display());
            // app-manager execs host app binaries from here (mirrors the image /keyos dir).
            let app_elf_root = services_path.parent().unwrap().join("keyos");
            let exec_err = Command::new(self.built_kernel)
                .current_dir(project_root().join("xous/kernel"))
                .env("FOUNDATION_SIMULATOR_APP_ELF_ROOT", app_elf_root)
                .arg(&services_path)
                .exec();
            panic!("Could not execute kernel: {exec_err}");
        } else {
            let loader_elf = self.built_loader.unwrap();
            let kernel_elf = self.built_kernel;
            let os_img = self.built_xous_img.as_ref().unwrap().strip_prefix(project_root()).unwrap();
            let main_service_elf = self.built_services.last().unwrap();
            let loader_size = self.built_loader_bin.unwrap().metadata().unwrap().len() as usize;
            let os_address = keyos::LOADER_CODE_ADDRESS + loader_size;

            let exec_err = Command::new(gdb)
                .current_dir(project_root())
                .args([
                    "-q",
                    &loader_elf,
                    "-ex",
                    &format!("set $KERNEL_ELF=\"{kernel_elf}\""),
                    "-ex",
                    &format!("set $OS_IMG={os_img:?}"),
                    "-ex",
                    &format!("set $SERVICE=\"{main_service_elf}\""),
                    "-ex",
                    &format!("set $OS_ADDRESS={os_address}"),
                    "-x",
                    "scripts/init.gdb",
                ])
                .exec();
            panic!("Could not execute ./debug-loader.sh: {exec_err}");
        };
    }

    /// Additionally runs `join-image` that creates combined loader + kernel + apps image to
    /// be used with `at91bootstrap` bootloader.
    /// Can only be called after build()
    pub fn build_combined_image(self, target_path: &Path, signing_mode: SigningMode, version: &str) {
        if self.target.is_none() {
            // We don't build combined images in hosted mode, so let's noop out.
            return;
        }

        let mut loader_bytes = std::fs::read(self.built_loader_bin.as_ref().unwrap()).unwrap();
        let mut image_bytes = std::fs::read(self.built_xous_img.as_ref().unwrap()).unwrap();
        loader_bytes.append(&mut image_bytes);
        pad_for_sha_dma(&mut loader_bytes);
        std::fs::write(target_path, loader_bytes).unwrap();

        let combined_img_path_str = target_path.to_str().unwrap();

        // Handle unsigned builds early
        if matches!(signing_mode, SigningMode::None) {
            println!("Creating unsigned combined image (no cosign2 signature)");
            return;
        }

        println!("Signing combined image at `{combined_img_path_str}` with cosign2");

        let cosign2_config_path = project_root().join("cosign2.toml");
        let cosign2_config_path_str = cosign2_config_path.to_str().unwrap();

        if let Err(e) = fs::File::open(&cosign2_config_path) {
            eprintln!("Cosign2 config not found at {cosign2_config_path_str}: {}", e);
            panic!("cosign2.toml not found at project root");
        }

        // Verify that cosign2 exists
        if Command::new("cosign2").stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_err() {
            eprintln!("Couldn't run `cosign2`. Is `cosign2` tool installed?");
            eprintln!("Visit https://github.com/Foundation-Devices/cosign2 for more info");
            panic!("cosign2 presence check failed");
        }

        let mut args = match signing_mode {
            SigningMode::None => unreachable!("Already handled above"),
            SigningMode::Developer => vec!["sign", "--developer"],
            SigningMode::Official => vec!["sign"],
        };

        args.extend_from_slice(&["-i", combined_img_path_str]);
        args.extend_from_slice(&["-c", cosign2_config_path_str]);
        args.extend_from_slice(&["--in-place"]);
        args.extend_from_slice(&["--binary-version", &version]);

        if !Command::new("cosign2").args(&args).status().unwrap().success() {
            panic!("cosign2 failed");
        }
    }
}

/// `atsama5d27::dma::set_data_size` asserts that any DMA transfer with more
/// than `0x800000` data units (32 MB at D32 width) is a multiple of
/// `BIG_TRANSFER_CHUNK_SIZE` (1 M data units = 4 MB).
/// The old bootloader hashes the firmware in one shot via DMA; if the
/// binary exceeds 32 MB and isn't 4 MB-aligned, it will boot-loop.
///
/// The padding happens *before* cosign2 signing so it is included in the
/// header's `bin_size` and the bootloader's
/// `binary_bytes.len() == header.bin_size()` check still passes.
fn pad_for_sha_dma(binary: &mut Vec<u8>) {
    const SHA_DMA_SIMPLE_MAX: usize = 0x800000 * 4; // 32 MB
    const SHA_DMA_BIG_ALIGNMENT: usize = 0x100000 * 4; // 4 MB

    if binary.len() <= SHA_DMA_SIMPLE_MAX {
        return;
    }

    println!("Using padding for 32MB+ app image...");

    let aligned = binary.len().next_multiple_of(SHA_DMA_BIG_ALIGNMENT);
    binary.resize(aligned, 0);
}

pub fn cargo() -> String { env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()) }

pub fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR")).ancestors().nth(1).unwrap().to_path_buf()
}

pub fn get_crate_os_deps(crate_name: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut non_binary_crates = HashSet::new();
    let mut crates_to_check = vec![crate_name.to_string()];
    let api_dir = project_root().join("api");
    let os_dir = project_root().join("os");
    while let Some(crate_to_check) = crates_to_check.pop() {
        for dep in &get_package_metadata(&crate_to_check).dependencies {
            if dep.path.as_ref().is_some_and(|d| d.starts_with(&os_dir))
                && !result.contains(&dep.name)
                && !non_binary_crates.contains(&dep.name)
            {
                if is_binary_crate(&dep.name) {
                    result.push(dep.name.clone());
                } else {
                    non_binary_crates.insert(dep.name.clone());
                }
                crates_to_check.push(dep.name.clone());
            }
            // Derive server crate name from the API crate name by stripping
            // `-api` and `-server` suffixes, then appending `-server`.
            // e.g. "camera-api" → "camera-server", "gui-server-api" → "gui-server".
            let mut base = dep.name.as_str();
            loop {
                if let Some(stripped) = base.strip_suffix("-api") {
                    base = stripped;
                } else if let Some(stripped) = base.strip_suffix("-server") {
                    base = stripped;
                } else {
                    break;
                }
            }
            let dep_server_crate = format!("{base}-server");
            if dep.path.as_ref().is_some_and(|d| d.starts_with(&api_dir))
                && !result.contains(&dep_server_crate)
            {
                result.push(dep_server_crate.clone());
                crates_to_check.push(dep_server_crate);
            }
        }
    }
    result
}

pub fn workspace_root() -> &'static cargo_metadata::camino::Utf8Path { &METADATA.workspace_root }

pub fn get_package_metadata(crate_name: &str) -> &'static cargo_metadata::Package {
    METADATA
        .packages
        .iter()
        .find(|p| p.name == crate_name)
        .unwrap_or_else(|| panic!("Could not find crate {crate_name} in cargo metadata"))
}

pub fn is_binary_crate(crate_name: &str) -> bool {
    get_package_metadata(crate_name)
        .targets
        .iter()
        .any(|t| t.name == crate_name && t.kind.iter().any(|k| k == "bin"))
}

pub fn get_crate_dir(crate_name: &str) -> PathBuf {
    get_package_metadata(crate_name).manifest_path.parent().unwrap().to_path_buf().into_std_path_buf()
}

pub fn crate_version(crate_name: &str) -> Version { get_package_metadata(crate_name).version.clone() }

pub fn get_package_declared_features(crate_name: &str) -> Vec<String> {
    get_package_metadata(crate_name).features.keys().map(|k| k.clone()).collect()
}

pub fn load_manifest(crate_name: &str) -> Manifest {
    let manifest = Manifest::load(&get_crate_dir(crate_name), &project_root());
    // fileHashes is generated below from the staged bundle; a hand-written one is rejected
    // rather than silently overwritten.
    assert!(
        manifest.file_hashes.is_empty(),
        "fileHashes is generated by the build; remove it from {crate_name}'s manifest.toml"
    );
    manifest
}

/// Convert a built-in app's source icon into `dest` as a raw `icon.bin`. The source is
/// discovered by the SDK convention `resources/icon.(svg|png)` under the app crate; apps
/// without one ship no icon.
fn stage_bundled_icon(crate_name: &str, dest: &Path) {
    let crate_dir = get_crate_dir(crate_name);
    let Some(source) = ["resources/icon.svg", "resources/icon.png"]
        .into_iter()
        .map(|rel| crate_dir.join(rel))
        .find(|path| path.exists())
    else {
        return;
    };
    let (_, icon_data) = slint_keyos_platform_build::convert_image_to_raw(&source)
        .unwrap_or_else(|e| panic!("Could not convert bundled icon {}: {e}", source.display()));
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("Could not create {}: {e}", parent.display()));
    }
    fs::write(dest, icon_data)
        .unwrap_or_else(|e| panic!("Could not write bundled icon to {}: {e}", dest.display()));
}

/// Hex sha256 of every bundle file except `manifest.json`, keyed by bundle-relative path with
/// forward slashes. The manifest is the signed container, so it never lists its own hash.
fn bundle_file_hashes(bundle_dir: &Path) -> BTreeMap<String, String> {
    use sha2::{Digest, Sha256};

    let mut hashes = BTreeMap::new();
    let mut stack = vec![bundle_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("Couldn't read app bundle directory") {
            let path = entry.expect("Couldn't read app bundle entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(bundle_dir).unwrap().to_str().unwrap().replace('\\', "/");
            if rel == "manifest.json" {
                continue;
            }
            let bytes = fs::read(&path).expect("Couldn't read app bundle file");
            hashes.insert(rel, hex::encode(Sha256::digest(&bytes)));
        }
    }
    hashes
}
