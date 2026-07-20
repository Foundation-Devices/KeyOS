// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // gui-server has no Slint UI, so it uses the standalone (Rust-only) translation emit to
    // resolve the permission prompt strings from its i18n catalog (see localizer.json).
    localizer_codegen::compile_service_translations();
}
