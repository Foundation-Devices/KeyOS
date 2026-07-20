// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // app-manager owns the permission subgroup labels, so it resolves them from its own i18n
    // catalog (registered in localizer.json). It has no Slint UI, so it uses the standalone
    // (Rust-only) translation emit.
    localizer_codegen::compile_service_translations();
}
