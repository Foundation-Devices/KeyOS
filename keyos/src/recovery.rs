// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

/// Single-consumer marker written before Recovery's first irreversible firmware swap and consumed by the
/// update server on the next normal boot.
pub const UPDATE_STATE_INVALIDATED_MARKER_PATH: &str = "state/recovery/update-state-invalidated";
