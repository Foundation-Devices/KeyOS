// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[test]
fn built_in_commands_expose_help() {
    let mut commands = vec![
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
        "docs",
        "preview",
        "logs",
        "completions",
    ];
    if cfg!(feature = "experimental-plugins") {
        commands.push("plugin");
    }

    for command in commands {
        let output = Command::new(foundation_bin()).arg(command).arg("--help").output().unwrap();

        assert!(output.status.success(), "help failed for {command}: {}", stderr(&output));
        assert!(
            stdout(&output).contains("Usage: foundation"),
            "missing usage output for {command}: {}",
            stdout(&output)
        );
    }

    #[cfg(feature = "experimental-plugins")]
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

    for subcommand in ["gen", "print", "fingerprint", "install"] {
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

#[cfg(not(feature = "experimental-plugins"))]
#[test]
fn plugin_surface_is_absent_without_the_experimental_feature() {
    let env = TestEnv::new();

    let help = env.command().arg("--help").output().unwrap();
    assert!(help.status.success(), "top-level help failed: {}", stderr(&help));
    assert!(!stdout(&help).contains("plugin"), "top-level help exposed plugins: {}", stdout(&help));

    let plugin_command = env.command().arg("plugin").arg("--help").output().unwrap();
    assert!(!plugin_command.status.success(), "plugin command unexpectedly succeeded");

    // FOUNDATION_FAKE_BIN contains a foundation-echo executable. A normal CLI
    // build must leave the unknown command to clap instead of executing it.
    let external = env.command().arg("echo").arg("alpha").output().unwrap();
    assert!(!external.status.success(), "external plugin unexpectedly executed");
    assert!(!stdout(&external).contains("plugin:"), "external plugin produced output: {}", stdout(&external));

    env.install_home_plugin("demo");
    let installed = env.command().arg("demo").output().unwrap();
    assert!(!installed.status.success(), "installed plugin unexpectedly executed");

    let completions = env.command().arg("completions").arg("bash").arg("--stdout").output().unwrap();
    assert!(completions.status.success(), "completions failed: {}", stderr(&completions));
    assert!(
        !stdout(&completions).contains("plugin") && !stdout(&completions).contains("demo"),
        "completions exposed plugins"
    );
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
fn docs_open_by_default_and_print_urls_on_request() {
    let env = TestEnv::new();
    let install_root = env.home().join(".foundation").join("sdk");
    let current_docs = install_root.join("current").join("docs").join("api");
    let sdk_root_docs = env.bundle_root().join("docs").join("api");
    let version = "1.2.3-beta.1";
    let target = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        platform => panic!("unsupported test platform: {platform:?}"),
    };
    let version_docs =
        install_root.join(format!("foundation-sdk-{version}-{target}")).join("docs").join("api");
    write_docs_bundle(&current_docs, "current docs");
    write_docs_bundle(&sdk_root_docs, "development docs");
    write_docs_bundle(&version_docs, "versioned docs");

    let current = env.command().arg("docs").output().unwrap();
    assert!(current.status.success(), "current docs failed: {}", stderr(&current));
    let current_stdout = stdout(&current);
    assert!(current_stdout.contains("current"));
    let browser_log = env.read_log("browser-open.log");
    assert!(browser_log.contains("file://"), "docs should open a file URL: {browser_log}");
    assert!(
        browser_log.contains(&sdk_root_docs.display().to_string()),
        "docs should open the FOUNDATION_SDK_ROOT bundle: {browser_log}"
    );
    assert!(
        !browser_log.contains(&current_docs.display().to_string()),
        "docs should not open the global current bundle: {browser_log}"
    );

    let versioned = env.command().args(["docs", version, "--url"]).output().unwrap();
    assert!(versioned.status.success(), "versioned docs failed: {}", stderr(&versioned));
    let versioned_stdout = stdout(&versioned);
    assert!(versioned_stdout.contains("file://"));
    assert!(versioned_stdout.contains(version));
    assert_eq!(env.read_log("browser-open.log"), browser_log, "--url must not open a browser");
    assert_eq!(fs::read_to_string(version_docs.join("index.html")).unwrap(), "versioned docs");

    let help = env.command().args(["docs", "--help"]).output().unwrap();
    assert!(help.status.success(), "docs help failed: {}", stderr(&help));
    assert!(!stdout(&help).contains("--port"));
    assert!(!stdout(&help).contains("--kill"));
    assert!(!stdout(&help).contains("--list"));
    assert!(!stdout(&help).contains("--open"));
    assert!(stdout(&help).contains("--url"));
}

fn write_docs_bundle(root: &Path, root_page: &str) {
    fs::create_dir_all(root.join("v1.4.0")).unwrap();
    fs::write(root.join("index.html"), root_page).unwrap();
    fs::write(root.join("bundle-manifest.js"), "manifest").unwrap();
    fs::write(root.join("version-selector.js"), "selector").unwrap();
    fs::write(root.join("v1.4.0/index.html"), "KeyOS 1.4.0 docs").unwrap();
    fs::write(
        root.join("bundle-manifest.json"),
        r#"{"schemaVersion":1,"defaultKeyosVersion":"1.4.0","versions":[{"keyosVersion":"1.4.0","path":"v1.4.0/"}]}"#,
    )
    .unwrap();
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
    assert!(
        stdout(&gen_cert).contains("Full:  0f715baf5d4c2ed329785cef29e562f73488c8a2bb9dbc5700b361d54b9b0554")
    );
    assert!(stdout(&gen_cert).contains("Short: 0f715baf…4b9b0554"));
    let signing_root = env.home().join(".foundation").join("signing").join("Smoke Publisher");
    assert!(signing_root.join("private.pem").exists());
    assert!(signing_root.join("public.pub").exists());
    assert!(signing_root.join("Smoke Publisher.crt").exists());
    assert!(signing_root.join("cosign2.toml").exists());

    let print_cert = env.command().arg("cert").arg("print").arg("Smoke Publisher").output().unwrap();
    assert!(print_cert.status.success(), "cert print failed: {}", stderr(&print_cert));
    assert!(stdout(&print_cert).contains("Certificate contents"));

    let fingerprint_cert = env
        .command()
        .arg("cert")
        .arg("fingerprint")
        .arg(signing_root.join("Smoke Publisher.crt"))
        .output()
        .unwrap();
    assert!(fingerprint_cert.status.success(), "cert fingerprint failed: {}", stderr(&fingerprint_cert));
    assert!(stdout(&fingerprint_cert)
        .contains("Full:  0f715baf5d4c2ed329785cef29e562f73488c8a2bb9dbc5700b361d54b9b0554"));
    assert!(stdout(&fingerprint_cert).contains("Short: 0f715baf…4b9b0554"));

    let install_cert = env.command().arg("cert").arg("install").arg("Smoke Publisher").output().unwrap();
    assert!(!install_cert.status.success(), "non-interactive cert install unexpectedly succeeded");
    assert!(
        stdout(&install_cert).contains("Foundation has NOT verified this publisher's identity"),
        "missing publisher warning: {}",
        stdout(&install_cert)
    );
    assert!(
        stderr(&install_cert).contains("requires an interactive terminal"),
        "missing interactive confirmation error: {}",
        stderr(&install_cert)
    );
    let passport_drive_log = env.read_log("passport-drive.log");
    assert!(
        !passport_drive_log.contains("\"name\":\"install_certificate\""),
        "non-interactive install reached passport-drive: {passport_drive_log}"
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
    assert!(
        !stderr(&build).contains("`version` in app-config.toml is deprecated"),
        "canonical config emitted a legacy-version warning: {}",
        stderr(&build)
    );
    let built_manifest = env.app_root().join("target").join("keyos").join("smoke-app").join("manifest.json");
    assert!(built_manifest.exists());
    // The build signs the manifest, prepending a 2048-byte cosign2 header; the JSON follows it.
    let built_manifest_bytes = fs::read(&built_manifest).unwrap();
    let built_manifest_json = std::str::from_utf8(&built_manifest_bytes[2048..]).unwrap();
    assert!(built_manifest_json.contains("\"version\": \"0.1.0\""));
    assert!(built_manifest_json.contains("os/gui-server"));
    assert!(built_manifest_json.contains("os/settings"));
    assert_cosign2_version(&built_manifest, "0.1.0");
    assert_cosign2_version(
        &env.app_root().join("target").join("keyos").join("smoke-app").join("app.elf"),
        "0.1.0",
    );

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
    assert!(
        !stderr(&sim).contains("`version` in app-config.toml is deprecated"),
        "canonical config emitted a legacy-version warning: {}",
        stderr(&sim)
    );
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
    assert!(cargo_log.contains("cmd=metadata "), "Cargo metadata bypassed the fake cargo shim: {cargo_log}");
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
fn legacy_app_config_versions_warn_and_must_match_cargo() {
    let env = TestEnv::new();
    env.write_smoke_app();

    let config_path = env.app_root().join("app-config.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace(
            "min-keyos-version = \"1.0.0\"",
            "version = \"0.1.0\"\n            min-keyos-version = \"1.0.0\"",
        ),
    )
    .unwrap();

    let sim = env.command_in(env.app_root()).arg("sim").output().unwrap();
    assert!(sim.status.success(), "legacy sim failed: {}", stderr(&sim));
    assert!(
        stderr(&sim).contains("`version` in app-config.toml is deprecated"),
        "sim omitted the legacy-version warning: {}",
        stderr(&sim)
    );

    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(&config_path, config.replace("version = \"0.1.0\"", "version = \"9.9.9\"")).unwrap();
    let mismatch =
        env.command_in(env.app_root()).env("FOUNDATION_DEVELOP_SHELL", "1").arg("build").output().unwrap();
    assert!(!mismatch.status.success(), "mismatched legacy version unexpectedly built");
    assert!(
        stderr(&mismatch).contains("`version` in app-config.toml is deprecated")
            && stderr(&mismatch).contains("Legacy version 9.9.9")
            && stderr(&mismatch).contains("Cargo package version 0.1.0"),
        "build warning or mismatched legacy version error was unclear: {}",
        stderr(&mismatch)
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

#[cfg(feature = "experimental-plugins")]
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
    let completions = env.command().arg("completions").arg("bash").arg("--stdout").output().unwrap();
    assert!(completions.status.success(), "completion output failed: {}", stderr(&completions));
    assert!(stdout(&completions).contains("demo"));
}

#[test]
fn completion_commands_work_in_smoke_env() {
    let env = TestEnv::new();

    let completions = env.command().arg("completions").arg("bash").arg("--stdout").output().unwrap();
    assert!(completions.status.success(), "completion output failed: {}", stderr(&completions));
    assert!(stdout(&completions).contains("foundation"));

    let zshrc = env.home().join(".zshrc");
    fs::write(&zshrc, "autoload -Uz compinit && compinit\n").unwrap();
    let install_completions = env.command().arg("completions").arg("zsh").output().unwrap();
    assert!(
        install_completions.status.success(),
        "completion install failed: {}",
        stderr(&install_completions)
    );
    assert!(stdout(&install_completions).is_empty());
    let completion_file = env.home().join(".zsh").join("completions").join("_foundation");
    assert!(completion_file.exists());
    assert!(fs::read_to_string(completion_file).unwrap().contains("foundation"));

    let zshrc = fs::read_to_string(zshrc).unwrap();
    let fpath_position = zshrc.find("fpath=(~/.zsh/completions $fpath)").unwrap();
    let compinit_position = zshrc.find("autoload -Uz compinit && compinit").unwrap();
    assert!(fpath_position < compinit_position, "completion fpath must be configured before compinit");

    fs::write(
        env.home().join(".zshrc"),
        "autoload -Uz compinit && compinit\n# Foundation CLI completions\nfpath=(~/.zsh/completions $fpath)\n",
    )
    .unwrap();
    let repair_completions = env.command().arg("completions").arg("zsh").output().unwrap();
    assert!(repair_completions.status.success(), "completion repair failed: {}", stderr(&repair_completions));
    let repaired_zshrc = fs::read_to_string(env.home().join(".zshrc")).unwrap();
    let fpath_position = repaired_zshrc.find("fpath=(~/.zsh/completions $fpath)").unwrap();
    let compinit_position = repaired_zshrc.find("autoload -Uz compinit && compinit").unwrap();
    assert!(fpath_position < compinit_position, "existing completion configuration was not repaired");
    assert_eq!(repaired_zshrc.matches("fpath=(~/.zsh/completions $fpath)").count(), 1);

    fs::write(
        env.home().join(".zshrc"),
        "# autoload -Uz compinit && compinit\n# fpath=(~/.zsh/completions $fpath)\n",
    )
    .unwrap();
    let commented = env.command().arg("completions").arg("zsh").output().unwrap();
    assert!(commented.status.success(), "commented zsh install failed: {}", stderr(&commented));
    let repaired = fs::read_to_string(env.home().join(".zshrc")).unwrap();
    let fpath_position = repaired.rfind("\nfpath=(~/.zsh/completions $fpath)").unwrap();
    let compinit_position = repaired.rfind("\nautoload -Uz compinit && compinit").unwrap();
    assert!(fpath_position < compinit_position, "commented commands were treated as active");

    for zshrc_content in [
        "autoload -Uz compinit && compinit\n[[ -d ~/.zsh/completions ]] && fpath=(~/.zsh/completions $fpath)\n",
        "autoload -Uz compinit && compinit; fpath=(~/.zsh/completions $fpath)\n",
    ] {
        fs::write(env.home().join(".zshrc"), zshrc_content).unwrap();
        let guarded = env.command().arg("completions").arg("zsh").output().unwrap();
        assert!(guarded.status.success(), "guarded zsh install failed: {}", stderr(&guarded));
        let repaired = fs::read_to_string(env.home().join(".zshrc")).unwrap();
        assert!(repaired.contains(zshrc_content.trim_end()), "custom zsh configuration was changed");
        let fpath_position = repaired.find("# Foundation CLI completions\nfpath=(~/.zsh/completions $fpath)").unwrap();
        let compinit_position = repaired.find("autoload -Uz compinit && compinit").unwrap();
        assert!(fpath_position < compinit_position, "safe completion configuration was not inserted");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let zshrc = env.home().join(".zshrc");
        fs::remove_file(&zshrc).unwrap();
        let dotfiles = env.home().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        let managed_zshrc = dotfiles.join("zshrc");
        fs::write(&managed_zshrc, "autoload -Uz compinit && compinit\nfpath=(~/.zsh/completions $fpath)\n")
            .unwrap();
        symlink("dotfiles/zshrc", &zshrc).unwrap();

        let install = env.command().arg("completions").arg("zsh").output().unwrap();
        assert!(install.status.success(), "symlinked zsh install failed: {}", stderr(&install));
        assert!(fs::symlink_metadata(&zshrc).unwrap().file_type().is_symlink());
        let repaired = fs::read_to_string(&managed_zshrc).unwrap();
        let fpath_position = repaired.find("fpath=(~/.zsh/completions $fpath)").unwrap();
        let compinit_position = repaired.find("autoload -Uz compinit && compinit").unwrap();
        assert!(fpath_position < compinit_position, "symlinked zsh configuration was not repaired");

        let immutable_dir = env.home().join("immutable-dotfiles");
        fs::create_dir(&immutable_dir).unwrap();
        let immutable_zshrc = immutable_dir.join("zshrc");
        fs::write(&immutable_zshrc, "autoload -Uz compinit && compinit\n").unwrap();
        fs::remove_file(&zshrc).unwrap();
        symlink("immutable-dotfiles/zshrc", &zshrc).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let writable_permissions = fs::metadata(&immutable_dir).unwrap().permissions();
        fs::set_permissions(&immutable_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let immutable = env.command().arg("completions").arg("zsh").output().unwrap();
        fs::set_permissions(&immutable_dir, writable_permissions).unwrap();

        assert!(immutable.status.success(), "immutable zsh install failed: {}", stderr(&immutable));
        assert!(stderr(&immutable).contains("Could not update ~/.zshrc automatically"));
        assert!(stderr(&immutable)
            .contains("fpath=(~/.zsh/completions $fpath)\nautoload -Uz compinit && compinit"));
    }

    #[cfg(target_os = "macos")]
    {
        let spaced_home = env.root.path().join("home with spaces");
        fs::create_dir(&spaced_home).unwrap();
        let bash = env.command().env("HOME", &spaced_home).arg("completions").arg("bash").output().unwrap();
        assert!(bash.status.success(), "Bash install failed: {}", stderr(&bash));
        let completion_file = spaced_home.join(".bash_completion.d").join("foundation");
        let profile = fs::read_to_string(spaced_home.join(".bash_profile")).unwrap();
        assert!(profile.contains(&format!("source '{}'", completion_file.display())));
    }

    let powershell = env.command().arg("completions").arg("powershell").output().unwrap();
    assert!(powershell.status.success(), "PowerShell install failed: {}", stderr(&powershell));
    let powershell_file =
        env.home().join(".config").join("powershell").join("completions").join("foundation.ps1");
    assert!(stderr(&powershell).contains(&format!(". '{}'", powershell_file.display())));

    #[cfg(not(target_os = "macos"))]
    {
        let bash = env.command().arg("completions").arg("bash").output().unwrap();
        assert!(bash.status.success(), "Bash install failed: {}", stderr(&bash));
        let bash_file = env.home().join(".bash_completion.d").join("foundation");
        assert!(stderr(&bash).contains(&format!("source '{}'", bash_file.display())));
    }

    let legacy_install = env.command().arg("completions").arg("fish").arg("--install").output().unwrap();
    assert!(legacy_install.status.success(), "legacy --install failed: {}", stderr(&legacy_install));
    assert!(env.home().join(".config").join("fish").join("completions").join("foundation.fish").exists());
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

    #[cfg(feature = "experimental-plugins")]
    {
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
    }

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
    real_cargo: PathBuf,
    path: OsString,
}

impl TestEnv {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("foundation-cli-smoke-").tempdir().unwrap();
        let home = root.path().join("home");
        let bundle = root.path().join("sdk-bundle");
        let app = root.path().join("smoke-app");
        let fake_bin = Path::new(env!("FOUNDATION_FAKE_BIN"));
        let real_cargo = std::env::var_os("CARGO")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .map(|entry| entry.join(format!("cargo{}", std::env::consts::EXE_SUFFIX)))
                    .find(|path| path.is_file())
            })
            .expect("find real cargo before prepending the test fakes");

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
        let api_settings = bundle.join("lib").join("keyos").join("api").join("settings");
        fs::create_dir_all(&api_settings).unwrap();
        fs::write(
            api_settings.join("manifest.toml"),
            "[servers.\"os/settings\"]\nGetDeviceName = { id = 32, type = \"blockingArchive\", \
             permissionGroup = \"settings.ui-essentials\", approval = \"autoAllow\" }\n",
        )
        .unwrap();
        let api_gui_server = bundle.join("lib").join("keyos").join("api").join("gui-server");
        fs::create_dir_all(&api_gui_server).unwrap();
        fs::write(
            api_gui_server.join("manifest.toml"),
            "[servers.\"os/gui-server\"]\nRegisterAppMessage = { id = 0, type = \"blockingArchive\", \
             permissionGroup = \"ui-and-input.app-surface\", approval = \"autoAllow\" }\n\
             RequestRedraw = { id = 23, type = \"scalar\", \
             permissionGroup = \"ui-and-input.app-surface\", approval = \"autoAllow\" }\n",
        )
        .unwrap();
        fs::write(themes_dir.join("base_theme.json"), r#"{"id":"base_theme","name":"Base Theme"}"#).unwrap();
        fs::write(bundle.join("ui").join("ui").join("placeholder.slint"), "// ui\n").unwrap();
        fs::write(bundle.join("ui").join("ui").join("theme.slint"), "// theme\n").unwrap();
        fs::write(bundle.join("resources").join("icons").join("loader.svg"), "<svg></svg>\n").unwrap();

        let mut path_entries = vec![fake_bin.to_path_buf()];
        path_entries.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        let path = std::env::join_paths(path_entries).unwrap();

        Self { root, home, bundle, app, real_cargo, path }
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
            .env("FOUNDATION_REAL_CARGO", &self.real_cargo)
            .env("CARGO", Path::new(env!("FOUNDATION_FAKE_BIN")).join("cargo"))
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

    #[cfg(feature = "experimental-plugins")]
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
        fs::create_dir_all(self.app.join("src")).unwrap();
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
        fs::write(self.app.join("src").join("main.rs"), "fn main() {}\n").unwrap();
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

fn assert_cosign2_version(path: &Path, expected: &str) {
    let bytes = fs::read(path).unwrap();
    let version = &bytes[22..42];
    let version_end = version.iter().position(|byte| *byte == 0).unwrap_or(version.len());
    assert_eq!(std::str::from_utf8(&version[..version_end]).unwrap(), expected);
}
