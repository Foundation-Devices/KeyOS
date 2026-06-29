// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use rkyv::{Deserialize, Portable};
use xous::{
    map_memory, send_message, try_send_message, unmap_memory, Error, MemoryAddress, MemoryFlags,
    MemoryMessage, MemoryRange, MemorySize, Message, Result, CID,
};

use crate::rkyv_utils::{
    decode, serialize_into, serialized_size, SizeOfSerializer, XousDeserializer, XousSerializer,
    XousValidator,
};

#[derive(Debug)]
pub struct Buffer {
    pages: MemoryRange,
    used: usize,
    should_drop: bool,
}

impl Buffer {
    pub(crate) fn new(len: usize) -> core::result::Result<Self, Error> {
        let len = core::cmp::max(len.next_multiple_of(0x1000), 0x1000);
        let pages = map_memory(None, None, len, MemoryFlags::W)?;
        Ok(Buffer { pages, used: 0, should_drop: true })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `pages` is a mapped, page-aligned range this Buffer owns, so its
        // pointer and length describe a valid [u8].
        unsafe { core::slice::from_raw_parts(self.pages.as_ptr(), self.pages.len()) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and `&mut self` rules out any other alias to the range.
        unsafe { core::slice::from_raw_parts_mut(self.pages.as_mut_ptr(), self.pages.len()) }
    }

    /// The payload length travels in `valid`; `offset` is unused. Confining the
    /// convention to these helpers keeps callers from touching the fields by hand.
    fn len_fields(len: usize) -> (Option<MemoryAddress>, Option<MemorySize>) { (None, MemorySize::new(len)) }

    /// Read a payload length back from a message that carries one.
    pub(crate) fn message_len(mem: &MemoryMessage) -> usize {
        mem.valid.map_or(0, |v| v.get()).min(mem.buf.len())
    }

    /// Build the message that lends or moves this buffer.
    fn message(&self, id: u32) -> MemoryMessage {
        let (offset, valid) = Self::len_fields(self.used);
        MemoryMessage { id: id as usize, buf: self.pages, offset, valid }
    }

    /// Consume the buffer and return the underlying storage.
    ///
    /// Fails for a message-backed buffer, whose pages belong to the message rather than us.
    fn into_inner(mut self) -> core::result::Result<(MemoryRange, usize), Error> {
        if self.should_drop {
            self.should_drop = false;
            Ok((self.pages, self.used))
        } else {
            Err(Error::ShareViolation)
        }
    }

    /// Deserialize the payload of an incoming message into an owned value.
    pub(crate) fn deserialize<T>(mem: &MemoryMessage) -> core::result::Result<T, rkyv::rancor::Error>
    where
        T: rkyv::Archive,
        T::Archived: Portable
            + for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>
            + Deserialize<T, XousDeserializer>,
    {
        decode(&mem.buf.as_slice::<u8>()[..Self::message_len(mem)])
    }

    /// Serialize `src` into the buffer the client lent in `mem`, recording the reply length
    /// so the client reads it.
    pub(crate) fn reply<T>(mem: &mut MemoryMessage, src: &T) -> core::result::Result<(), rkyv::rancor::Error>
    where
        T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>,
    {
        // SAFETY: `mem.buf` is the lent, page-aligned range the client gave us.
        let dst = unsafe { core::slice::from_raw_parts_mut(mem.buf.as_mut_ptr(), mem.buf.len()) };
        let used = serialize_into(dst, src)?;
        (mem.offset, mem.valid) = Self::len_fields(used);
        Ok(())
    }

    /// Serialize `src` as the reply to the `BlockingMove` carried by `mem`: swap `mem`'s buffer
    /// to the reply and free whatever of the moved-in buffer the reply does not cover.
    ///
    /// The reply reuses the front of the moved-in buffer when it is large enough, else a fresh
    /// allocation; unlike `reply` it need not fit the buffer the client sent.
    pub(crate) fn reply_move<T>(mem: &mut MemoryMessage, src: &T) -> core::result::Result<(), crate::Error>
    where
        T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>
            + for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
    {
        let existing = mem.buf;
        let size = serialized_size(src)?;
        let needed = core::cmp::max(size.next_multiple_of(0x1000), 0x1000);

        // Reuse the front of the moved-in buffer only on hardware; on hosted it is a
        // single host allocation that can't be freed in pieces, so always allocate fresh.
        let (reply, used) = if cfg!(keyos) && existing.len() >= needed {
            let reply = existing.subrange(0, needed).expect("page-aligned subrange of a mapped buffer");
            // SAFETY: `reply` is a mapped, page-aligned range we own for the duration of the reply.
            let dst = unsafe { core::slice::from_raw_parts_mut(reply.as_mut_ptr(), reply.len()) };
            (reply, serialize_into(dst, src)?)
        } else {
            // Keep the Buffer alive across serialize, or else a failure here leaks the mapping.
            let mut buf = Self::new(size)?;
            let used = serialize_into(buf.bytes_mut(), src)?;
            (buf.into_inner().expect("freshly mapped buffer is ownable").0, used)
        };
        mem.buf = reply;
        (mem.offset, mem.valid) = Self::len_fields(used);

        // Free whatever of the moved-in buffer the reply doesn't cover: the tail when we
        // reused the front, or all of it when we allocated a fresh one.
        if reply.as_ptr() == existing.as_ptr() {
            let tail_len = existing.len() - reply.len();
            if tail_len > 0 {
                let tail =
                    existing.subrange(reply.len(), tail_len).expect("tail subrange of a mapped buffer");
                unmap_memory(tail).expect("unmap reply tail");
            }
        } else {
            unmap_memory(existing).expect("unmap moved-in buffer");
        }
        Ok(())
    }

    /// Perform a mutable lend of this Buffer to the server.
    pub(crate) fn lend_mut(&mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = self.message(id);
        let result = send_message(connection, Message::MutableBorrow(msg));
        if let Ok(Result::MemoryReturned(_range, _offset, valid)) = result {
            self.used = valid.map_or(0, |v| v.get()).min(self.pages.len());
        }

        result
    }

    /// Send the buffer as a `BlockingMove`: ownership moves to the server and the call
    /// blocks until the server moves a (possibly resized) buffer back, which this buffer
    /// adopts. Unlike `lend_mut`, the reply need not match the sent buffer: it may outgrow
    /// it, or come back trimmed when a huge request yields a tiny response.
    pub(crate) fn blocking_move(&mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = self.message(id);
        // The kernel keeps this range reserved (lent) while we block, and on a server
        // death leaves it as on-demand rather than freeing it. So on any error the range
        // is still ours and mapped here, and `pages`/`should_drop` stay valid.
        let result = send_message(connection, Message::BlockingMove(msg))?;
        if let Result::MemoryReturned(range, _offset, valid) = result {
            self.pages = range;
            self.used = valid.map_or(0, |v| v.get()).min(range.len());
            self.should_drop = true;
        }
        Ok(result)
    }

    pub fn send(mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = self.message(id);
        let result = send_message(connection, Message::Move(msg))?;

        // Move transfers our pages to the server; don't unmap on drop.
        self.should_drop = false;
        Ok(result)
    }

    pub(crate) fn send_nowait(mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = self.message(id);
        let result = try_send_message(connection, Message::Move(msg))?;

        // Move transfers our pages to the server; don't unmap on drop.
        self.should_drop = false;
        Ok(result)
    }

    /// Allocate a fresh page-aligned Buffer and copy `bytes` into it. Useful for forwarding an
    /// already-archived rkyv payload without re-serializing.
    pub fn from_bytes(bytes: &[u8]) -> core::result::Result<Self, Error> {
        let mut buf = Self::new(bytes.len())?;
        buf.bytes_mut()[..bytes.len()].copy_from_slice(bytes);
        buf.used = bytes.len();
        Ok(buf)
    }

    pub(crate) fn into_buf<T>(src: &T) -> core::result::Result<Self, crate::Error>
    where
        T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>
            + for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
    {
        let mut buf = Self::new(serialized_size(src)?)?;
        buf.used = serialize_into(buf.bytes_mut(), src)?;
        Ok(buf)
    }

    /// Deserialize the buffer's valid bytes into an owned value.
    #[inline]
    pub(crate) fn to_original<T>(&self) -> core::result::Result<T, rkyv::rancor::Error>
    where
        T: rkyv::Archive,
        T::Archived: Portable
            + for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>
            + Deserialize<T, XousDeserializer>,
    {
        decode(&self.bytes()[..self.used])
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.should_drop {
            unmap_memory(self.pages).expect("Buffer: failed to drop memory");
        }
    }
}
