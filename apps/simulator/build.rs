// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // window.slint imports its components via relative paths, so no library
    // aliases are needed.
    slint_build::compile("ui/window.slint").unwrap();
}
