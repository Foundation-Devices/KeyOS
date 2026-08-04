// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[test]
fn built_in_commands_expose_help() {
    for command in [
        "new",
        "develop",
        "exit",
        "build",
        "sideload",
        "sim",
        "cert",
        "theme",
        "themes",
        "doctor",
        "preview",
        "logs",
        "plugin",
        "completions",
    ] {
        let output = Command::new(foundation_bin()).arg(command).arg("--help").output().unwrap();

        assert!(output.status.success(), "help failed for {command}: {}", stderr(&output));
        assert!(
            stdout(&output).contains("Usage: foundation"),
            "missing usage output for {command}: {}",
            stdout(&output)
        );
    }

    for subcommand in ["install", "uninstall", "search"] {
        let output =
            Command::new(foundation_bin()).arg("plugin").arg(subcommand).arg("--help").output().unwrap();
        assert!(output.status.success(), "help failed for plugin {subcommand}: {}", stderr(&output));
        assert!(
            stdout(&output).contains("Usage: foundation plugin"),
            "missing plugin usage output for {subcommand}: {}",
            stdout(&output)
        );
    }

    for subcommand in ["gen", "print", "install"] {
        let output =
            Command::new(foundation_bin()).arg("cert").arg(subcommand).arg("--help").output().unwrap();
        assert!(output.status.success(), "help failed for cert {subcommand}: {}", stderr(&output));
        assert!(
            stdout(&output).contains("Usage: foundation cert"),
            "missing cert usage output for {subcommand}: {}",
            stdout(&output)
        );
    }
}

#[test]
fn removed_legacy_command_names_fail() {
    for command in ["undevelop", "genkey", "gen-cert", "view", "install", "uninstall", "search"] {
        let output = Command::new(foundation_bin()).arg(command).arg("--help").output().unwrap();

        assert!(
            !output.status.success(),
            "legacy command unexpectedly succeeded for {command}: {}",
            stdout(&output)
        );
    }
}

#[test]
fn environment_commands_work_in_smoke_env() {
    let env = TestEnv::new();

    let doctor = env.command().env("FOUNDATION_DEVELOP_SHELL", "1").arg("doctor").output().unwrap();
    assert!(doctor.status.success(), "doctor failed: {}", stderr(&doctor));
    assert!(stdout(&doctor).contains("All checks passed"));

    let develop = env.command().arg("develop").output().unwrap();
    assert!(develop.status.success(), "develop failed: {}", stderr(&develop));
    assert!(env.home().join(".foundation").join(".bashrc").exists());
    let develop_log = env.read_log("nix-develop.log");
    assert!(develop_log.contains("develop"));
    assert!(develop_log.contains("--rcfile"));

    fs::create_dir_all(env.home().join(".foundation").join("sdk").join("current")).unwrap();
    fs::create_dir_all(env.home().join(".cache").join("nix")).unwrap();
    let exit = env.command().arg("exit").output().unwrap();
    assert!(exit.status.success(), "exit failed: {}", stderr(&exit));
    assert!(env.home().join(".foundation").join("sdk").join("current").exists());
    assert!(!env.home().join(".cache").join("nix").exists());
}

#[test]
fn build_sim_preview_sideload_and_gen_cert_work_in_smoke_env() {
    let env = TestEnv::new();
    env.write_smoke_app();

    let gen_cert = env
        .command()
        .arg("cert")
        .arg("gen")
        .arg("Smoke Publisher")
        .arg("--publisher-name")
        .arg("Smoke Publisher")
        .arg("--contact-email")
        .arg("support@example.com")
        .arg("--support-url")
        .arg("https://example.com/support")
        .output()
        .unwrap();
    assert!(gen_cert.status.success(), "cert gen failed: {}", stderr(&gen_cert));
    let signing_root = env.home().join(".foundation").join("signing").join("Smoke Publisher");
    assert!(signing_root.join("private.pem").exists());
    assert!(signing_root.join("public.pub").exists());
    assert!(signing_root.join("Smoke Publisher.crt").exists());
    assert!(signing_root.join("cosign2.toml").exists());

    let print_cert = env.command().arg("cert").arg("print").arg("Smoke Publisher").output().unwrap();
    assert!(print_cert.status.success(), "cert print failed: {}", stderr(&print_cert));
    assert!(stdout(&print_cert).contains("Certificate contents"));

    let install_cert = env.command().arg("cert").arg("install").arg("Smoke Publisher").output().unwrap();
    assert!(install_cert.status.success(), "cert install failed: {}", stderr(&install_cert));
    assert!(stdout(&install_cert).contains("Certificate installed successfully"));
    let passport_drive_log = env.read_log("passport-drive.log");
    assert!(
        passport_drive_log.contains("\"name\":\"install_certificate\""),
        "missing install_certificate call: {passport_drive_log}"
    );
    assert!(
        passport_drive_log.contains("Smoke Publisher.crt"),
        "install_certificate did not receive generated certificate: {passport_drive_log}"
    );

    let preview =
        env.command_in(env.app_root()).arg("preview").arg("--style").arg("fluent").output().unwrap();
    assert!(preview.status.success(), "preview failed: {}", stderr(&preview));
    let viewer_log = env.read_log("viewer.log");
    assert!(viewer_log.contains("--style fluent"));
    let expected_ui_path = env.app_root().join("target").join("foundation").join("ui").join("ui");
    let expected_ui_path = fs::canonicalize(&expected_ui_path).unwrap_or(expected_ui_path);
    let expected_ui_arg = format!("ui={}", expected_ui_path.display());
    assert!(viewer_log.contains(&expected_ui_arg), "viewer log missing {expected_ui_arg}: {viewer_log}");
    assert!(viewer_log.contains("ui/app.slint"));

    let build =
        env.command_in(env.app_root()).env("FOUNDATION_DEVELOP_SHELL", "1").arg("build").output().unwrap();
    assert!(build.status.success(), "build failed: {}", stderr(&build));
    let built_manifest = env.app_root().join("target").join("keyos").join("smoke-app").join("manifest.json");
    assert!(built_manifest.exists());
    // The build signs the manifest, prepending a 2048-byte cosign2 header; the JSON follows it.
    let built_manifest_bytes = fs::read(&built_manifest).unwrap();
    let built_manifest_json = std::str::from_utf8(&built_manifest_bytes[2048..]).unwrap();
    assert!(built_manifest_json.contains("os/gui-server"));
    assert!(built_manifest_json.contains("os/settings"));

    let sideload = env
        .command_in(env.app_root())
        .env("FOUNDATION_DEVELOP_SHELL", "1")
        .arg("sideload")
        .arg("--no-run")
        .output()
        .unwrap();
    assert!(sideload.status.success(), "sideload failed: {}", stderr(&sideload));
    let passport_drive_log = env.read_log("passport-drive.log");
    assert!(
        passport_drive_log.contains("\"name\":\"load_app\""),
        "missing load_app call: {passport_drive_log}"
    );
    assert!(
        passport_drive_log.contains("target/keyos/smoke-app"),
        "load_app did not receive built artifact dir: {passport_drive_log}"
    );

    let sim = env.command_in(env.app_root()).arg("sim").output().unwrap();
    assert!(sim.status.success(), "sim failed: {}", stderr(&sim));
    let app_id_dir = "00112233445566778899aabbccddeeff";
    // The host-exec'd app.elf and reference manifest land under the simulator's
    // sideloaded-apps dir, keyed by the bare hex app id.
    let host_staged = env
        .bundle_root()
        .join("lib")
        .join("keyos")
        .join("simulator")
        .join("sideloaded-apps")
        .join(app_id_dir);
    assert!(host_staged.join("app.elf").exists());
    assert!(host_staged.join("manifest.json").exists());
    assert!(host_staged.join("icon.bin").exists());
    // The device-format bundle (manifest + converted resources, no app.elf) that
    // gets injected into the simulator system image.
    let injected = env.app_root().join("target").join("foundation").join("sim-sideload").join(app_id_dir);
    assert!(injected.join("manifest.json").exists());
    assert!(injected.join("icon.bin").exists());
    assert!(env.read_log("simulator.log").contains(env.bundle_root().display().to_string().as_str()));
    assert!(env.read_log("simulator-stdin.log").contains("run 0x00112233445566778899aabbccddeeff"));
    let cargo_log = env.read_log("cargo.log");
    assert!(
        cargo_log
            .contains("cmd=build --package smoke-app --message-format json-render-diagnostics RUSTFLAGS=\n"),
        "sim should build natively without forced hardware flags: {cargo_log}"
    );
    assert!(
        !cargo_log.contains("cmd=build --package smoke-app RUSTFLAGS=--cfg keyos"),
        "sim should not force hardware cfg for hosted builds: {cargo_log}"
    );
}

#[test]
fn build_refuses_invalid_app_names_before_cargo() {
    let env = TestEnv::new();
    env.write_smoke_app();

    let config_path = env.app_root().join("app-config.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace("friendly-app-name = \"Smoke App\"", "friendly-app-name = \"Smoke_App\""),
    )
    .unwrap();

    let build =
        env.command_in(env.app_root()).env("FOUNDATION_DEVELOP_SHELL", "1").arg("build").output().unwrap();

    assert!(!build.status.success());
    assert!(stderr(&build).contains("friendly-app-name"), "stderr was: {}", stderr(&build));
    assert!(env.read_log("cargo.log").is_empty());
}

#[test]
fn build_refuses_invalid_icon_size_before_cargo() {
    let env = TestEnv::new();
    env.write_smoke_app();

    fs::write(env.app_root().join("resources").join("icon.svg"), r#"<svg width="128" height="96"></svg>"#)
        .unwrap();

    let build =
        env.command_in(env.app_root()).env("FOUNDATION_DEVELOP_SHELL", "1").arg("build").output().unwrap();

    assert!(!build.status.success());
    assert!(stderr(&build).contains("Icon must be 110x110px"), "stderr was: {}", stderr(&build));
    assert!(env.read_log("cargo.log").is_empty());
}

#[test]
fn logs_command_launches_bundled_log_viewer() {
    let env = TestEnv::new();

    let logs = env.command().arg("logs").arg("--timeout").arg("9").output().unwrap();
    assert!(logs.status.success(), "logs failed: {}", stderr(&logs));

    let viewer_log = env.read_log("log-viewer.log");
    assert!(viewer_log.contains("--timeout 9"));
}

#[test]
fn theme_command_creates_app_theme_and_launches_bundled_editor() {
    let env = TestEnv::new();
    env.write_smoke_app();

    let theme = env.command_in(env.app_root()).arg("theme").output().unwrap();
    assert!(theme.status.success(), "theme failed: {}", stderr(&theme));

    let config = fs::read_to_string(env.app_root().join("app-config.toml")).unwrap();
    assert!(config.contains("theme = \"resources/theme.json\""));

    let theme_path = env.app_root().join("resources").join("theme.json");
    let theme_json = fs::read_to_string(&theme_path).unwrap();
    assert!(theme_json.contains("\"id\": \"app_theme\""));
    assert!(theme_json.contains("\"name\": \"Smoke App\""));
    assert!(theme_json.contains("\"parent\": \"base_theme\""));

    let expected_theme_path = fs::canonicalize(&theme_path).unwrap_or(theme_path);
    let editor_log = env.read_log("theme-editor.log");
    assert!(
        editor_log.contains(expected_theme_path.display().to_string().as_str()),
        "theme editor log missing {}: {editor_log}",
        expected_theme_path.display()
    );
}

#[test]
fn theme_command_opens_an_explicit_file_without_rewriting_it() {
    let env = TestEnv::new();
    let theme_path = env
        .bundle_root()
        .join("lib")
        .join("keyos")
        .join("sdk")
        .join("crates")
        .join("foundation-themes")
        .join("themes")
        .join("base_theme.json");
    let before = fs::read_to_string(&theme_path).unwrap();

    let theme = env.command().arg("theme").arg(&theme_path).output().unwrap();
    assert!(theme.status.success(), "theme failed: {}", stderr(&theme));
    assert_eq!(fs::read_to_string(&theme_path).unwrap(), before);

    let expected_theme_path = fs::canonicalize(&theme_path).unwrap_or(theme_path);
    let editor_log = env.read_log("theme-editor.log");
    assert!(
        editor_log.contains(expected_theme_path.display().to_string().as_str()),
        "theme editor log missing {}: {editor_log}",
        expected_theme_path.display()
    );
}

#[test]
fn plugin_and_completion_commands_work_in_smoke_env() {
    let env = TestEnv::new();
    env.write_plugin_index();

    let search = env.command().arg("plugin").arg("search").arg("demo").output().unwrap();
    assert!(search.status.success(), "search failed: {}", stderr(&search));
    assert!(stdout(&search).contains("Demo plugin"));

    let install = env.command().arg("plugin").arg("install").arg("owner/").output().unwrap();
    assert!(!install.status.success(), "install unexpectedly succeeded");
    assert!(
        stderr(&install).contains("Invalid plugin spec") || stdout(&install).contains("Invalid plugin spec")
    );

    let plugin = env.command().arg("echo").arg("alpha").arg("beta").output().unwrap();
    assert!(plugin.status.success(), "plugin dispatch failed: {}", stderr(&plugin));
    assert!(stdout(&plugin).contains("plugin:alpha beta"));

    env.install_home_plugin("demo");
    let uninstall = env.command().arg("plugin").arg("uninstall").arg("demo").output().unwrap();
    assert!(uninstall.status.success(), "uninstall failed: {}", stderr(&uninstall));
    assert!(!env.home().join(".foundation").join("plugins").join("foundation-demo").exists());

    env.install_home_plugin("demo");
    let completions = env.command().arg("completions").arg("bash").output().unwrap();
    assert!(completions.status.success(), "completions failed: {}", stderr(&completions));
    assert!(stdout(&completions).contains("demo"));

    let install_completions = env.command().arg("completions").arg("zsh").arg("--install").output().unwrap();
    assert!(
        install_completions.status.success(),
        "completion install failed: {}",
        stderr(&install_completions)
    );
    assert!(env.home().join(".zsh").join("completions").join("_foundation").exists());
}

#[test]
fn english_commands_still_work_when_locale_is_non_english() {
    let env = TestEnv::new();

    let top_level_help = env.command().env("FOUNDATION_LANG", "es").arg("--help").output().unwrap();
    assert!(
        top_level_help.status.success(),
        "top-level help failed in es locale: {}",
        stderr(&top_level_help)
    );
    let english = env.command().env("FOUNDATION_LANG", "es").arg("new").arg("--help").output().unwrap();
    assert!(english.status.success(), "english command failed in es locale: {}", stderr(&english));

    let english_plugin = env
        .command()
        .env("FOUNDATION_LANG", "es")
        .arg("plugin")
        .arg("install")
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        english_plugin.status.success(),
        "english plugin command failed in es locale: {}",
        stderr(&english_plugin)
    );

    let english_cert =
        env.command().env("FOUNDATION_LANG", "es").arg("cert").arg("gen").arg("--help").output().unwrap();
    assert!(
        english_cert.status.success(),
        "english cert command failed in es locale: {}",
        stderr(&english_cert)
    );
}

struct TestEnv {
    root: TempDir,
    home: PathBuf,
    bundle: PathBuf,
    app: PathBuf,
    path: OsString,
}

impl TestEnv {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("foundation-cli-smoke-").tempdir().unwrap();
        let home = root.path().join("home");
        let bundle = root.path().join("sdk-bundle");
        let app = root.path().join("smoke-app");
        let fake_bin = Path::new(env!("FOUNDATION_FAKE_BIN"));

        fs::create_dir_all(home.join(".foundation").join("plugins")).unwrap();
        fs::create_dir_all(bundle.join("lib").join("keyos")).unwrap();
        let themes_dir = bundle
            .join("lib")
            .join("keyos")
            .join("sdk")
            .join("crates")
            .join("foundation-themes")
            .join("themes");
        fs::create_dir_all(&themes_dir).unwrap();
        fs::create_dir_all(bundle.join("ui").join("ui")).unwrap();
        fs::create_dir_all(bundle.join("resources").join("icons")).unwrap();
        fs::create_dir_all(bundle.join("target").join("apps")).unwrap();
        link(fake_bin, &bundle.join("bin"));
        fs::write(bundle.join("flake.nix"), "{}").unwrap();
        fs::write(bundle.join("lib").join("keyos").join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(themes_dir.join("base_theme.json"), r#"{"id":"base_theme","name":"Base Theme"}"#).unwrap();
        fs::write(bundle.join("ui").join("ui").join("placeholder.slint"), "// ui\n").unwrap();
        fs::write(bundle.join("ui").join("ui").join("theme.slint"), "// theme\n").unwrap();
        fs::write(bundle.join("resources").join("icons").join("loader.svg"), "<svg></svg>\n").unwrap();

        let mut path_entries = vec![fake_bin.to_path_buf()];
        path_entries.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        let path = std::env::join_paths(path_entries).unwrap();

        Self { root, home, bundle, app, path }
    }

    fn home(&self) -> &Path { &self.home }

    fn bundle_root(&self) -> &Path { &self.bundle }

    fn app_root(&self) -> &Path { &self.app }

    fn command(&self) -> Command { self.base_command(foundation_bin()) }

    fn command_in(&self, current_dir: &Path) -> Command {
        let mut command = self.command();
        command.current_dir(current_dir);
        command
    }

    fn base_command(&self, program: PathBuf) -> Command {
        let mut command = Command::new(program);
        command
            .env("FOUNDATION_LANG", "en")
            .env("FOUNDATION_SDK_ROOT", &self.bundle)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/bash")
            .env("PATH", &self.path)
            // Fakes log into this dir; child tools inherit it so read_log finds them.
            .env("FOUNDATION_FAKE_LOG_DIR", self.root.path());
        command
    }

    fn install_home_plugin(&self, name: &str) {
        let plugin = self.home.join(".foundation").join("plugins").join(format!("foundation-{name}"));
        link(&Path::new(env!("FOUNDATION_FAKE_BIN")).join("noop"), &plugin);
    }

    fn write_plugin_index(&self) {
        fs::write(
            self.home.join(".foundation").join("plugin-index.toml"),
            r#"
            version = 1

            [[plugins]]
            name = "demo"
            description = "Demo plugin"
            repository = "owner/demo"
            verified = true
            tags = ["demo", "test"]
            "#,
        )
        .unwrap();
    }

    fn write_smoke_app(&self) {
        fs::create_dir_all(self.app.join("resources")).unwrap();
        fs::create_dir_all(self.app.join("ui")).unwrap();
        fs::write(
            self.app.join("Cargo.toml"),
            r#"
            [package]
            name = "smoke-app"
            version = "0.1.0"
            edition = "2021"
            "#,
        )
        .unwrap();
        fs::write(
            self.app.join("build.rs"),
            r#"
            use slint_keyos_platform_build::{compile_options, CompileOptions};

            fn main() {
                compile_options(CompileOptions {
                    module_path: "ui/app.slint",
                    include_slint: true,
                    include_router: false,
                    include_translations: true,
                });
            }
            "#,
        )
        .unwrap();
        fs::write(
            self.app.join("app-config.toml"),
            r#"
            app-name = "smoke-app"
            friendly-app-name = "Smoke App"
            description = "Smoke test app"
            icon = "resources/icon.svg"
            app-id = "0x00112233445566778899aabbccddeeff"
            version = "0.1.0"
            min-keyos-version = "1.0.0"

            [publisher]
            name = "Smoke Publisher"
            contact-email = "support@example.com"
            support-url = "https://example.com/support"

            [permissions]
            template = ["gui-app"]
            "os/settings" = ["GetDeviceName"]
            "#,
        )
        .unwrap();
        fs::write(
            self.app.join("permission_templates.toml"),
            r#"
            [gui-app]
            "os/gui-server" = ["RegisterAppMessage", "RequestRedraw"]
            "#,
        )
        .unwrap();
        fs::write(self.app.join("resources").join("icon.svg"), r#"<svg width="110" height="110"></svg>"#)
            .unwrap();
        fs::write(
            self.app.join("ui").join("app.slint"),
            "export component AppWindow inherits Window { Text { text: \"smoke\"; } }\n",
        )
        .unwrap();
    }

    fn read_log(&self, name: &str) -> String {
        fs::read_to_string(self.root.path().join(name)).unwrap_or_default()
    }
}

fn foundation_bin() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_foundation")) }

#[cfg(unix)]
fn link(src: &Path, dst: &Path) { std::os::unix::fs::symlink(src, dst).unwrap(); }

#[cfg(not(unix))]
fn link(_src: &Path, _dst: &Path) {}

fn stdout(output: &Output) -> String { String::from_utf8_lossy(&output.stdout).to_string() }

fn stderr(output: &Output) -> String { String::from_utf8_lossy(&output.stderr).to_string() }
