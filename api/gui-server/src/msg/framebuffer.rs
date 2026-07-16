// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::SimpleMemoryMessage;
use xous::MemoryRange;

#[derive(Debug, server::Message)]
pub struct SubmitFrame {
    pub buffer: MemoryRange,
}

impl From<SimpleMemoryMessage> for SubmitFrame {
    fn from(value: SimpleMemoryMessage) -> Self { SubmitFrame { buffer: value.buf } }
}

impl From<SubmitFrame> for SimpleMemoryMessage {
    fn from(value: SubmitFrame) -> Self { SimpleMemoryMessage { buf: value.buffer, arg1: 0, arg2: 0 } }
}
