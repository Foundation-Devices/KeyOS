// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

// Internal message used to trigger general housekeeping (closing done transactions,
// pushing device configuration forward)
#[derive(Debug, server::Message, Clone)]
pub struct DoWork;

#[derive(Debug, server::Message)]
pub struct SubscriberDisconnected(pub xous::CID);

#[derive(Debug, server::Message, Clone)]
pub struct HostOtgMode(pub bool);
