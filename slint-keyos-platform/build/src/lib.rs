// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;
use std::path::{Path, PathBuf};

use slint_keyos_platform_common::utils;

pub mod raw_image;
pub(crate) mod router;

use anyhow::Result;
// The translation codegen and its `Source`/`GeneratedFile` helpers live in the standalone
// `localizer-codegen` crate; re-export them under the module paths the rest of this crate uses.
pub(crate) use localizer_codegen::{generated_file, source};
pub use raw_image::{convert_icons, convert_image_to_raw};
pub use slint_keyos_platform_common::{UI2_ICON_SET, UI_ICON_SET};
use source::{uwrite, uwriteln, Source};

pub fn compile(module_path: &str) {
    compile_options(CompileOptions {
        module_path,
        include_slint: false,
        include_router: false,
        include_translations: false,
        include_time_localization: false,
    });
}

pub fn compile_app(module_path: &str) {
    compile_options(CompileOptions {
        module_path,
        include_slint: true,
        include_router: false,
        include_translations: false,
        include_time_localization: false,
    });
}

pub struct CompileOptions<'a> {
    pub module_path: &'a str,
    pub include_slint: bool,
    pub include_router: bool,
    pub include_translations: bool,
    pub include_time_localization: bool,
}

pub fn compile_options(options: CompileOptions) {
    let CompileOptions {
        module_path,
        include_slint,
        include_router,
        include_translations,
        include_time_localization,
    } = options;

    configure_miette();
    ensure_file_exists(module_path);

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manifest_path = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let module_path = Path::new(module_path);
    let root_path = module_path.parent().unwrap_or(module_path);
    let gen_dir = manifest_path.join(root_path).join("gen");

    localize(&out_dir, &manifest_path, &gen_dir, include_translations, include_time_localization);

    // Generate exports.slint if either router or translations are enabled
    if include_router || include_translations {
        generate_exports_slint(&gen_dir, include_router, include_translations).unwrap();
    }

    {
        let config = router::BuildRouterConfig { root_slint: root_path, out_dir: &out_dir, include_router };
        router::build_router(config);
    }

    let module_path = PathBuf::from(module_path);
    let input_slint_file_path = manifest_path.join(module_path);
    let output_rust_file_path = &out_dir.join(
        input_slint_file_path
            .file_stem()
            .map(Path::new)
            .unwrap_or_else(|| Path::new("slint_out"))
            .with_extension("rs"),
    );

    compile_slint(&input_slint_file_path, &output_rust_file_path).unwrap();

    // Patch the generated code to insert slint_keyos_platform's slint import.
    // This allows the user app to avoid having a dependency on the slint crate directly.
    // Without it, the compiler will complain on the include_modules!() macro
    // not being able to access slint crate from the outer scope.
    if include_slint {
        let code = std::fs::read_to_string(&output_rust_file_path).unwrap();
        let replace = concat!(
            "#[allow(unused)]\n",
            "use slint_keyos_platform::{route, route::*, slint};\n",
            "use slint ::"
        );
        let code = code.replacen("use slint ::", replace, 1);
        std::fs::write(&output_rust_file_path, code).unwrap();
    }
}

fn localize(
    out_dir: &Path,
    manifest_dir: &Path,
    gen_dir: &Path,
    include_translations: bool,
    include_time_localization: bool,
) {
    let translations_dir = manifest_dir.join("i18n");

    if include_translations && translations_dir.exists() {
        let options = localizer_codegen::TranslationOptions { slint: true, include_time_localization };
        localizer_codegen::build_translations(&translations_dir, out_dir, gen_dir, &options).unwrap();
    } else {
        localizer_codegen::generate_empty_translations(out_dir).unwrap();
    }
}

/// port of [`slint_build::compile_with_config`]
fn compile_slint(
    input_slint_file: impl AsRef<Path>,
    output_rust_file: impl AsRef<Path>,
) -> Result<(), slint_build::CompileError> {
    let input_slint_file = input_slint_file.as_ref();
    let output_rust_file = output_rust_file.as_ref();

    println!("cargo:rustc-env=SLINT_INCLUDE_GENERATED={}", output_rust_file.display());

    let config = slint_build::CompilerConfiguration::default()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRendererNoFonts)
        .with_library_paths(utils::library_paths())
        .with_style("fluent".into());

    let dependencies = slint_build::compile_with_output_path(input_slint_file, output_rust_file, config)?;

    for dependency in dependencies {
        println!("cargo:rerun-if-changed={}", dependency.display());
    }

    Ok(())
}

fn configure_miette() {
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .graphical_theme(miette::GraphicalTheme::unicode())
                .break_words(false)
                .width(100)
                .word_splitter(textwrap::WordSplitter::NoHyphenation)
                .word_separator(textwrap::WordSeparator::AsciiSpace)
                .rgb_colors(miette::RgbColors::Preferred)
                .build(),
        )
    }));
}

fn ensure_file_exists(path: impl AsRef<Path>) {
    let path = path.as_ref();
    if !path.exists() {
        let file_name = path.display().to_string();
        let report = miette::miette!(
            severity = miette::Severity::Error,
            labels = vec![miette::LabeledSpan::new(Some("File Name".into()), 0, file_name.len())],
            "File not found",
        )
        .with_source_code(file_name);

        eprintln!("{report:?}");
        std::process::exit(1);
    }
}

fn generate_exports_slint(gen_dir: &Path, include_router: bool, include_translations: bool) -> Result<()> {
    let mut src = Source::default();

    if include_translations {
        uwriteln!(src, "import {{ TrId, TR2 }} from \"tr.slint\";");
    }

    if include_router {
        uwriteln!(src, "import {{ Navigate, RoutePath }} from \"navigate.slint\";");
        uwriteln!(src, "import {{ RouteOption, RouteState }} from \"internal.slint\";");
    }

    uwriteln!(src, "");
    uwriteln!(src, "// IMPORTANT: you must export this entire module");
    uwriteln!(src, "//");
    uwriteln!(src, "// Add the following line to the top-level file (e.g. `app.slint`):");
    uwriteln!(src, "//");
    uwriteln!(src, "//```");
    uwriteln!(src, "// export * from \"gen/exports.slint\";");
    uwriteln!(src, "//```");
    uwriteln!(src, "");

    uwrite!(src, "export {{");
    if include_translations {
        uwrite!(src, "TrId, TR2,");
    }
    if include_router {
        uwrite!(src, "Navigate, RoutePath, RouteOption, RouteState");
    }
    uwriteln!(src, "}}");
    uwriteln!(src, "");

    generated_file::GeneratedFile { path: PathBuf::from("exports.slint"), content: src.into() }
        .write(gen_dir)?;

    Ok(())
}
