// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    fs::{self, File},
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use anyhow::Context;
use app_manifest::{ApprovalBehavior, Manifest, RequiredSignature};

/// How a server message may be granted: its required signature and approval behaviour.
type MessagePolicy = (RequiredSignature, ApprovalBehavior);
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
/// Host dir the sideload archives' bundles are built in, wiped per build and reported by
/// print-hashes. Separate from `app-bundles`, where `build-app` leaves developer bundles that
/// are not release artifacts.
pub(crate) const SIDELOAD_BUNDLES_DIR: &str = "sideload-bundles";
/// Unsigned counterpart produced for the external production signing pipeline.
pub(crate) const UNSIGNED_SIDELOAD_BUNDLES_DIR: &str = "sideload-bundles-unsigned";

static METADATA: LazyLock<cargo_metadata::Metadata> =
    LazyLock::new(|| cargo_metadata::MetadataCommand::new().exec().unwrap());

#[derive(Debug, Copy, Clone)]
pub enum SigningMode {
    None,
    Developer,
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
    features: Vec<String>,
    target: Option<String>,
    profile: Profile,
    ci: bool,
    reproducible: bool,
    keyos_version: Version,
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
    keyos_version: Version,
}

impl Builder {
    pub fn new(args: BuildArgs) -> Builder {
        args.require_production_keyos_version();
        let keyos_version = args.keyos_version.clone().unwrap_or_else(|| {
            Version::parse(crate::KEYOS_VERSION).expect("KEYOS_VERSION must be valid SemVer")
        });
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
            features,
            target,
            profile: if args.hosted { Profile::Hosted } else { Profile::Release },
            ci: args.ci,
            // production_firmware implies reproducible
            reproducible: args.reproducible || args.production_firmware,
            keyos_version,
        }
    }

    pub fn hosted() -> Builder { Builder::new(BuildArgs { hosted: true, ..Default::default() }) }

    /// A builder for the hardware target with no services/apps queued: for one-off single-crate
    /// builds like `build_app`, which drive `build_local_crate` directly.
    pub fn hardware() -> Builder { Builder::new(BuildArgs::default()) }

    pub fn images_path() -> PathBuf {
        let path = "target/armv7a-unknown-xous-elf/release/images";
        fs::create_dir_all(path).unwrap();
        path.parse().unwrap()
    }

    pub(crate) fn get_target_root(&self) -> PathBuf {
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
        if self.services.is_empty() && self.apps.is_empty() {
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
        // image builder.
        let app_icons_dir = self.get_target_root().join(APP_ICONS_DIR);
        fs::remove_dir_all(&app_icons_dir).ok();
        self.build_and_bundle_apps(
            &self.get_apps_path(),
            &self.apps,
            sign_apps,
            signing_mode,
            &app_icons_dir,
        );

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
            keyos_version: self.keyos_version,
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

        let mut app_data = vec![];
        for (app_src, app_bin) in apps.iter().zip(app_bins) {
            let app_name = app_src.name().to_string();
            println!("Bundling app {}", app_name);
            let bundle_dir = apps_dir.join(&app_name);
            // Built-ins stage their icons in the shared app-icons dir as <app-id>[-dark].bin (the
            // device reads them from CommonAssets); only local crates ship one, prebuilt crates none.
            let icon_dest = matches!(app_src, CrateSpec::Local(_))
                .then(|| app_icons_dir.join(format!("{}.bin", hex::encode(load_manifest(&app_name).app_id))));
            app_data.push(self.bundle_app(&app_name, &app_bin, &bundle_dir, icon_dest.as_deref()));
        }

        if sign_apps && !matches!(signing_mode, SigningMode::None) {
            let cosign2 = Cosign2::new(Some(project_root().join("cosign2.toml")))
                .context("Creating cosign2 command")
                .expect("Could not create cosign2 command");
            for data in &app_data {
                sign_bundle(&cosign2, signing_mode, data);
            }
        } else {
            println!("[!] App signing was skipped");
        }
    }

    /// Build one app crate into a signed, sideloadable bundle: a directory named by the app id
    /// holding `app.elf`, `manifest.json` (with `fileHashes`), and `icon[-dark].bin` for the icons
    /// the crate ships. The trust class follows from the key: no `cosign2_config` means the repo
    /// `cosign2.toml`, whose developer signature the firmware build trusts as Foundation, so the
    /// bundle validates its permissions at Foundation level; an explicit key is a third-party
    /// publisher and validates as such. `SigningMode::None` or a hosted build leaves the bundle
    /// unsigned. `sideload_deps` are the other sideloadable app crates of the target image, whose
    /// servers this bundle may declare permissions on; built-ins must not depend on a sideload,
    /// so only sideloaded bundles are built through here.
    pub fn build_app(
        &self,
        app_name: &str,
        out: &Path,
        cosign2_config: Option<PathBuf>,
        signing_mode: SigningMode,
        sideload_deps: &[&str],
    ) -> BundledApp {
        let signature = if cosign2_config.is_some() {
            RequiredSignature::ThirdParty
        } else {
            RequiredSignature::Foundation
        };
        let mut manifest = load_manifest(app_name);
        // A standalone bundle is built outside an image build, so the message lookup is rebuilt
        // from the canonical service and app set of the image the bundle targets.
        let service_crates: &[&[&str]] = if matches!(self.profile, Profile::Hosted) {
            &[
                crate::MANDATORY_SYSTEM_SERVICES_HOSTED,
                crate::SYSTEM_SERVICES_HOSTED,
                crate::DEFAULT_SERVICES_HOSTED,
            ]
        } else {
            &[crate::MANDATORY_SYSTEM_SERVICES_HW, crate::DEFAULT_SERVICES_NORMAL]
        };
        let mut message_names = HashMap::new();
        for crate_name in
            service_crates.iter().copied().flatten().chain(crate::DEFAULT_APPS_NORMAL).chain(sideload_deps)
        {
            for (server_name, messages) in load_manifest(crate_name).servers {
                for (message_name, message) in messages {
                    message_names.insert(
                        (server_name.clone(), message_name),
                        (message.required_signature(), message.approval),
                    );
                }
            }
        }
        if !validate_manifest_permissions(&mut manifest, &message_names, false, signature) {
            panic!("There were errors in the manifest files");
        }
        let app_bin = self.build_local_crate(app_name);
        let app_id_hex = hex::encode(manifest.app_id);
        let bundle_dir = out.join(&app_id_hex);
        fs::remove_dir_all(&bundle_dir).ok();
        // A standalone bundle carries its icons inside as `icon.bin` and `icon-dark.bin`, where the
        // registry looks for a sideloaded app's icon.
        let icon_dest = bundle_dir.join("icon.bin");
        let bundled = self.bundle_app(app_name, &app_bin, &bundle_dir, Some(&icon_dest));

        // Hosted (raw host) binaries can't carry a cosign2 header; the simulator execs them by path.
        if self.target.is_none() || matches!(signing_mode, SigningMode::None) {
            println!("[!] App signing was skipped");
            return bundled;
        }
        let cosign2_config = cosign2_config.unwrap_or_else(|| project_root().join("cosign2.toml"));
        match signature {
            RequiredSignature::Foundation => println!(
                "[!] Foundation bundle signed with {}: trusted like a built-in only by firmware \
                 built from the same key.",
                cosign2_config.display()
            ),
            RequiredSignature::ThirdParty => println!(
                "[!] Third-party bundle signed with {}: it launches only while a matching \
                 publisher certificate is installed on the device.",
                cosign2_config.display()
            ),
        }
        let cosign2 = Cosign2::new(Some(cosign2_config))
            .context("Creating cosign2 command")
            .expect("Could not create cosign2 command");
        sign_bundle(&cosign2, signing_mode, &bundled);

        bundled
    }

    /// Bundle one already-built app into `bundle_dir`: strip its ELF to `app.elf`, stage the icons
    /// at `icon_dest` when given, and write `manifest.json` with `fileHashes`. The caller
    /// wipes/creates the parent as needed; `icon_dest` is where the light icon lands (inside the
    /// bundle for standalone bundles, the shared app-icons dir for built-ins), with any dark
    /// variant beside it, or `None` to stage no icon.
    fn bundle_app(
        &self,
        app_name: &str,
        app_bin: &str,
        bundle_dir: &Path,
        icon_dest: Option<&Path>,
    ) -> BundledApp {
        fs::create_dir_all(bundle_dir)
            .unwrap_or_else(|e| panic!("Could not create {}: {e}", bundle_dir.display()));
        let elf_path = bundle_dir.join("app.elf");
        self.strip_elf(app_bin, elf_path.to_str().unwrap());
        if let Some(icon_dest) = icon_dest {
            stage_bundled_icon(app_name, icon_dest);
        }
        // fileHashes must be taken after the icons are staged, so in-bundle icon files are covered.
        let app_version = crate_version(app_name);
        let manifest = generated_bundle_manifest(
            load_manifest(app_name),
            &app_version,
            &self.keyos_version,
            bundle_file_hashes(bundle_dir),
        );
        let hashed_files = manifest.file_hashes.keys().cloned().collect();
        let manifest_path = bundle_dir.join("manifest.json");
        serde_json::to_writer(
            fs::File::create(&manifest_path).expect("Couldn't open target manifest file"),
            &manifest,
        )
        .expect("Json serialization failed");
        BundledApp {
            dir: bundle_dir.to_path_buf(),
            app_name: app_name.to_string(),
            elf_path,
            manifest_path,
            app_version,
            hashed_files,
        }
    }

    /// After a hosted rebuild, re-stage an app that already has a bundle under the hosted apps tree
    /// so the simulator (which execs from the staged bundle) picks up the new ELF. A no-op for a
    /// crate with no staged bundle, e.g. a service.
    pub(crate) fn restage_hosted_app(&self, crate_name: &str, app_bin: &str) {
        let bundle_dir = self.get_apps_path().join(crate_name);
        if !bundle_dir.join("app.elf").exists() {
            return;
        }
        println!("Re-staging app bundle at {}", bundle_dir.display());
        self.bundle_app(crate_name, app_bin, &bundle_dir, None);
    }

    fn update_nameserver_system_manifests(&self) {
        let is_recovery = self.features.contains(&String::from("recovery-os"));
        let mut manifests = Vec::new();
        let mut message_names = HashMap::<(String, String), MessagePolicy>::new();
        let mut manifest_error = false;
        for service in &self.services {
            let CrateSpec::Local(service) = service else { continue };
            let manifest = load_manifest(&service);
            let app_name = manifest.app_name_en();
            for (server_name, messages) in manifest.servers.iter() {
                for (message_name, message) in messages {
                    if message_names
                        .insert(
                            (server_name.clone(), message_name.clone()),
                            (message.required_signature(), message.approval),
                        )
                        .is_some()
                    {
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
            if !validate_manifest_permissions(
                manifest,
                &message_names,
                is_recovery,
                RequiredSignature::Foundation,
            ) {
                manifest_error = true;
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
    pub fn build_combined_image(self, target_path: &Path, signing_mode: SigningMode) {
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
        };

        args.extend_from_slice(&["-i", combined_img_path_str]);
        args.extend_from_slice(&["-c", cosign2_config_path_str]);
        args.extend_from_slice(&["--in-place"]);
        let keyos_version = self.keyos_version.to_string();
        args.extend_from_slice(&["--binary-version", &keyos_version]);

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

/// Validate a manifest's `[permissions]` against the servers' declared messages: unresolved
/// templates, messages that do not exist (removed with a warning on recovery images, which build
/// a subset of the services), and, for a `ThirdParty` manifest, messages the device never grants
/// to a sideloaded app (Foundation-only ones and ones whose approval is not user-grantable), so
/// the first use would panic with AccessDenied. Returns false when the manifest had errors.
fn validate_manifest_permissions(
    manifest: &mut Manifest,
    message_names: &HashMap<(String, String), MessagePolicy>,
    is_recovery: bool,
    app_signature: RequiredSignature,
) -> bool {
    let app_name = manifest.app_name_en();
    let app_id = hex::encode(manifest.app_id);
    let mut ok = true;
    for (server_name, messages) in manifest.permissions.iter_mut() {
        if server_name == "template" {
            println!("[!] Manifest error in {app_name} (0x{app_id}): template(s) {messages:?} do not exist.");
            ok = false;
            continue;
        }
        // Unknown messages must not reach the nameserver manifest, or it panics on start.
        messages.retain(|message_name| {
            match message_names.get(&(server_name.clone(), message_name.clone())) {
                None if is_recovery => {
                    println!(
                        "Manifest warning in {app_name} (0x{app_id}): message {server_name}:{message_name} does not exist. Removing."
                    );
                    return false;
                }
                None => {
                    println!(
                        "[!] Manifest error in {app_name} (0x{app_id}): message {server_name}:{message_name} does not exist."
                    );
                    ok = false;
                }
                Some((RequiredSignature::Foundation, _)) if app_signature == RequiredSignature::ThirdParty => {
                    println!(
                        "[!] Manifest error in {app_name} (0x{app_id}): message {server_name}:{message_name} is Foundation-only and never granted to a sideloaded app (using it panics with AccessDenied)."
                    );
                    ok = false;
                }
                Some((_, ApprovalBehavior::NotUserGrantable))
                    if app_signature == RequiredSignature::ThirdParty =>
                {
                    println!(
                        "[!] Manifest error in {app_name} (0x{app_id}): message {server_name}:{message_name} is never user-grantable, so the device never grants it to a sideloaded app (using it panics with AccessDenied)."
                    );
                    ok = false;
                }
                Some(_) => {}
            }
            true
        });
    }
    ok
}

pub fn load_manifest(crate_name: &str) -> Manifest {
    let manifest = Manifest::load(&get_crate_dir(crate_name), &project_root());
    // version and fileHashes are generated below from Cargo metadata and the staged bundle; a
    // hand-written value is rejected rather than silently overwritten.
    assert!(
        manifest.version.is_none(),
        "version is generated from Cargo.toml; remove it from {crate_name}'s manifest.toml"
    );
    assert!(
        manifest.file_hashes.is_empty(),
        "fileHashes is generated by the build; remove it from {crate_name}'s manifest.toml"
    );
    manifest
}

/// One app bundled: where it landed, the paths cosign2 signs in place, and the files its manifest
/// hashes.
pub struct BundledApp {
    pub dir: PathBuf,
    app_name: String,
    elf_path: PathBuf,
    manifest_path: PathBuf,
    /// Cargo package version stamped into the manifest and both cosign2 headers.
    app_version: Version,
    /// Bundle-relative names of the files the manifest's `fileHashes` covers.
    pub hashed_files: Vec<String>,
}

/// Pack a built bundle into an app archive the device can install from a USB drive.
pub fn pack_app_archive(bundle: &BundledApp, archive_path: &Path) {
    let report = app_archive::pack_bundle(&bundle.dir, archive_path, &bundle.hashed_files)
        .unwrap_or_else(|e| panic!("Could not pack {}: {e}", bundle.dir.display()));
    println!("App archive ready at {} ({} bytes)", report.archive_path.display(), report.archive_bytes);
}

/// Sign a bundle's `app.elf` and `manifest.json` in place with `cosign2`. The two are signed
/// separately: `fileHashes` was taken from the unsigned elf, so signing the elf does not
/// invalidate it and the signatures are independent.
fn sign_bundle(cosign2: &Cosign2, mode: SigningMode, bundled: &BundledApp) {
    for path in [&bundled.elf_path, &bundled.manifest_path] {
        let path_str = path.to_str().unwrap();
        let args = cosign2_sign_args(mode, path, &bundled.app_version);
        println!("Signing `{path_str}` with `cosign2`");
        let exit_status =
            cosign2.sign(args).context("Running cosign2 command").expect("Could not run cosign2 command");
        if !exit_status.success() {
            panic!("Failed to sign {}", bundled.app_name);
        }
    }
}

fn generated_bundle_manifest(
    mut manifest: Manifest,
    app_version: &Version,
    keyos_version: &Version,
    file_hashes: BTreeMap<String, [u8; app_manifest::FILE_HASH_BYTE_LEN]>,
) -> Manifest {
    manifest.version = Some(app_version.to_string());
    manifest.min_keyos_version.get_or_insert_with(|| keyos_version.clone());
    manifest.file_hashes = file_hashes;
    manifest
}

fn cosign2_sign_args(mode: SigningMode, path: &Path, app_version: &Version) -> Vec<String> {
    let mut args = vec!["--in-place".to_string()];
    match mode {
        SigningMode::None => panic!("invalid signing mode"),
        SigningMode::Developer => args.push("--developer".to_string()),
    }
    args.extend([
        "-i".to_string(),
        path.to_str().unwrap().to_string(),
        "--binary-version".to_string(),
        app_version.to_string(),
    ]);
    args
}

/// Convert a built-in app's source icons into raw `.bin` files: the light icon at `dest` and,
/// for a crate that ships one, the dark icon at its `-dark` sibling. Sources are discovered by
/// the SDK convention `resources/icon[-dark].(svg|png)` under the app crate; apps without a
/// light icon ship none.
fn stage_bundled_icon(crate_name: &str, dest: &Path) {
    let crate_dir = get_crate_dir(crate_name);
    let find_source =
        |names: [&str; 2]| names.into_iter().map(|rel| crate_dir.join(rel)).find(|path| path.exists());

    let dark_source = find_source(["resources/icon-dark.svg", "resources/icon-dark.png"]);
    let Some(source) = find_source(["resources/icon.svg", "resources/icon.png"]) else {
        assert!(
            dark_source.is_none(),
            "{crate_name} ships resources/icon-dark.* without the resources/icon.* it falls back to"
        );
        return;
    };

    write_raw_icon(&source, dest);
    if let Some(dark_source) = dark_source {
        write_raw_icon(&dark_source, &dark_icon_path(dest));
    }
}

/// The dark icon's staged path: the `-dark` sibling of the light icon, so both
/// `app-icons/<app-id>.bin` and an in-bundle `icon.bin` gain a matching `-dark.bin`.
fn dark_icon_path(dest: &Path) -> PathBuf {
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
    dest.with_file_name(format!("{stem}-dark.bin"))
}

fn write_raw_icon(source: &Path, dest: &Path) {
    let (_, icon_data) = slint_keyos_platform_build::convert_image_to_raw(source)
        .unwrap_or_else(|e| panic!("Could not convert bundled icon {}: {e}", source.display()));
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("Could not create {}: {e}", parent.display()));
    }
    fs::write(dest, icon_data)
        .unwrap_or_else(|e| panic!("Could not write bundled icon to {}: {e}", dest.display()));
}

/// Sha256 of every bundle file except `manifest.json`, keyed by bundle-relative path with
/// forward slashes. The manifest is the signed container, so it never lists its own hash.
fn bundle_file_hashes(bundle_dir: &Path) -> BTreeMap<String, [u8; app_manifest::FILE_HASH_BYTE_LEN]> {
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
            hashes.insert(rel, Sha256::digest(&bytes).into());
        }
    }
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_manifest_uses_cargo_package_version() {
        let manifest: Manifest =
            serde_json::from_str(r#"{"appName":{"en":"Demo"},"appId":"0x00112233445566778899aabbccddeeff"}"#)
                .unwrap();
        let cargo_version = crate_version("xtask");

        let keyos_version = Version::parse("1.4.0-beta3").unwrap();
        let generated = generated_bundle_manifest(manifest, &cargo_version, &keyos_version, BTreeMap::new());
        let generated_json = serde_json::to_string(&generated).unwrap();
        let packaged: Manifest = serde_json::from_str(&generated_json).unwrap();

        assert_eq!(packaged.version, Some(cargo_version.to_string()));
        assert_eq!(packaged.min_keyos_version, Some(keyos_version));
    }

    #[test]
    #[should_panic(expected = "--production-firmware requires --keyos-version VERSION")]
    fn production_builds_require_an_explicit_keyos_version() {
        Builder::new(BuildArgs { production_firmware: true, ..Default::default() });
    }

    #[test]
    fn developer_builds_default_to_keyos_version_and_cli_overrides_it() {
        let default = Builder::new(BuildArgs::default());
        assert_eq!(default.keyos_version, Version::parse(crate::KEYOS_VERSION).unwrap());

        let requested = Version::parse("1.4.0-beta3").unwrap();
        let explicit =
            Builder::new(BuildArgs { keyos_version: Some(requested.clone()), ..Default::default() });
        assert_eq!(explicit.keyos_version, requested);
    }

    #[test]
    fn signing_header_input_uses_cargo_package_version() {
        let cargo_version = crate_version("xtask");

        let args = cosign2_sign_args(SigningMode::Developer, Path::new("bundle/app.elf"), &cargo_version);

        assert_eq!(
            args,
            [
                "--in-place",
                "--developer",
                "-i",
                "bundle/app.elf",
                "--binary-version",
                env!("CARGO_PKG_VERSION"),
            ]
        );
    }

    #[test]
    fn staged_dark_icon_is_the_dark_suffixed_sibling() {
        assert_eq!(
            dark_icon_path(Path::new("keyos/app-icons/00112233445566778899aabbccddeeff.bin")),
            PathBuf::from("keyos/app-icons/00112233445566778899aabbccddeeff-dark.bin")
        );
        assert_eq!(dark_icon_path(Path::new("bundle/icon.bin")), PathBuf::from("bundle/icon-dark.bin"));
    }
}
