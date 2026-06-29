// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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
    env.install_fake_nix();
    env.install_fake_git();
    env.install_fake_rustc();
    env.install_fake_arm_strip();
    env.install_bundle_asset_tool();
    env.install_bundle_cosign2();
    env.install_bundle_viewer();

    let doctor = env.command().arg("doctor").output().unwrap();
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
    env.install_fake_nix();
    env.install_fake_git();
    env.install_fake_rustc();
    env.install_fake_arm_strip();
    env.install_fake_cargo();
    env.install_fake_openssl();
    env.install_bundle_asset_tool();
    env.install_bundle_cosign2();
    env.install_bundle_viewer();
    env.install_bundle_theme_compiler();
    env.install_bundle_simulator();
    env.install_bundle_fatfs_image();
    env.install_fake_passport_drive();
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

    let build = env.command_in(env.app_root()).env("IN_NIX_SHELL", "1").arg("build").output().unwrap();
    assert!(build.status.success(), "build failed: {}", stderr(&build));
    let built_manifest = env.app_root().join("target").join("keyos").join("smoke-app").join("manifest.json");
    assert!(built_manifest.exists());
    let built_manifest_contents = fs::read_to_string(&built_manifest).unwrap();
    assert!(built_manifest_contents.contains("os/gui-server"));
    assert!(built_manifest_contents.contains("os/settings"));

    let sideload = env
        .command_in(env.app_root())
        .env("IN_NIX_SHELL", "1")
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
    // The device-format bundle (manifest + converted resources, no app.elf) that
    // gets injected into the simulator system image.
    let injected = env.app_root().join("target").join("foundation").join("sim-sideload").join(app_id_dir);
    assert!(injected.join("manifest.json").exists());
    assert!(injected.join("resources").join(".foundation").join("icon.raw").exists());
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
fn logs_command_launches_bundled_log_viewer() {
    let env = TestEnv::new();
    env.install_bundle_log_viewer();

    let logs = env.command().arg("logs").arg("--timeout").arg("9").output().unwrap();
    assert!(logs.status.success(), "logs failed: {}", stderr(&logs));

    let viewer_log = env.read_log("log-viewer.log");
    assert!(viewer_log.contains("--timeout 9"));
}

#[test]
fn theme_command_creates_app_theme_and_launches_bundled_editor() {
    let env = TestEnv::new();
    env.install_bundle_theme_editor();
    env.write_smoke_app();

    let theme = env.command_in(env.app_root()).arg("theme").output().unwrap();
    assert!(theme.status.success(), "theme failed: {}", stderr(&theme));

    let config = fs::read_to_string(env.app_root().join("app-config.toml")).unwrap();
    assert!(config.contains("theme = \"resources/theme.json\""));

    let theme_path = env.app_root().join("resources").join("theme.json");
    let theme_json = fs::read_to_string(&theme_path).unwrap();
    assert!(theme_json.contains("\"id\": \"app_theme\""));
    assert!(theme_json.contains("\"name\": \"Smoke App\""));

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
    env.install_fake_git();
    env.install_fake_nix();
    env.install_fake_rustc();
    env.install_fake_arm_strip();
    env.install_bundle_cosign2();
    env.install_bundle_viewer();
    env.install_path_plugin("foundation-echo", "#!/bin/sh\nprintf 'plugin:%s\\n' \"$*\"\n");
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
    root: PathBuf,
    home: PathBuf,
    fake_bin: PathBuf,
    bundle: PathBuf,
    app: PathBuf,
    path: OsString,
}

impl TestEnv {
    fn new() -> Self {
        let root = make_temp_dir("foundation-cli-smoke");
        let home = root.join("home");
        let fake_bin = root.join("fake-bin");
        let bundle = root.join("sdk-bundle");
        let app = root.join("smoke-app");

        fs::create_dir_all(home.join(".foundation").join("plugins")).unwrap();
        fs::create_dir_all(fake_bin.clone()).unwrap();
        fs::create_dir_all(bundle.join("bin")).unwrap();
        fs::create_dir_all(bundle.join("lib").join("keyos")).unwrap();
        fs::create_dir_all(bundle.join("ui").join("ui")).unwrap();
        fs::create_dir_all(bundle.join("resources").join("icons")).unwrap();
        fs::create_dir_all(bundle.join("target").join("apps")).unwrap();
        fs::write(bundle.join("flake.nix"), "{}").unwrap();
        fs::write(bundle.join("lib").join("keyos").join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(bundle.join("ui").join("ui").join("placeholder.slint"), "// ui\n").unwrap();
        fs::write(bundle.join("ui").join("ui").join("theme.slint"), "// theme\n").unwrap();
        fs::write(bundle.join("resources").join("icons").join("loader.svg"), "<svg></svg>\n").unwrap();

        let mut path_entries = vec![fake_bin.clone()];
        path_entries.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        let path = std::env::join_paths(path_entries).unwrap();

        Self { root, home, fake_bin, bundle, app, path }
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
            .env("PATH", &self.path);
        command
    }

    fn install_fake_cargo(&self) {
        self.write_script(
            self.fake_bin.join("cargo"),
            format!(
                r#"#!/bin/sh
printf 'cmd=%s RUSTFLAGS=%s\n' "$*" "${{RUSTFLAGS:-}}" >> "{}"
if [ "$1" = "--version" ]; then
  echo 'cargo 1.0.0-fake'
  exit 0
fi
cmd="$1"
shift || true
pkg=""
profile="debug"
target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --package)
      pkg="$2"
      shift 2
      ;;
    --target)
      target="$2"
      shift 2
      ;;
    --release)
      profile="release"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
if [ -z "$pkg" ] && [ -f Cargo.toml ]; then
  pkg=$(sed -n 's/^name = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)
fi
case "$cmd" in
  check)
    mkdir -p ui/gen
    if [ -f build.rs ] && grep -q 'include_router: true' build.rs; then
      : > ui/gen/router.slint
      : > ui/gen/navigate.slint
      : > ui/gen/exports.slint
    fi
    if [ -f build.rs ] && grep -q 'include_translations: true' build.rs; then
      : > ui/gen/tr.slint
      : > ui/gen/exports.slint
    fi
    exit 0
    ;;
  build)
    if [ -n "$target" ]; then
      out="target/$target/$profile/$pkg"
    else
      out="target/$profile/$pkg"
    fi
    mkdir -p "$(dirname "$out")"
    printf 'fake-binary' > "$out"
    exit 0
    ;;
esac
exit 0
"#,
                self.root.join("cargo.log").display()
            ),
        );
    }

    fn install_fake_nix(&self) {
        self.write_script(
            self.fake_bin.join("nix"),
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'nix 2.0.0-fake'; exit 0; fi\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"accept-flake-config\" ]; then echo false; exit 0; fi\nif [ \"$1\" = \"develop\" ]; then printf '%s\\n' \"$*\" > \"{}\"; exit 0; fi\nexit 0\n",
                self.root.join("nix-develop.log").display()
            ),
        );
        self.write_script(self.fake_bin.join("nix-collect-garbage"), "#!/bin/sh\nexit 0\n");
    }

    fn install_fake_rustc(&self) {
        self.write_script(
            self.fake_bin.join("rustc"),
            "#!/bin/sh\nif [ \"$1\" = \"--target\" ] && [ \"$2\" = \"armv7a-unknown-xous-elf\" ] && [ \"$3\" = \"--print\" ] && [ \"$4\" = \"cfg\" ]; then echo 'target_arch=\"arm\"'; exit 0; fi\nif [ \"$1\" = \"--version\" ]; then echo 'rustc 1.0.0-fake'; exit 0; fi\nexit 0\n",
        );
    }

    fn install_fake_arm_strip(&self) {
        self.write_script(
            self.fake_bin.join("arm-none-eabi-strip"),
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'arm-none-eabi-strip 1.0.0-fake'; exit 0; fi\nout=''\ninput=''\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      out=\"$2\"\n      shift 2\n      ;;\n    --strip-unneeded)\n      shift\n      ;;\n    *)\n      input=\"$1\"\n      shift\n      ;;\n  esac\ndone\ncp \"$input\" \"$out\"\n",
        );
    }

    fn install_fake_git(&self) {
        self.write_script(
            self.fake_bin.join("git"),
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'git version 2.0.0-fake'; exit 0; fi\nif [ \"$1\" = \"init\" ]; then exit 0; fi\nexit 0\n",
        );
    }

    fn install_fake_openssl(&self) {
        self.write_script(
            self.fake_bin.join("openssl"),
            "#!/bin/sh\nif [ \"$1\" = \"version\" ]; then echo 'OpenSSL fake'; exit 0; fi\nif [ \"$1\" = \"ecparam\" ]; then printf 'PRIVATE-KEY'; exit 0; fi\nif [ \"$1\" = \"ec\" ]; then printf 'FAKEDER'; printf '\\x02'; printf '12345678901234567890123456789012'; exit 0; fi\nif [ \"$1\" = \"req\" ]; then out=''; while [ $# -gt 0 ]; do if [ \"$1\" = \"-out\" ]; then out=\"$2\"; shift 2; else shift; fi; done; printf 'CERTIFICATE' > \"$out\"; exit 0; fi\nif [ \"$1\" = \"x509\" ]; then in=''; while [ $# -gt 0 ]; do if [ \"$1\" = \"-in\" ]; then in=\"$2\"; shift 2; else shift; fi; done; printf 'Certificate contents for %s\\n' \"$in\"; exit 0; fi\nexit 1\n",
        );
    }

    fn install_bundle_cosign2(&self) {
        // Emulate signing by prepending a minimal cosign2 header (magic + the
        // binary version at the version offset, zero-padded to the default
        // header size) so `build`'s ensure_cosign2_header guard accepts the elf.
        self.write_script(
            self.bundle.join("bin").join("cosign2"),
            r#"#!/bin/sh
elf=""
version="0.0.0"
while [ $# -gt 0 ]; do
  case "$1" in
    -i) elf="$2"; shift 2 ;;
    --binary-version) version="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ -z "$elf" ]; then
  exit 0
fi
header="$(mktemp)"
dd if=/dev/zero of="$header" bs=2048 count=1 2>/dev/null
printf 'PRM1' | dd of="$header" bs=1 seek=0 conv=notrunc 2>/dev/null
printf '%s' "$version" | dd of="$header" bs=1 seek=22 conv=notrunc 2>/dev/null
cat "$elf" >> "$header"
mv "$header" "$elf"
"#,
        );
    }

    fn install_bundle_asset_tool(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-asset-tool"),
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo 'foundation-asset-tool 1.0.0-fake'
  exit 0
fi
cmd="$1"
source="$2"
destination="$3"
case "$cmd" in
  raw-image-file)
    mkdir -p "$(dirname "$destination")"
    cp "$source" "$destination"
    ;;
  raw-image-dir)
    mkdir -p "$destination"
    base=$(basename "$source")
    stem=${base%.*}
    cp "$source" "$destination/$stem.raw"
    ;;
esac
"#,
        );
    }

    fn install_fake_passport_drive(&self) {
        self.write_script(
            self.fake_bin.join("foundation-passport-drive"),
            r#"#!/bin/sh
log="$(dirname "$0")/../passport-drive.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  if [ -z "$id" ]; then
    continue
  fi
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"name":"get_developer_mode"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"enabled"}]}}\n' "$id"
          ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}]}}\n' "$id"
          ;;
      esac
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
  esac
done
"#,
        );
    }

    fn install_bundle_viewer(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-slint-viewer"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"{}\"\nexit 0\n",
                self.root.join("viewer.log").display()
            ),
        );
    }

    fn install_bundle_theme_compiler(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-theme-compiler"),
            r#"#!/bin/sh
rust_dir=''
while [ $# -gt 0 ]; do
  case "$1" in
    --rust-dir)
      rust_dir="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ -z "$rust_dir" ]; then
  exit 1
fi
mkdir -p "$rust_dir"
printf 'pub mod default_theme;\n' > "$rust_dir/mod.rs"
printf 'pub fn theme() {}\n' > "$rust_dir/default_theme.rs"
"#,
        );
    }

    fn install_bundle_theme_editor(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-theme-editor"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD $*\" >> \"{}\"\nexit 0\n",
                self.root.join("theme-editor.log").display()
            ),
        );
    }

    fn install_bundle_log_viewer(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-keyos-log-viewer"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"{}\"\nexit 0\n",
                self.root.join("log-viewer.log").display()
            ),
        );
    }

    fn install_bundle_simulator(&self) {
        self.write_script(
            self.bundle.join("bin").join("foundation-simulator"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> \"{}\"\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"{}\"\n  case \"$line\" in\n    ping)\n      printf 'ok ping proto=1 caps=run\\n'\n      ;;\n    run\\ *)\n      printf 'ok run launched pid=123\\n'\n      exit 0\n      ;;\n  esac\ndone\n",
                self.root.join("simulator.log").display(),
                self.root.join("simulator-stdin.log").display()
            ),
        );
    }

    fn install_bundle_fatfs_image(&self) {
        self.write_script(
            self.bundle.join("bin").join("fatfs-image"),
            "#!/bin/sh\nif [ \"$1\" = \"create\" ]; then : > \"$2\"; fi\nexit 0\n",
        );
    }

    fn install_path_plugin(&self, name: &str, script: &str) {
        self.write_script(self.fake_bin.join(name), script);
    }

    fn install_home_plugin(&self, name: &str) {
        self.write_script(
            self.home.join(".foundation").join("plugins").join(format!("foundation-{name}")),
            "#!/bin/sh\nexit 0\n",
        );
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
        fs::write(self.app.join("resources").join("icon.svg"), "<svg />\n").unwrap();
        fs::write(
            self.app.join("ui").join("app.slint"),
            "export component AppWindow inherits Window { Text { text: \"smoke\"; } }\n",
        )
        .unwrap();
    }

    fn write_script(&self, path: PathBuf, contents: impl AsRef<str>) {
        fs::write(&path, contents.as_ref()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }
    }

    fn read_log(&self, name: &str) -> String { fs::read_to_string(self.root.join(name)).unwrap_or_default() }
}

impl Drop for TestEnv {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}

fn foundation_bin() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_foundation")) }

fn make_temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("{label}-{unique}"));
    fs::create_dir_all(&root).unwrap();
    root
}

fn stdout(output: &Output) -> String { String::from_utf8_lossy(&output.stdout).to_string() }

fn stderr(output: &Output) -> String { String::from_utf8_lossy(&output.stderr).to_string() }
