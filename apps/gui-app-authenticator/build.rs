// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(&["proto/google_auth_migration.proto"], &["proto/"])?;
    slint_keyos_platform_build::compile_options(slint_keyos_platform_build::CompileOptions {
        module_path: "ui/app.slint",
        include_router: true,
        include_slint: true,
        include_translations: true,
        include_time_localization: false,
    });
    Ok(())
}
