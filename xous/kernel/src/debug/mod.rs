// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use xous::MemoryRange;

#[macro_use]
mod macros;

#[cfg(keyos)]
pub mod commands;
#[cfg(keyos)]
mod process_list;
#[cfg(keyos)]
pub mod serial;

#[derive(Clone)]
pub(crate) struct BufStr<T> {
    buf: T,
    pos: usize,
}

impl<T: AsRef<[u8]>> BufStr<T> {
    #[allow(dead_code)]
    pub fn as_slice(&self) -> &[u8] { &self.buf.as_ref()[..self.pos] }
}

impl<const N: usize> BufStr<[u8; N]> {
    pub const fn new() -> Self { BufStr { buf: [0; N], pos: 0 } }
}

impl<'a> From<&'a mut [u8]> for BufStr<&'a mut [u8]> {
    fn from(value: &'a mut [u8]) -> Self { Self { buf: value, pos: 0 } }
}
impl<'a> From<&'a mut MemoryRange> for BufStr<&'a mut [u8]> {
    fn from(value: &'a mut MemoryRange) -> Self { Self::from(value.as_slice_mut()) }
}

impl<T: AsMut<[u8]>> core::fmt::Write for BufStr<T> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let s = s.as_bytes();
        let len = s.len().min(self.buf.as_mut().len() - self.pos);
        if len > 0 {
            self.buf.as_mut()[self.pos..self.pos + len].copy_from_slice(&s[..len]);
            self.pos += len;
        }
        Ok(())
    }
}

impl<T: AsRef<[u8]>> core::fmt::Display for BufStr<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = core::str::from_utf8(&self.buf.as_ref()[..self.pos]).unwrap_or("");
        f.write_str(s)
    }
}
