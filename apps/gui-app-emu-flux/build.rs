// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    slint_keyos_platform_build::compile_options(slint_keyos_platform_build::CompileOptions {
        module_path: "ui/app.slint",
        include_router: true,
        include_slint: true,
        include_translations: true,
        include_time_localization: false,
    });
}
