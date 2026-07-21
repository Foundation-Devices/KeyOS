// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

pub const SDK_STRING_C: &str = include_str!("sdk_string.c");

pub const LEDGER_SECURE_SDK_NAME: &str = "ledger-secure-sdk";

pub const BASE_HOSTED_SKIP_FILES: &[&str] =
    &["string.c", "syscalls.c", "pic.c", "stack_protector_init.c", "app_metadata.c"];

pub const BASE_STRIP_OBJS: &[&str] =
    &["stack_protector_init.o", "nbgl_stubs.o", "nbgl_fonts.o", "cx_wrappers.o"];

pub struct ArmToolchain {
    pub bash: String,
    pub cc: String,
    pub gccpath: String,
    pub sysroot: Option<String>,
    pub gcc_include: Option<String>,
    pub libc_include: Option<String>,
}

impl ArmToolchain {
    pub fn detect(out_dir: &str) -> Self {
        let bash = command_stdout("which", &["bash"]).unwrap_or_else(|| "/bin/bash".to_string());

        let clang_wrapper_path = Path::new(out_dir).join("clang-wrapper.sh");
        let clang_wrapper = command_stdout("which", &["clang"]).and_then(|wrapper_path| {
            let wrapper_dir = Path::new(&wrapper_path).parent()?.parent()?;
            let orig_cc_path = wrapper_dir.join("nix-support/orig-cc");
            let orig_cc = fs::read_to_string(&orig_cc_path).ok()?.trim().to_string();
            Some(format!("#!/bin/sh\nexec {}/bin/clang \"$@\"\n", orig_cc))
        });

        if let Some(wrapper_content) = clang_wrapper {
            fs::write(&clang_wrapper_path, wrapper_content)
                .unwrap_or_else(|e| panic!("Failed to write {}: {e}", clang_wrapper_path.display()));
            let chmod_out = Command::new("chmod")
                .arg("+x")
                .arg(&clang_wrapper_path)
                .output()
                .unwrap_or_else(|e| panic!("Failed to chmod {}: {e}", clang_wrapper_path.display()));
            if !chmod_out.status.success() {
                panic!(
                    "Failed to chmod {}: {}",
                    clang_wrapper_path.display(),
                    String::from_utf8_lossy(&chmod_out.stderr)
                );
            }
        }

        let cc = if clang_wrapper_path.exists() {
            clang_wrapper_path.to_string_lossy().to_string()
        } else {
            "clang".to_string()
        };

        let gccpath = command_stdout("which", &["arm-none-eabi-gcc"])
            .and_then(|gcc_path| Path::new(&gcc_path).parent().map(|p| format!("{}/", p.display())))
            .unwrap_or_default();
        let sysroot = command_stdout("arm-none-eabi-gcc", &["-print-sysroot"]);
        let gcc_include = command_stdout("arm-none-eabi-gcc", &["-print-file-name=include"]);

        // A sysroot-less toolchain (Ubuntu's gcc-arm-none-eabi) leaves clang
        // without the C library's headers, or the SDK build fails on <stdio.h>.
        let libc_include = sysroot.is_none().then(libc_include_dir).flatten();

        Self { bash, cc, gccpath, sysroot, gcc_include, libc_include }
    }
}

const KEYOS_SDK_DELETE_SRC_FILES: [&str; 5] = [
    "src/svc_call.s",             // Avoid original SVC_Call() as we provide it by FFI from Rust.
    "src/svc_cx_call.s",          // Avoid original SVC_cx_call() as we provide it by FFI from Rust.
    "src/stack_protector_init.c", // Avoid duplicate __stack_chk_fail (provided by KeyOS).
    "src/pic.c",                  // Contains ARM assembly; PIC(x) is defined as no-op in patch.
    "src/syscalls.c",             // Contains ARM assembly (udf instruction); KeyOS provides syscalls.
];

const KEYOS_SDK_LIBAPP_RULE: &str = r#"

# Added by KeyOS build.rs for static library creation
$(LIB_DEPENDENCIES): prepare

LIB_DEPENDENCIES  = $(OBJECT_FILES) Makefile
LIB_DEPENDENCIES += $(APP_CUSTOM_LIB_DEPENDENCIES)

libapp: $(LIB_DEPENDENCIES)
	@echo "[LIB]  $(BIN_DIR)/libapp.a"
	$(L)$(GCCPATH)arm-none-eabi-ar rcs $(BIN_DIR)/libapp.a $(OBJECT_FILES)
"#;

#[derive(Clone, Copy)]
pub struct LedgerSdkOptions {
    pub patch_sdk: fn(&Path),
    pub ensure_nbgl_font_data: bool,
    pub ensure_nbgl_draw_text_override: bool,
    pub extra_source_fixes: Option<fn(&Path)>,
}

impl Default for LedgerSdkOptions {
    fn default() -> Self {
        Self {
            patch_sdk: patch_sdk_common,
            ensure_nbgl_font_data: false,
            ensure_nbgl_draw_text_override: false,
            extra_source_fixes: None,
        }
    }
}

#[derive(Clone, Copy)]
pub enum LedgerAppSubmodules {
    None,
    Init,
}

#[derive(Clone, Copy)]
pub struct LedgerAppOptions<'a> {
    pub patch_app: fn(&Path),
    pub hosted_inline_asm_paths: &'a [&'a str],
    pub submodules: LedgerAppSubmodules,
}

impl<'a> Default for LedgerAppOptions<'a> {
    fn default() -> Self {
        Self {
            patch_app: patch_app_noop,
            hosted_inline_asm_paths: &[],
            submodules: LedgerAppSubmodules::None,
        }
    }
}

pub struct LedgerGlyphOptions<'a> {
    pub app_icon: &'a str,
    pub extra_icons: &'a [PathBuf],
    pub stub_icon_names: &'a [&'a str],
    pub stub_comment: Option<&'a str>,
    pub chain_icon_stub_dir: Option<&'a str>,
}

/// Marker recording which git ref a clone under `out_dir` was made from, kept
/// beside the checkout (not inside it) so patching or `git clean` can't drop it.
fn clone_ref_marker(out_dir: &str, name: &str) -> PathBuf {
    Path::new(out_dir).join(format!("{name}.git-ref"))
}

/// Whether the clone of `name` under `out_dir` was made from `git_ref`.
fn clone_matches_ref(out_dir: &str, name: &str, git_ref: &str) -> bool {
    fs::read_to_string(clone_ref_marker(out_dir, name)).map(|s| s.trim() == git_ref).unwrap_or(false)
}

/// Record the git ref a fresh clone of `name` was made from.
fn record_clone_ref(out_dir: &str, name: &str, git_ref: &str) {
    let _ = fs::write(clone_ref_marker(out_dir, name), git_ref);
}

pub fn prepare_ledger_app(
    out_dir: &str,
    app_name: &str,
    app_git_ref: &str,
    local_path_env_var: &str,
    hosted: bool,
    options: LedgerAppOptions<'_>,
) -> PathBuf {
    let app_path = Path::new(out_dir).join(app_name);
    // Re-clone when the pinned ref changed since the last build. Otherwise the
    // stale checkout is reused and the app is built at the old version even though
    // the pinned ref names the new one, so the reported version (read from the
    // rebuilt source below) and the compiled Settings page would disagree.
    if app_path.exists() && !clone_matches_ref(out_dir, app_name, app_git_ref) {
        let _ = fs::remove_dir_all(&app_path);
    }
    if !app_path.exists() {
        // Use the local mirror only when it actually has the pinned ref; a stale
        // mirror misses newly pinned tags. Filter once here so the app clone and
        // the submodule copy share one validated source: a rejected mirror must
        // not seed submodules from a stale tree while the app comes from upstream.
        let local_app_path =
            find_local_ledger_repo(local_path_env_var, app_name).filter(|p| local_has_ref(p, app_git_ref));
        clone_ledger_app(&app_path, app_name, app_git_ref, local_app_path.as_deref());
        if matches!(options.submodules, LedgerAppSubmodules::Init) {
            prepare_ledger_app_submodules(&app_path, local_app_path.as_deref());
        }
        record_clone_ref(out_dir, app_name, app_git_ref);
    }

    clean_ledger_app_build_dir(&app_path);
    (options.patch_app)(&app_path);
    if hosted {
        for path in options.hosted_inline_asm_paths {
            patch_hosted_arm_inline_asm(&app_path.join(path));
        }
    }

    app_path
}

/// The app's version, read from `APPVERSION_M/N/P` in its upstream Makefile (the
/// same source the app's own Settings page shows). Errs when those fields are
/// absent, so a broken clone fails the build instead of reporting a wrong version.
fn flux_app_version(app_path: &Path) -> Result<String, String> {
    let makefile =
        fs::read_to_string(app_path.join("Makefile")).map_err(|e| format!("Makefile unreadable: {e}"))?;
    let field = |key: &str| {
        makefile.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().to_owned())
        })
    };
    match (field("APPVERSION_M"), field("APPVERSION_N"), field("APPVERSION_P")) {
        (Some(m), Some(n), Some(p)) => Ok(format!("{m}.{n}.{p}")),
        _ => Err("Makefile has no APPVERSION_M/N/P".to_owned()),
    }
}

/// The app's single BOLOS NVM region in `archive`: its symbol name and byte size,
/// measured with `nm_bin` (`--print-size`). BOLOS storage globals are named `N_*`.
/// Panics on none or more than one, since the runtime mirrors exactly one region.
fn sole_nvm_region(archive: &Path, nm_bin: &str) -> (String, usize) {
    let output = Command::new(nm_bin)
        .arg("--print-size")
        .arg(archive)
        .output()
        .unwrap_or_else(|e| panic!("{nm_bin} on {} failed: {e}", archive.display()));
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut regions: Vec<(String, usize)> = listing
        .lines()
        .filter_map(|line| {
            // A defined data symbol prints "addr size type name"; undefined ones lack
            // the size/type columns, so a 4-field split already excludes them.
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [_addr, size, type_, name] = fields[..] else { return None };
            let is_data = matches!(type_, "b" | "B" | "d" | "D" | "r" | "R");
            (is_data && name.starts_with("N_"))
                .then(|| usize::from_str_radix(size, 16).ok().map(|len| (name.to_owned(), len)))
                .flatten()
        })
        .collect();
    regions.sort();
    regions.dedup();
    match regions.as_slice() {
        [(name, len)] => (name.clone(), *len),
        [] => panic!("no N_ NVM region found in {}", archive.display()),
        many => panic!(
            "multiple N_ NVM regions in {} ({}); the runtime mirrors only one",
            archive.display(),
            many.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "),
        ),
    }
}

/// Generate `flux_nvm_gen.rs` in `OUT_DIR` (for the app's `main.rs` to `include!`)
/// exposing the app's BOLOS NVM region as `nvm_base()` and `NVM_LEN`, both taken
/// from `archive` so the mirrored size always matches the C struct instead of a
/// hand-copied number. Call after the archive is built; `nm_bin` is the `nm` for
/// the archive's target (host `nm` for hosted, `arm-none-eabi-nm` for ARM).
/// Emit `flux_app_gen.rs` for the `flux_app!` macro to include: the app's `APP_VERSION` (parsed from
/// its upstream Makefile) and its NVM region (`nvm_base()` / `NVM_LEN`, measured from the compiled
/// archive so the mirrored size can't drift from the C struct). Bundling both keeps a single
/// build-time source of app metadata.
pub fn generate_flux_app_module(out_dir: &str, app_path: &Path, archive: &Path, nm_bin: &str) {
    let version = flux_app_version(app_path)
        .unwrap_or_else(|reason| panic!("flux app version undeterminable: {reason}"));
    let (region, len) = sole_nvm_region(archive, nm_bin);
    let generated = format!(
        "// generated by build.rs -- do not edit\n\
         /// The app's version, from its upstream Makefile; answered to the host's\n\
         /// GET_APP_AND_VERSION probe via `os_registry_get_current_app_tag`.\n\
         pub const APP_VERSION: &str = \"{version}\";\n\
         extern \"C\" {{\n    static mut {region}: u8;\n}}\n\
         /// Base of the app's NVM region; `init_nvm` reads and writes `NVM_LEN` bytes\n\
         /// through it, and `NVM_LEN` is its measured size, so it stays in bounds.\n\
         pub fn nvm_base() -> *mut u8 {{ core::ptr::addr_of_mut!({region}) }}\n\
         pub const NVM_LEN: usize = {len};\n"
    );
    write_if_changed(&Path::new(out_dir).join("flux_app_gen.rs"), &generated);
}

pub fn generate_ledger_glyphs(
    out_dir: &str,
    crate_name: &str,
    app_path: &Path,
    sdk_path: &Path,
    options: LedgerGlyphOptions<'_>,
) {
    copy_app_icon(out_dir, crate_name, app_path, options.app_icon);

    let gen_src_dir = app_path.join("build/flex/gen_src");
    fs::create_dir_all(&gen_src_dir)
        .unwrap_or_else(|e| panic!("Failed to create {}: {e}", gen_src_dir.display()));

    let icon2glyph = sdk_path.join("lib_nbgl/tools/icon2glyph.py");
    let glyphs_h_path = gen_src_dir.join("glyphs.h");
    let glyphs_c_path = gen_src_dir.join("glyphs.c");

    let mut icon_files = Vec::new();
    collect_icon_files(&sdk_path.join("lib_nbgl/glyphs/wallet"), &mut icon_files);
    collect_icon_files(&sdk_path.join("lib_nbgl/glyphs/40px"), &mut icon_files);
    collect_icon_files(&sdk_path.join("lib_nbgl/glyphs/64px"), &mut icon_files);
    icon_files.extend(options.extra_icons.iter().filter(|path| path.exists()).cloned());

    let app_icon_path = app_path.join(options.app_icon);
    if app_icon_path.exists() {
        icon_files.push(app_icon_path);
    }

    let icon_args: Vec<String> = icon_files.iter().map(|p| p.display().to_string()).collect();
    let glyph_out = Command::new("python3")
        .arg(&icon2glyph)
        .arg("--glyphcheader")
        .arg(&glyphs_h_path)
        .arg("--glyphcfile")
        .arg(&glyphs_c_path)
        .args(&icon_args)
        .output();

    match glyph_out {
        Ok(out) if out.status.success() => {
            eprintln!("icon2glyph.py: generated {} icons", icon_files.len());
        }
        Ok(out) => {
            eprintln!("icon2glyph.py failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Err(e) => {
            eprintln!("icon2glyph.py: failed to run: {e}");
        }
    }

    append_glyph_stubs(app_path, &glyphs_h_path, options);
}

/// SDK ref whose NBGL subsystem the emulator uses. From SDK v26 the NBGL engine
/// (obj, draw, fonts, screen, touch, pools) is served by the OS behind syscalls
/// and its sources are gone from the app-side tree; the app-side glue that
/// remains is built for the v26 engine and its changed `nbgl_obj_s` layout. The
/// emulator instead links NBGL into the app, so it needs the whole subsystem at
/// one self-consistent version. When the engine sources are missing, replace all
/// of `lib_nbgl/src` and `lib_nbgl/include` with this ref's, which ships both the
/// engine and matching glue/headers.
const NBGL_ENGINE_GIT_REF: &str = "v25.11.5";

/// A sentinel engine source: present in the pre-v26 tree, absent from v26.
const NBGL_ENGINE_SENTINEL: &str = "lib_nbgl/src/nbgl_obj.c";

/// Reset `lib_nbgl/src` and `lib_nbgl/include` to the clone's committed (pristine
/// SDK) state, so the swap is deterministic regardless of a prior build's edits
/// or an earlier partial restore. Reverts tracked files and drops untracked ones.
fn reset_nbgl_tree(sdk_path: &Path) {
    let paths = ["lib_nbgl/src", "lib_nbgl/include"];
    let checkout = Command::new("git")
        .arg("-C")
        .arg(sdk_path)
        .arg("checkout")
        .arg("--")
        .args(paths)
        .output()
        .unwrap_or_else(|e| panic!("Failed to reset lib_nbgl: {e}"));
    if !checkout.status.success() {
        panic!("Failed to reset lib_nbgl: {}", String::from_utf8_lossy(&checkout.stderr));
    }
    let clean = Command::new("git")
        .arg("-C")
        .arg(sdk_path)
        .args(["clean", "-fdq"])
        .args(paths)
        .output()
        .unwrap_or_else(|e| panic!("Failed to clean lib_nbgl: {e}"));
    if !clean.status.success() {
        panic!("Failed to clean lib_nbgl: {}", String::from_utf8_lossy(&clean.stderr));
    }
}

/// Recursively list `lib_nbgl/src` (or `/include`) at `NBGL_ENGINE_GIT_REF`,
/// returning paths relative to that subdir (e.g. `fonts/nbgl_font_...inc`).
fn nbgl_dir_files_at_ref(sdk_path: &Path, subdir: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(sdk_path)
        .args(["ls-tree", "-r", "--name-only"])
        .arg(format!("{NBGL_ENGINE_GIT_REF}:lib_nbgl/{subdir}"))
        .output()
        .unwrap_or_else(|e| panic!("Failed to list lib_nbgl/{subdir} at {NBGL_ENGINE_GIT_REF}: {e}"));
    if !out.status.success() {
        panic!(
            "Failed to list lib_nbgl/{subdir} at {NBGL_ENGINE_GIT_REF}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

/// Restore one file from `NBGL_ENGINE_GIT_REF` into the working tree.
fn restore_file_at_ref(sdk_path: &Path, rel_path: &str) {
    let out = Command::new("git")
        .arg("-C")
        .arg(sdk_path)
        .arg("show")
        .arg(format!("{NBGL_ENGINE_GIT_REF}:{rel_path}"))
        .output()
        .unwrap_or_else(|e| panic!("Failed to read {rel_path} at {NBGL_ENGINE_GIT_REF}: {e}"));
    if !out.status.success() {
        panic!(
            "Failed to restore {rel_path} from {NBGL_ENGINE_GIT_REF}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let dst = sdk_path.join(rel_path);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("Failed to create {}: {e}", parent.display()));
    }
    fs::write(&dst, &out.stdout).unwrap_or_else(|e| panic!("Failed to write {}: {e}", dst.display()));
}

/// When the NBGL engine sources are absent (SDK v26+), replace the whole
/// `lib_nbgl/src` and `lib_nbgl/include` with `NBGL_ENGINE_GIT_REF`'s, so the
/// linked NBGL subsystem is self-consistent (engine, glue, and the `nbgl_obj_s`
/// layout all match). No-op for SDK trees that still ship the engine.
/// Returns true when the swap was performed (the SDK is v26+ and shipped no
/// engine); false when the tree already ships the engine (pre-v26), leaving it
/// untouched.
fn restore_nbgl_engine_sources(sdk_path: &Path) -> bool {
    reset_nbgl_tree(sdk_path);
    if sdk_path.join(NBGL_ENGINE_SENTINEL).exists() {
        return false;
    }
    for subdir in ["src", "include"] {
        let dir = sdk_path.join("lib_nbgl").join(subdir);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("Failed to create {}: {e}", dir.display()));
        for name in nbgl_dir_files_at_ref(sdk_path, subdir) {
            restore_file_at_ref(sdk_path, &format!("lib_nbgl/{subdir}/{name}"));
        }
    }
    true
}

pub fn prepare_ledger_sdk(
    out_dir: &str,
    sdk_git_ref: &str,
    hosted: bool,
    options: LedgerSdkOptions,
) -> PathBuf {
    let sdk_path = Path::new(out_dir).join(LEDGER_SECURE_SDK_NAME);
    // Re-clone when the pinned SDK ref changed, so a version bump doesn't build
    // the app against the previously cloned SDK.
    if sdk_path.exists() && !clone_matches_ref(out_dir, LEDGER_SECURE_SDK_NAME, sdk_git_ref) {
        let _ = fs::remove_dir_all(&sdk_path);
    }
    if !sdk_path.exists() {
        clone_ledger_sdk(&sdk_path, sdk_git_ref);
        record_clone_ref(out_dir, LEDGER_SECURE_SDK_NAME, sdk_git_ref);
    }

    let nbgl_restored = restore_nbgl_engine_sources(&sdk_path);
    (options.patch_sdk)(&sdk_path);
    ensure_common_sdk_c_fixes(&sdk_path);
    // The restored (v25) NBGL sources need the same font-data and draw-text
    // patches as a natively shipped v25 tree, so apply them whenever the swap
    // ran, independent of the per-app opt-in flags (which predate the restore).
    if nbgl_restored || options.ensure_nbgl_font_data {
        let _ = ensure_nbgl_font_data(&sdk_path);
    }
    if nbgl_restored || options.ensure_nbgl_draw_text_override {
        let _ = ensure_nbgl_draw_text_override(&sdk_path);
    }
    if let Some(extra_source_fixes) = options.extra_source_fixes {
        extra_source_fixes(&sdk_path);
    }

    if hosted {
        use_host_system_string_h(&sdk_path);
        patch_hosted_arm_inline_asm(&sdk_path.join("lib_standard_app/main.c"));
    } else {
        prepare_sdk_for_keyos_arm_build(&sdk_path);
    }

    sdk_path
}

pub fn base_hosted_source_dirs(sdk_path: &Path, app_path: &Path) -> Vec<PathBuf> {
    vec![
        sdk_path.join("lib_nbgl/src"),
        sdk_path.join("lib_ux_nbgl"),
        sdk_path.join("lib_standard_app"),
        sdk_path.join("qrcode/src"),
        sdk_path.join("src"),
        app_path.join("src"),
    ]
}

pub fn base_hosted_skip_paths(sdk_path: &Path) -> Vec<PathBuf> {
    vec![sdk_path.join("lib_standard_app/main.c")]
}

pub fn append_generated_glyphs_c(c_files: &mut Vec<PathBuf>, app_path: &Path) {
    let glyphs_c = app_path.join("build/flex/gen_src/glyphs.c");
    if glyphs_c.exists() {
        c_files.push(glyphs_c);
    }
}

pub fn collect_c_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    collect_c_dirs_inner(root, &mut dirs);
    dirs.sort();
    dirs
}

pub fn collect_c_files(
    dirs: &[PathBuf],
    skip_files: &[&str],
    skip_paths: &[PathBuf],
    skip_suffixes: &[&str],
) -> Vec<PathBuf> {
    let mut c_files = Vec::new();
    for dir in dirs {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "c") {
                    continue;
                }
                let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
                    continue;
                };
                if skip_files.iter().any(|skip| *skip == name.as_ref())
                    || skip_paths.iter().any(|skip| skip == &path)
                    || skip_suffixes.iter().any(|suffix| name.ends_with(suffix))
                {
                    continue;
                }
                c_files.push(path);
            }
        }
    }
    c_files.sort();
    c_files
}

pub fn base_hosted_cc_build() -> cc::Build {
    let mut build = cc::Build::new();
    build
        .warnings(false)
        .flag("-fshort-enums")
        .flag("-fPIC")
        .flag("-Wno-incompatible-pointer-types")
        .flag("-Wno-unused-parameter")
        .flag("-Wno-implicit-function-declaration");
    build
}

pub fn apply_hosted_base_flag_defines(build: &mut cc::Build) {
    let flag_defines: &[&str] = &[
        "TARGET_FLEX",
        "HAVE_NBGL",
        "HAVE_SE_TOUCH",
        "NBGL_USE_CASE",
        "NBGL_PAGE",
        "SCREEN_SIZE_WALLET",
        "HAVE_FAST_HOLD_TO_APPROVE",
        "NBGL_QRCODE",
        "NBGL_KEYBOARD",
        "NBGL_KEYPAD",
        "OS_IO_SEPROXYHAL",
        "HAVE_PIEZO_SOUND",
        "HAVE_IO_USB",
        "HAVE_USB_APDU",
        "HAVE_SPRINTF",
        "HAVE_SNPRINTF_FORMAT_U",
        "STANDARD_APP_SYNC_RAPDU",
        "HAVE_ECC",
        "HAVE_ECC_WEIERSTRASS",
        "HAVE_ECC_TWISTED_EDWARDS",
        "HAVE_ECC_MONTGOMERY",
        "HAVE_SECP256K1_CURVE",
        "HAVE_SECP256R1_CURVE",
        "HAVE_SECP384R1_CURVE",
        "HAVE_SECP521R1_CURVE",
        "HAVE_FR256V1_CURVE",
        "HAVE_STARK256_CURVE",
        "HAVE_BRAINPOOL_P256R1_CURVE",
        "HAVE_BRAINPOOL_P256T1_CURVE",
        "HAVE_BRAINPOOL_P320R1_CURVE",
        "HAVE_BRAINPOOL_P320T1_CURVE",
        "HAVE_BRAINPOOL_P384R1_CURVE",
        "HAVE_BRAINPOOL_P384T1_CURVE",
        "HAVE_BRAINPOOL_P512R1_CURVE",
        "HAVE_BRAINPOOL_P512T1_CURVE",
        "HAVE_BLS12_381_G1_CURVE",
        "HAVE_CV25519_CURVE",
        "HAVE_CV448_CURVE",
        "HAVE_ED25519_CURVE",
        "HAVE_ED448_CURVE",
        "HAVE_ECDH",
        "HAVE_ECDSA",
        "HAVE_EDDSA",
        "HAVE_ECSCHNORR",
        "HAVE_X25519",
        "HAVE_X448",
        "HAVE_HASH",
        "HAVE_RIPEMD160",
        "HAVE_SHA224",
        "HAVE_SHA256",
        "HAVE_SHA3",
        "HAVE_SHA384",
        "HAVE_SHA512",
        "HAVE_SHA512_WITH_BLOCK_ALT_METHOD",
        "HAVE_SHA512_WITH_BLOCK_ALT_METHOD_M0",
        "HAVE_BLAKE2",
        "HAVE_HMAC",
        "HAVE_PBKDF2",
        "HAVE_AES",
        "HAVE_AES_GCM",
        "HAVE_AES_SIV",
        "HAVE_CMAC",
        "HAVE_MATH",
        "HAVE_RNG",
        "HAVE_RNG_RFC6979",
        "HAVE_RNG_SP800_90A",
        "HAVE_CRC",
        "HAVE_NES_CRYPT",
        "HAVE_ST_AES",
        "NATIVE_LITTLE_ENDIAN",
        "LINUX_SIMU",
        "KEYOS_HOSTED",
    ];
    apply_hosted_flag_defines(build, flag_defines);
}

pub fn apply_hosted_io_value_defines(build: &mut cc::Build) {
    let value_defines: &[(&str, &str)] = &[
        ("TARGET_NAME", "TARGET_FLEX"),
        ("IO_SEPROXYHAL_BUFFER_SIZE_B", "300"),
        ("OS_IO_SEPH_BUFFER_SIZE", "272"),
        ("IO_HID_EP_LENGTH", "64"),
        ("IO_USB_MAX_ENDPOINTS", "4"),
        ("USB_SEGMENT_SIZE", "64"),
    ];
    apply_hosted_value_defines(build, value_defines);
}

pub fn apply_hosted_flag_defines(build: &mut cc::Build, defines: &[&str]) {
    for define in defines {
        build.define(define, None);
    }
}

pub fn apply_hosted_value_defines(build: &mut cc::Build, defines: &[(&str, &str)]) {
    for (key, value) in defines {
        build.define(key, Some(*value));
    }
}

pub fn apply_common_hosted_includes(build: &mut cc::Build, sdk_path: &Path, app_path: &Path) {
    build
        .include(sdk_path)
        .include(sdk_path.join("target/flex/include"))
        .include(sdk_path.join("include"))
        .include(sdk_path.join("lib_nbgl/include"))
        .include(sdk_path.join("lib_nbgl/include/fonts"))
        .include(sdk_path.join("lib_standard_app"))
        .include(sdk_path.join("lib_ux_nbgl"))
        .include(sdk_path.join("lib_cxng/include"))
        .include(sdk_path.join("lib_cxng/src"))
        .include(sdk_path.join("io/include"))
        .include(sdk_path.join("io_legacy/include"))
        .include(sdk_path.join("protocol/include"))
        .include(sdk_path.join("lib_stusb/include"))
        .include(sdk_path.join("lib_stusb_impl/include"))
        .include(sdk_path.join("lib_alloc"))
        .include(sdk_path.join("qrcode/include"))
        .include(app_path.join("src"))
        .include(app_path.join("build/flex/gen_src"));
}

/// `--sysroot`/`-isystem` flags for the C library and compiler headers; every
/// ARM C compile must apply them. The libc dir comes before GCC's so its own
/// stdint.h wins, or inttypes.h macros like PRId64 break.
pub fn arm_include_flags(toolchain: &ArmToolchain) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(sysroot) = &toolchain.sysroot {
        flags.push(format!("--sysroot={sysroot}"));
    }
    if let Some(include) = &toolchain.libc_include {
        flags.push(format!("-isystem{include}"));
    }
    if let Some(include) = &toolchain.gcc_include {
        flags.push(format!("-isystem{include}"));
    }
    flags
}

pub fn base_arm_cflags(toolchain: &ArmToolchain, sdk_path: &Path, appname: &str) -> Vec<String> {
    let mut cflags: Vec<String> = [
        "--target=arm-none-eabi",
        "-march=armv7-a",
        "-mthumb",
        "-mfloat-abi=soft",
        "-fPIC",
        "-fshort-enums",
        "-Oz",
        "-ffunction-sections",
        "-fdata-sections",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    cflags.push(format!("-DAPPNAME=\\\"{appname}\\\""));
    cflags.extend(arm_include_flags(toolchain));
    for include in [
        sdk_path.join("lib_nbgl/include/fonts"),
        sdk_path.join("lib_alloc"),
        sdk_path.join("io/include"),
        sdk_path.join("io_legacy/include"),
        sdk_path.join("protocol/include"),
        sdk_path.join("lib_stusb/include"),
        sdk_path.join("lib_stusb_impl/include"),
    ] {
        cflags.push(format!("-I{}", include.display()));
    }
    cflags
}

pub fn run_make_libapp(app_path: &Path, sdk_path: &Path, toolchain: &ArmToolchain, cflags: &str) {
    let out = Command::new("make")
        .current_dir(app_path)
        .env("BOLOS_SDK", sdk_path)
        .env("TARGET", "flex")
        .arg(format!("SHELL={}", toolchain.bash))
        .arg(format!("GCCPATH={}", toolchain.gccpath))
        .arg(format!("CC={}", toolchain.cc))
        .arg(format!("CFLAGS={cflags}"))
        .arg("libapp")
        .output()
        .unwrap_or_else(|e| panic!("Failed to run make libapp in {}: {e}", app_path.display()));
    if !out.status.success() {
        panic!("{}", String::from_utf8_lossy(&out.stderr));
    }
}

pub fn libapp_path(app_path: &Path) -> PathBuf { app_path.join("build/flex/bin/libapp.a") }

pub fn strip_libapp_objects(libapp: &Path, objs: &[&str]) {
    for obj in objs {
        Command::new("ar").arg("d").arg(libapp).arg(obj).output().ok();
    }
}

pub fn compile_nbgl_arm_objects(
    toolchain: &ArmToolchain,
    sdk_path: &Path,
    app_path: &Path,
    appname: &str,
) -> Vec<PathBuf> {
    let nbgl_src_dir = sdk_path.join("lib_nbgl/src");
    let nbgl_obj_dir = app_path.join("build/flex/bin/nbgl_objs");
    fs::create_dir_all(&nbgl_obj_dir)
        .unwrap_or_else(|e| panic!("Failed to create {}: {e}", nbgl_obj_dir.display()));

    let include_args: Vec<String> = common_arm_include_dirs(sdk_path, app_path)
        .into_iter()
        .map(|path| format!("-I{}", path.display()))
        .collect();
    let mut nbgl_defines: Vec<String> = [
        "-DTARGET_FLEX",
        "-DHAVE_NBGL",
        "-DHAVE_SE_TOUCH",
        "-DSCREEN_SIZE_WALLET",
        "-DNBGL_USE_CASE",
        "-DNBGL_QRCODE",
        "-DNBGL_KEYBOARD",
        "-DNBGL_KEYPAD",
        "-DHAVE_SPRINTF",
        "-DHAVE_SNPRINTF_FORMAT_U",
        "-DHAVE_PIEZO_SOUND",
        "-DHAVE_LANGUAGE_PACK",
        "-DOS_IO_SEPROXYHAL",
        "-DHAVE_IO_USB",
        "-DHAVE_USB_APDU",
        "-DHAVE_BAGL_FONT_INTER_REGULAR_28PX",
        "-DHAVE_BAGL_FONT_INTER_SEMIBOLD_28PX",
        "-DHAVE_BAGL_FONT_INTER_MEDIUM_36PX",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    nbgl_defines.push(format!("-DAPPNAME=\"{appname}\""));

    let nbgl_c_files = [
        "nbgl_obj.c",
        "nbgl_obj_pool.c",
        "nbgl_obj_keyboard.c",
        "nbgl_obj_keypad.c",
        "nbgl_screen.c",
        "nbgl_touch.c",
        "nbgl_fonts.c",
        "nbgl_draw.c",
    ];

    let mut objs = Vec::new();
    for c_file in &nbgl_c_files {
        let src = nbgl_src_dir.join(c_file);
        if !src.exists() {
            continue;
        }
        let obj = nbgl_obj_dir.join(c_file.replace(".c", ".o"));
        let out = Command::new(&toolchain.cc)
            .arg("--target=arm-none-eabi")
            .arg("-march=armv7-a")
            .arg("-mthumb")
            .arg("-mfloat-abi=soft")
            .arg("-fPIC")
            .arg("-fshort-enums")
            .arg("-Os")
            .arg("-Wno-implicit-function-declaration")
            .arg("-c")
            .args(&nbgl_defines)
            .args(&include_args)
            .args(arm_include_flags(toolchain))
            .arg("-o")
            .arg(&obj)
            .arg(&src)
            .output()
            .unwrap_or_else(|e| panic!("Failed to compile NBGL {c_file}: {e}"));
        if !out.status.success() {
            panic!("Failed to compile NBGL {}: {}", c_file, String::from_utf8_lossy(&out.stderr));
        }
        objs.push(obj);
    }
    objs
}

pub fn ar_add(libapp: &Path, objs: &[PathBuf]) {
    for obj in objs {
        Command::new("ar").arg("r").arg(libapp).arg(obj).output().ok();
    }
}

pub fn emit_app_link_directives(app_path: &Path) {
    println!("cargo:rustc-link-search=native={}/build/flex/bin", app_path.display());
    println!("cargo:rustc-link-lib=static=app");
}

fn command_stdout(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

fn command_stderr(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (!stderr.is_empty()).then_some(stderr)
}

/// The C library's header dir, read from the compiler's own `<...>` include
/// search so it holds across distro layouts. Returns the first dir with `stdio.h`.
fn libc_include_dir() -> Option<String> {
    let search = command_stderr("arm-none-eabi-gcc", &["-E", "-Wp,-v", "-xc", "/dev/null"])?;
    let mut in_list = false;
    for line in search.lines() {
        if line.starts_with("#include <...> search starts here:") {
            in_list = true;
        } else if line.starts_with("End of search list.") {
            break;
        } else if in_list {
            let dir = Path::new(line.trim());
            if dir.join("stdio.h").exists() {
                return Some(
                    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).to_string_lossy().into_owned(),
                );
            }
        }
    }
    None
}

fn collect_c_dirs_inner(dir: &Path, dirs: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let mut has_c = false;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_c_dirs_inner(&path, dirs);
            } else if path.extension().is_some_and(|e| e == "c") {
                has_c = true;
            }
        }
    }
    if has_c {
        dirs.push(dir.to_path_buf());
    }
}

fn common_arm_include_dirs(sdk_path: &Path, app_path: &Path) -> Vec<PathBuf> {
    vec![
        sdk_path.join("target/flex/include"),
        sdk_path.join("include"),
        sdk_path.join("lib_nbgl/include"),
        sdk_path.join("lib_nbgl/include/fonts"),
        sdk_path.join("lib_ux_nbgl"),
        sdk_path.join("lib_cxng/include"),
        sdk_path.join("lib_cxng/src"),
        sdk_path.join("lib_standard_app"),
        sdk_path.join("io/include"),
        sdk_path.join("io_legacy/include"),
        sdk_path.join("protocol/include"),
        sdk_path.join("lib_stusb/include"),
        sdk_path.join("lib_stusb_impl/include"),
        sdk_path.join("lib_alloc"),
        sdk_path.join("qrcode/include"),
        app_path.join("src"),
        app_path.join("include"),
        app_path.join("build/flex/gen_src"),
    ]
}

fn clone_ledger_sdk(sdk_path: &Path, sdk_git_ref: &str) {
    // Use the local mirror only when it has both the pinned SDK ref and the NBGL
    // engine ref. Missing the SDK ref fails the checkout below; missing the NBGL
    // ref (e.g. a shallow/single-tag mirror with v26 but not v25.11.5) later
    // panics restore_nbgl_engine_sources on its `git ls-tree NBGL_ENGINE_GIT_REF`.
    // If either is absent, fall back to the upstream clone, which has both.
    let local_sdk_path = find_local_ledger_repo("LEDGER_SDK_PATH", LEDGER_SECURE_SDK_NAME)
        .filter(|p| local_has_ref(p, sdk_git_ref) && local_has_ref(p, NBGL_ENGINE_GIT_REF));

    let mut clone_cmd = Command::new("git");
    clone_cmd.arg("clone");
    if let Some(local_path) = local_sdk_path {
        clone_cmd.arg("--local").arg("--no-hardlinks").arg(local_path);
    } else {
        clone_cmd.arg(format!("https://github.com/LedgerHQ/{LEDGER_SECURE_SDK_NAME}.git"));
    }
    clone_cmd.arg(sdk_path);
    let clone_out = clone_cmd.output().unwrap_or_else(|e| panic!("Failed to clone Ledger SDK: {e}"));
    if !clone_out.status.success() {
        panic!("Failed to clone Ledger SDK: {}", String::from_utf8_lossy(&clone_out.stderr));
    }

    let checkout_out = Command::new("git")
        .arg("checkout")
        .arg(sdk_git_ref)
        .current_dir(sdk_path)
        .output()
        .unwrap_or_else(|e| panic!("Failed to checkout Ledger SDK ref {sdk_git_ref}: {e}"));
    if !checkout_out.status.success() {
        panic!(
            "Failed to checkout Ledger SDK ref {sdk_git_ref}: {}",
            String::from_utf8_lossy(&checkout_out.stderr)
        );
    }
}

fn patch_app_noop(_app_path: &Path) {}

fn copy_app_icon(out_dir: &str, crate_name: &str, app_path: &Path, app_icon: &str) {
    let Some(ext) = Path::new(app_icon).extension() else {
        return;
    };
    let icon_dst = Path::new(out_dir).ancestors().nth(3).unwrap().join(format!(
        "{}_icon.{}",
        crate_name,
        ext.to_string_lossy()
    ));
    if icon_dst.exists() {
        return;
    }

    let icon_src = app_path.join(app_icon);
    if icon_src.exists() {
        fs::copy(&icon_src, &icon_dst).unwrap_or_else(|e| {
            panic!("Failed to copy {} to {}: {e}", icon_src.display(), icon_dst.display())
        });
    } else {
        eprintln!("Warning: app icon not found at {}", icon_src.display());
    }
}

pub fn collect_icon_files(dir: &Path, icon_files: &mut Vec<PathBuf>) {
    let start = icon_files.len();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_supported_icon_file(&path) {
                icon_files.push(path);
            }
        }
    }
    // read_dir order is filesystem-dependent; sort the entries this call appended so
    // the generated glyph sources are reproducible, without reordering any entries the
    // caller collected from earlier directories. Matches collect_c_files/collect_c_dirs.
    icon_files[start..].sort();
}

fn is_supported_icon_file(path: &Path) -> bool {
    path.extension().map_or(false, |e| e == "png" || e == "bmp" || e == "gif")
}

fn append_glyph_stubs(app_path: &Path, glyphs_h_path: &Path, options: LedgerGlyphOptions<'_>) {
    if options.stub_icon_names.is_empty() && options.chain_icon_stub_dir.is_none() {
        return;
    }
    if !glyphs_h_path.exists() {
        return;
    }

    let mut h = fs::read_to_string(glyphs_h_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", glyphs_h_path.display()));
    if let Some(comment) = options.stub_comment {
        h.push('\n');
        h.push_str(comment);
        h.push('\n');
    }

    if let Some(dir) = options.chain_icon_stub_dir {
        let glyphs_dir = app_path.join(dir);
        if let Ok(entries) = fs::read_dir(&glyphs_dir) {
            let mut names: Vec<String> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with("chain_") && name.contains("_64px") && name.ends_with(".gif") {
                        Some(format!("C_{}", name.trim_end_matches(".gif")))
                    } else {
                        None
                    }
                })
                .collect();
            names.sort();

            for symbol in names {
                push_glyph_stub(&mut h, &symbol);
            }
        }
    }

    for symbol in options.stub_icon_names {
        push_glyph_stub(&mut h, symbol);
    }

    fs::write(glyphs_h_path, h)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", glyphs_h_path.display()));
}

fn push_glyph_stub(header: &mut String, symbol: &str) {
    if header.contains(symbol) {
        return;
    }
    header.push_str(&format!(
        "static const uint8_t {symbol}_bitmap[] = {{0}};\n\
         static const nbgl_icon_details_t {symbol} = {{0, 0, 0, 0, NULL}};\n"
    ));
}

fn find_local_ledger_repo(local_path_env_var: &str, repo_name: &str) -> Option<PathBuf> {
    env::var(local_path_env_var).ok().map(PathBuf::from).filter(|p| p.exists()).or_else(|| {
        env::var("HOME")
            .ok()
            .map(|home| Path::new(&home).join("Prime/Ledger").join(repo_name))
            .filter(|p| p.exists())
    })
}

/// Whether a local git repo has `git_ref` available so a `--local` clone can
/// check it out. A stale local mirror misses newly pinned tags; the caller then
/// clones from upstream instead of failing.
fn local_has_ref(repo: &Path, git_ref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{git_ref}^{{commit}}"))
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn clone_ledger_app(app_path: &Path, app_name: &str, app_git_ref: &str, local_app_path: Option<&Path>) {
    // Precondition: `local_app_path`, if set, is a mirror already known to hold
    // the pinned ref; the caller drops stale mirrors so this clone and the
    // submodule copy share one validated source.
    let mut clone_cmd = Command::new("git");
    clone_cmd.arg("clone").arg("--branch").arg(app_git_ref).arg("--single-branch");
    if let Some(local_path) = local_app_path {
        clone_cmd.arg("--local").arg("--no-hardlinks").arg(local_path);
    } else {
        clone_cmd.arg(format!("https://github.com/LedgerHQ/{app_name}.git"));
    }
    clone_cmd.arg(app_path);

    let clone_out =
        clone_cmd.output().unwrap_or_else(|e| panic!("Failed to clone Ledger app {app_name}: {e}"));
    if !clone_out.status.success() {
        panic!("Failed to clone Ledger app {app_name}: {}", String::from_utf8_lossy(&clone_out.stderr));
    }
}

fn prepare_ledger_app_submodules(app_path: &Path, local_app_path: Option<&Path>) {
    let gitmodules_path = app_path.join(".gitmodules");
    if !gitmodules_path.exists() {
        return;
    }

    let content = fs::read_to_string(&gitmodules_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", gitmodules_path.display()));
    let content = content.replace("git@github.com:", "https://github.com/");
    fs::write(&gitmodules_path, &content)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", gitmodules_path.display()));

    if let Some(local_app_path) = local_app_path {
        copy_submodules_from_local(app_path, local_app_path, &content);
        return;
    }

    let submodule_out = Command::new("git")
        .arg("submodule")
        .arg("update")
        .arg("--init")
        .current_dir(app_path)
        .output()
        .unwrap_or_else(|e| panic!("Failed to init Ledger app submodules: {e}"));
    if !submodule_out.status.success() {
        panic!("Failed to init Ledger app submodules: {}", String::from_utf8_lossy(&submodule_out.stderr));
    }
}

fn copy_submodules_from_local(app_path: &Path, local_app_path: &Path, gitmodules_content: &str) {
    for line in gitmodules_content.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("path = ") {
            let src = local_app_path.join(path);
            if !src.exists() {
                continue;
            }

            let dest = app_path.join(path);
            let dest_empty = dest.read_dir().ok().map(|mut d| d.next().is_none()).unwrap_or(true);
            if dest.exists() && !dest_empty {
                continue;
            }

            let _ = Command::new("rm").arg("-rf").arg(&dest).output();
            let copy_out = Command::new("cp")
                .arg("-a")
                .arg(&src)
                .arg(&dest)
                .output()
                .unwrap_or_else(|e| panic!("Failed to copy Ledger app submodule {}: {e}", src.display()));
            if !copy_out.status.success() {
                panic!(
                    "Failed to copy Ledger app submodule {}: {}",
                    src.display(),
                    String::from_utf8_lossy(&copy_out.stderr)
                );
            }
        }
    }
}

fn clean_ledger_app_build_dir(app_path: &Path) {
    let build_dir = app_path.join("build");
    match fs::remove_dir_all(&build_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => panic!("Failed to remove {}: {e}", build_dir.display()),
    }
}

/// Write file content only if it differs or doesn't exist.
pub fn write_if_changed(path: &Path, content: &str) {
    if path.exists() {
        if let Ok(existing) = fs::read_to_string(path) {
            if existing == content {
                return;
            }
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
}

/// Replace first occurrence of `from` with `to` in a file.
///
/// This is idempotent: it skips files that already contain `to`.
pub fn replace_in_file(path: &Path, from: &str, to: &str) {
    if !path.exists() {
        return;
    }
    let content =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
    if content.contains(to) {
        return;
    }
    if !content.contains(from) {
        eprintln!("Warning: pattern not found in {}: {:?}", path.display(), &from[..from.len().min(60)]);
        return;
    }
    let patched = content.replacen(from, to, 1);
    fs::write(path, patched).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
}

pub fn use_host_system_string_h(sdk_path: &Path) {
    let string_h = sdk_path.join("include/string.h");
    if !string_h.exists() {
        return;
    }

    let backup = sdk_path.join("include/string.h.bak");
    if backup.exists() {
        fs::remove_file(&string_h).unwrap_or_else(|e| panic!("Failed to remove {}: {e}", string_h.display()));
    } else {
        fs::rename(&string_h, &backup).unwrap_or_else(|e| {
            panic!("Failed to rename {} to {}: {e}", string_h.display(), backup.display())
        });
    }
}

pub fn patch_hosted_arm_inline_asm(path: &Path) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    if !content.contains("__asm volatile(\"cpsie i\")") {
        return;
    }

    let patched =
        content.replace("__asm volatile(\"cpsie i\")", "/* cpsie i - patched out for hosted build */");
    fs::write(path, patched).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
}

pub fn patch_sdk_common(sdk_path: &Path) {
    let os_pic = sdk_path.join("include/os_pic.h");
    write_if_changed(
        &os_pic,
        "#pragma once\n\
         \n\
         // Position-independent code reference\n\
         // Function that align the dereferenced value in a rom struct to use it depending on the execution\n\
         // address. Can be used even if code is executing at the same place where it had been linked.\n\
         #if defined(HAVE_BOLOS) && !defined(BOLOS_OS_UPGRADER_APP) || 1\n\
         #define PIC(x) (x)\n\
         #endif\n\
         #ifndef PIC\n\
         #define PIC(x) pic((void *) x)\n\
         void *pic(void *linked_address);\n\
         #endif\n\
         void pic_init(void *pic_flash_start, void *pic_ram_start);\n",
    );

    let string_h = sdk_path.join("include/string.h");
    if !string_h.exists() {
        write_if_changed(
            &string_h,
            "#pragma once\n\
             \n\
             #define NULL ((void*)0)\n\
             \n\
             typedef unsigned int size_t;\n\
             \n\
             void *memcpy(void *dest, const void *src, size_t len);\n\
             void *memset(void *dest, int val, size_t len);\n\
             int memcmp(const void *dest, const void *src, size_t len);\n\
             void *memchr(const void *src, int val, size_t len);\n\
             void *memmove(void *dest, const void *src, size_t len);\n\
             void explicit_bzero(void *s, size_t len);\n\
             size_t strlen(const char *str);\n\
             size_t strnlen(const char *str, size_t len);\n\
             char *strcpy(char *dest, const char *src);\n\
             char *strncpy(char *dest, const char *src, size_t len);\n\
             size_t strlcpy(char *dest, const char *src, size_t len);\n\
             char *strcat(char *dest, const char *src);\n\
             size_t strlcat(char *dest, const char *src, size_t len);\n\
             int strcmp(const char *p1, const char *p2);\n\
             int strncmp(const char *p1, const char *p2, size_t len);\n\
             char *strchr(const char *s, int c);\n",
        );
    }

    let sjlj = sdk_path.join("src/sjlj.s");
    if !sjlj.exists() {
        write_if_changed(
            &sjlj,
            "        .syntax unified\n\
             \n\
                     .global setjmp\n\
                     .text\n\
                     .thumb_func\n\
             setjmp:\n\
                stmia   r0!, {r4, r5, r6, r7}\n\
                mov     r1, r8\n\
                mov     r2, r9\n\
                mov     r3, r10\n\
                mov     r4, r11\n\
                mov     r5, sp\n\
                mov     r6, lr\n\
                stmia   r0!, {r1, r2, r3, r4, r5, r6}\n\
                subs    r0, #40\n\
                ldmia   r0!, {r4, r5, r6, r7}\n\
                movs   r0, #0\n\
                bx      lr\n\
             \n\
                     .global longjmp\n\
                     .text\n\
                     .thumb_func\n\
             \n\
              longjmp:\n\
                adds   r0, #16\n\
                ldmia   r0!, {r2, r3, r4, r5, r6}\n\
                mov     r8, r2\n\
                mov     r9, r3\n\
                mov     r10, r4\n\
                mov     r11, r5\n\
                mov     sp, r6\n\
                ldmia   r0!, {r3}\n\
                subs    r0, #40\n\
                ldmia   r0!, {r4, r5, r6, r7}\n\
                adds    r0, r1, #0\n\
                bne     longjmp_ret_ok\n\
                movs    r0, #1\n\
              longjmp_ret_ok:\n\
                bx      r3\n\
                     .end\n",
        );
    }

    let string_c = sdk_path.join("src/string.c");
    if !string_c.exists() {
        write_if_changed(&string_c, SDK_STRING_C);
    }
}

pub fn prepare_sdk_for_keyos_arm_build(sdk_path: &Path) {
    patch_sdk_makefile_for_keyos_static_lib(sdk_path);
    disable_ledger_manifest_use_cases(sdk_path);
    ensure_keyos_app_config_h(sdk_path);
    remove_keyos_replaced_sdk_sources(sdk_path);
}

fn patch_sdk_makefile_for_keyos_static_lib(sdk_path: &Path) {
    let makefile_path = sdk_path.join("Makefile.rules_generic");
    let mut content = fs::read_to_string(&makefile_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", makefile_path.display()));

    if !content.contains("libapp:") {
        content.push_str(KEYOS_SDK_LIBAPP_RULE);
    }
    content = content.replace("SHELL =       /bin/bash", "SHELL ?=      /bin/bash");

    fs::write(&makefile_path, content)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", makefile_path.display()));
}

fn disable_ledger_manifest_use_cases(sdk_path: &Path) {
    let makefile_path = sdk_path.join("Makefile.rules");
    if !makefile_path.exists() {
        return;
    }

    let mut content = fs::read_to_string(&makefile_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", makefile_path.display()));
    if !content.contains("ledger-manifest") {
        return;
    }

    // KeyOS builds statically and has no `ledger-manifest` CLI, so neutralize the
    // two `$(shell ledger-manifest ...)` calls the SDK Makefile makes to read the
    // app's use-cases. Match the shell-call text, not the surrounding indentation,
    // which differs across SDK versions.
    let use_cases_shell =
        r#"$(shell ledger-manifest "$(APP_MANIFEST_FILE)" -ouc -j | jq -r 'keys | join(" ")')"#;
    let flags_shell = r#"$(shell ledger-manifest "$(APP_MANIFEST_FILE)" -ouc $1)"#;
    if !content.contains(use_cases_shell) && !content.contains(flags_shell) {
        return;
    }
    content = content.replace(use_cases_shell, "").replace(flags_shell, "");
    fs::write(&makefile_path, content)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", makefile_path.display()));
}

fn ensure_keyos_app_config_h(sdk_path: &Path) {
    write_if_changed(
        &sdk_path.join("include/app_config.h"),
        "#ifndef APP_CONFIG_H_\n\
         #define APP_CONFIG_H_\n\
         \n\
         #ifdef BOLOS_OS_UPGRADER_APP\n\
         #include \"osu_defines.h\"\n\
         #endif\n\
         \n\
         #endif /* !APP_CONFIG_H_ */\n",
    );
}

fn remove_keyos_replaced_sdk_sources(sdk_path: &Path) {
    for relative_path in KEYOS_SDK_DELETE_SRC_FILES {
        let path = sdk_path.join(relative_path);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => panic!("Failed to remove {}: {e}", path.display()),
        }
    }
}

pub fn ensure_sdk_string_c_includes(sdk_path: &Path) -> Result<(), String> {
    let string_c = sdk_path.join("src/string.c");
    if !string_c.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&string_c).map_err(|e| format!("Failed to read {}: {e}", string_c.display()))?;
    if content.contains("#include <stdarg.h>") && content.contains("#include <stdio.h>") {
        return Ok(());
    }

    let insert = "\n#include <stdarg.h>\n#include <stdio.h>";
    if let Some(pos) = content.find("#include \"string.h\"") {
        let line_end = content[pos..].find('\n').map(|i| pos + i).unwrap_or(content.len());
        content.insert_str(line_end, insert);
    } else {
        content.insert_str(0, "#include \"string.h\"\n");
        content.insert_str(0, insert);
    }

    fs::write(&string_c, content).map_err(|e| format!("Failed to write {}: {e}", string_c.display()))?;
    Ok(())
}

pub fn ensure_sdk_sprintf_no_vsnprintf(sdk_path: &Path) -> Result<(), String> {
    let string_c = sdk_path.join("src/string.c");
    if !string_c.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&string_c).map_err(|e| format!("Failed to read {}: {e}", string_c.display()))?;

    if !content.contains("int sprintf") {
        return Ok(());
    }

    let new_sprintf = r#"int sprintf(char *str, const char *format, ...)
{
    va_list ap;
    va_start(ap, format);
    int n = vsnprintf(str, (size_t)-1, format, ap);
    va_end(ap);
    return n;
}
"#;

    let start = content.find("int sprintf").ok_or_else(|| "Failed to find sprintf".to_string())?;
    let end_rel =
        content[start..].find("\n}\n").ok_or_else(|| "Failed to find end of sprintf".to_string())?;
    let end = start + end_rel + 3;
    content.replace_range(start..end, new_sprintf);

    fs::write(&string_c, content).map_err(|e| format!("Failed to write {}: {e}", string_c.display()))?;
    Ok(())
}

pub fn ensure_sdk_vsnprintf(sdk_path: &Path) -> Result<(), String> {
    let os_printf = sdk_path.join("src/os_printf.c");
    if !os_printf.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&os_printf).map_err(|e| format!("Failed to read {}: {e}", os_printf.display()))?;
    if content.contains("int vsnprintf(") {
        return Ok(());
    }

    let sig = "int snprintf(char *str, size_t str_size, const char *format, ...)";
    let start =
        content.find(sig).ok_or_else(|| "Failed to find snprintf signature in os_printf.c".to_string())?;
    let brace_start = content[start..]
        .find('{')
        .map(|i| start + i)
        .ok_or_else(|| "Failed to find snprintf body start in os_printf.c".to_string())?;

    let mut depth = 0usize;
    let mut end = None;
    for (i, ch) in content[brace_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(brace_start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| "Failed to find end of snprintf body in os_printf.c".to_string())?;

    let snprintf_block = &content[start..end];
    let mut vsnprintf_block = snprintf_block
        .replace(sig, "int vsnprintf(char *str, size_t str_size, const char *format, va_list args)");
    vsnprintf_block = vsnprintf_block.replace("va_start(vaArgP, format);", "va_copy(vaArgP, args);");

    content.insert_str(start, &format!("{vsnprintf_block}\n\n"));
    fs::write(&os_printf, content).map_err(|e| format!("Failed to write {}: {e}", os_printf.display()))?;
    Ok(())
}

pub fn ensure_common_sdk_c_fixes(sdk_path: &Path) {
    let _ = ensure_sdk_string_c_includes(sdk_path);
    let _ = ensure_sdk_sprintf_no_vsnprintf(sdk_path);
    let _ = ensure_sdk_vsnprintf(sdk_path);
}

pub fn ensure_nbgl_draw_text_override(sdk_path: &Path) -> Result<(), String> {
    let nbgl_draw = sdk_path.join("lib_nbgl/src/nbgl_draw.c");
    if !nbgl_draw.exists() {
        return Ok(());
    }

    let mut content =
        fs::read_to_string(&nbgl_draw).map_err(|e| format!("Failed to read {}: {e}", nbgl_draw.display()))?;

    if content.contains("// patched by KeyOS: nbgl_drawText removed") {
        return Ok(());
    }

    if let Some(start) = content.find("\nnbgl_font_id_e nbgl_drawText(") {
        let func_start = start + 1;
        if let Some(open_brace) = content[func_start..].find('{') {
            let mut depth = 0;
            let mut end = func_start + open_brace;
            for (i, ch) in content[func_start + open_brace..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = func_start + open_brace + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let replacement = "// patched by KeyOS: nbgl_drawText removed (provided by Rust override)\n";
            content.replace_range(func_start..end, replacement);
        }
    }
    fs::write(&nbgl_draw, content).map_err(|e| format!("Failed to write {}: {e}", nbgl_draw.display()))?;
    Ok(())
}

pub fn ensure_nbgl_font_data(sdk_path: &Path) -> Result<(), String> {
    let nbgl_fonts = sdk_path.join("lib_nbgl/src/nbgl_fonts.c");
    if !nbgl_fonts.exists() {
        return Ok(());
    }

    let mut content = fs::read_to_string(&nbgl_fonts)
        .map_err(|e| format!("Failed to read {}: {e}", nbgl_fonts.display()))?;

    content = content.replace(
        "#if defined(BOLOS_OS_UPGRADER_APP)",
        "#if 1 // patched by KeyOS build.rs (was BOLOS_OS_UPGRADER_APP)",
    );
    content = content.replace(
        "#if (defined(HAVE_BOLOS) && !defined(BOLOS_OS_UPGRADER_APP))",
        "#if 0 // patched by KeyOS build.rs (was HAVE_BOLOS && !BOLOS_OS_UPGRADER_APP)",
    );
    content =
        content.replace("__attribute__((section(\"._nbgl_fonts_\")))", "/* section removed for KeyOS */");

    fs::write(&nbgl_fonts, content).map_err(|e| format!("Failed to write {}: {e}", nbgl_fonts.display()))?;

    // The restored (pre-v26) nbgl_fonts.h falls back to `#define LANGUAGE_PACK
    // void` when HAVE_LANGUAGE_PACK is unset, which the app build leaves unset.
    // The v26 core `ux_loc.h` (kept) always typedefs LANGUAGE_PACK to a struct,
    // so that macro clobbers the typedef (`} void;`). Include ux_loc.h in the
    // fallback branch to pull in the real type instead.
    let nbgl_fonts_h = sdk_path.join("lib_nbgl/include/nbgl_fonts.h");
    if nbgl_fonts_h.exists() {
        let header = fs::read_to_string(&nbgl_fonts_h)
            .map_err(|e| format!("Failed to read {}: {e}", nbgl_fonts_h.display()))?;
        let patched = header.replace(
            "#ifndef HAVE_LANGUAGE_PACK\n#define LANGUAGE_PACK void\n#endif  // HAVE_LANGUAGE_PACK",
            "#ifndef HAVE_LANGUAGE_PACK\n#include \"ux_loc.h\"  // KeyOS: v26 core provides the LANGUAGE_PACK type\n#endif  // HAVE_LANGUAGE_PACK",
        );
        if patched != header {
            fs::write(&nbgl_fonts_h, patched)
                .map_err(|e| format!("Failed to write {}: {e}", nbgl_fonts_h.display()))?;
        }
    }
    Ok(())
}
