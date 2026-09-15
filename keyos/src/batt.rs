// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

/// BQ24157 charger configuration shared between bootloader and `power-manager`.
pub const CHARGER_CONFIG_DUMP: [(u8, u8); 6] = [
    (6, 0x7c), // Safety limits, written first because they clamp the rest: 4.44 V, 85.0 mV sense.
    (0, 0x50), // Enable the STAT output. Bits 5:0 are read-only status.
    (1, 0x70), // 500 mA input limit, 3.7 V weak battery threshold, charge termination off.
    (2, 0xa2), // Charge to 4.30 V. OTG pin disabled, active high.
    (4, 0x00), // Lowest charge and termination current, 37.4 mV and 3.4 mV across the sense resistor.
    (5, 0x02), // Input DPM threshold 4.36 V, normal charge current sense.
];
