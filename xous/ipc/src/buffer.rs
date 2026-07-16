use core::cell::RefCell;

use rkyv::{
    ser::{
        allocator::{Arena, ArenaHandle},
        writer::Buffer as RkyvBuffer,
        Positional, Serializer, Writer,
    },
    Deserialize, Portable,
};
use xous::{
    map_memory, send_message, try_send_message, unmap_memory, Error, MemoryAddress, MemoryFlags,
    MemoryMessage, MemoryRange, MemorySize, Message, Result, CID,
};

pub type XousDeserializer = rkyv::rancor::Strategy<(), rkyv::rancor::Error>;
pub type XousValidator<'a> = rkyv::api::low::LowValidator<'a, rkyv::rancor::Error>;
pub type XousSerializer<'a, 'b> = rkyv::rancor::Strategy<
    rkyv::ser::Serializer<rkyv::ser::writer::Buffer<'b>, ArenaHandle<'a>, ()>,
    rkyv::rancor::Error,
>;
pub type SizeOfSerializer<'a> =
    rkyv::rancor::Strategy<rkyv::ser::Serializer<SizeOfWriter, ArenaHandle<'a>, ()>, rkyv::rancor::Error>;

#[derive(Debug)]
pub struct Buffer<'buf> {
    pages: MemoryRange,
    used: usize,
    slice: &'buf mut [u8],
    should_drop: bool,
    memory_message: Option<&'buf mut MemoryMessage>,
}

impl<'buf> Buffer<'buf> {
    pub fn new(len: usize) -> Self {
        let len = core::cmp::max(len.next_multiple_of(0x1000), 0x1000);
        // Allocate enough memory to hold the requested data
        let new_mem = map_memory(None, None, len, MemoryFlags::W).expect("Buffer: error in new()/map_memory");
        Buffer {
            pages: new_mem,
            slice: unsafe { core::slice::from_raw_parts_mut(new_mem.as_mut_ptr(), len) },
            used: 0,
            should_drop: true,
            memory_message: None,
        }
    }

    /// use a volatile write to ensure a clear operation is not optimized out
    /// for ensuring that a buffer is cleared, e.g. at the exit of a function
    pub fn volatile_clear(&mut self) {
        let b = self.slice.as_mut_ptr();
        for i in 0..self.slice.len() {
            unsafe {
                b.add(i).write_volatile(core::mem::zeroed());
            }
        }
        // Ensure the compiler doesn't re-order the clear.
        // We use `SeqCst`, because `Acquire` only prevents later accesses from being reordered before
        // *reads*, but this method only *writes* to the locations.
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }

    /// use to serialize a buffer between process-local threads. mainly for spawning new threads with more
    /// complex argument structures.
    pub unsafe fn to_raw_parts(&self) -> (usize, usize, usize) {
        (self.pages.as_ptr() as usize, self.pages.len(), self.used)
    }

    /// use to serialize a buffer between process-local threads. mainly for spawning new threads with more
    /// complex argument structures.
    pub unsafe fn from_raw_parts(address: usize, len: usize, offset: usize) -> Self {
        let mem = MemoryRange::new(address, len).expect("invalid memory range args");
        Buffer {
            pages: mem,
            slice: core::slice::from_raw_parts_mut(mem.as_mut_ptr(), mem.len()),
            used: offset,
            should_drop: false,
            memory_message: None,
        }
    }

    /// Consume the buffer and return the underlying storage. Used for situations where we just want to
    /// serialize into a buffer and then do something manually with the serialized data.
    ///
    /// Fails if the buffer was converted from a memory message -- the Drop semantics
    /// of the memory message would cause problems with this conversion.
    pub fn into_inner(mut self) -> core::result::Result<(MemoryRange, usize), Error> {
        if self.memory_message.is_none() {
            self.should_drop = false;
            Ok((self.pages, self.used))
        } else {
            Err(Error::ShareViolation)
        }
    }

    /// Inverse of into_inner(). Used to re-cycle pages back into a Buffer so we don't have
    /// to re-allocate data. Only safe if the `pages` matches the criteria for mapped memory
    /// pages in Xous: page-aligned, with lengths that are a multiple of a whole page size.
    pub unsafe fn from_inner(pages: MemoryRange, used: usize) -> Self {
        Buffer {
            pages,
            slice: core::slice::from_raw_parts_mut(pages.as_mut_ptr(), pages.len()),
            used,
            should_drop: false,
            memory_message: None,
        }
    }

    #[inline]
    pub unsafe fn from_memory_message(mem: &'buf MemoryMessage) -> Self {
        let len = mem.buf.len();
        Buffer {
            pages: mem.buf,
            slice: core::slice::from_raw_parts_mut(mem.buf.as_mut_ptr(), len),
            used: mem.offset.map_or(0, |v| v.get()).min(len),
            should_drop: false,
            memory_message: None,
        }
    }

    #[inline]
    pub unsafe fn from_memory_message_mut(mem: &'buf mut MemoryMessage) -> Self {
        let len = mem.buf.len();
        Buffer {
            pages: mem.buf,
            slice: core::slice::from_raw_parts_mut(mem.buf.as_mut_ptr(), len),
            used: mem.offset.map_or(0, |v| v.get()).min(len),
            should_drop: false,
            memory_message: Some(mem),
        }
    }

    /// Perform a mutable lend of this Buffer to the server.
    pub fn lend_mut(&mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = MemoryMessage {
            id: id as usize,
            buf: self.pages,
            offset: MemoryAddress::new(self.used),
            valid: MemorySize::new(self.pages.len()),
        };

        // Update the offset pointer if the server modified it.
        let result = send_message(connection, Message::MutableBorrow(msg));
        if let Ok(Result::MemoryReturned(offset, _valid)) = result {
            self.used = offset.map_or(0, |v| v.get()).min(self.pages.len());
        }

        result
    }

    pub fn lend(&self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = MemoryMessage {
            id: id as usize,
            buf: self.pages,
            offset: MemoryAddress::new(self.used),
            valid: MemorySize::new(self.pages.len()),
        };
        send_message(connection, Message::Borrow(msg))
    }

    #[allow(dead_code)]
    pub fn send(mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = MemoryMessage {
            id: id as usize,
            buf: self.pages,
            offset: MemoryAddress::new(self.used),
            valid: MemorySize::new(self.pages.len()),
        };
        let result = send_message(connection, Message::Move(msg))?;

        // prevents it from being Dropped.
        self.should_drop = false;
        Ok(result)
    }

    pub fn send_nowait(mut self, connection: CID, id: u32) -> core::result::Result<Result, Error> {
        let msg = MemoryMessage {
            id: id as usize,
            buf: self.pages,
            offset: MemoryAddress::new(self.used),
            valid: MemorySize::new(self.pages.len()),
        };
        let result = try_send_message(connection, Message::Move(msg))?;

        // prevents it from being Dropped.
        self.should_drop = false;
        Ok(result)
    }

    /// Allocate a fresh page-aligned Buffer and copy `bytes` into it. The
    /// resulting buffer can be sent via [`Self::send`]/[`Self::lend`] just
    /// like one produced by [`Self::into_buf`]. Useful for forwarding an
    /// already-archived rkyv payload without re-serializing.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut buf = Self::new(bytes.len());
        buf.slice[..bytes.len()].copy_from_slice(bytes);
        buf.used = bytes.len();
        buf
    }

    pub fn into_buf<T>(src: &T) -> core::result::Result<Self, rkyv::rancor::Error>
    where
        T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>
            + for<'a> rkyv::Serialize<SizeOfSerializer<'a>>,
    {
        with_arena(|arena| {
            let mut size_serializer = Serializer::new(SizeOfWriter::new(), arena, ());
            rkyv::api::serialize_using(src, &mut size_serializer)?;
            let size = size_serializer.writer.pos();

            let (_, arena, _) = size_serializer.into_raw_parts();

            let mut xous_buf = Self::new(size);
            let writer = RkyvBuffer::from(&mut xous_buf.slice[..]);
            let mut serializer = Serializer::new(writer, arena, ());
            rkyv::api::serialize_using(src, &mut serializer)?;
            xous_buf.used = serializer.pos();

            Ok(xous_buf)
        })
    }

    pub fn replace<T>(&mut self, src: &T) -> core::result::Result<(), rkyv::rancor::Error>
    where
        T: for<'a, 'b> rkyv::Serialize<XousSerializer<'a, 'b>>,
    {
        with_arena(|arena| {
            let writer = RkyvBuffer::from(&mut self.slice[..]);
            let mut serializer = Serializer::new(writer, arena, ());
            rkyv::api::serialize_using(src, &mut serializer)?;
            self.used = serializer.pos();
            Ok(())
        })?;

        if let Some(ref mut msg) = self.memory_message.as_mut() {
            msg.offset = MemoryAddress::new(self.used);
        }
        Ok(())
    }

    /// Zero-copy representation of the data on the receiving side, wrapped in an "Archived" trait and left in
    /// the heap
    #[inline]
    pub fn access<T>(&'buf self) -> core::result::Result<&'buf T::Archived, rkyv::rancor::Error>
    where
        T: rkyv::Archive,
        T::Archived: Portable + for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>,
    {
        rkyv::api::low::access::<T::Archived, rkyv::rancor::Error>(&self.slice[..self.used])
    }

    /// A representation identical to the original, but requires copying to the stack. More expensive so uses
    /// "to_" prefix.
    #[inline]
    pub fn to_original<T>(&self) -> core::result::Result<T, rkyv::rancor::Error>
    where
        T: rkyv::Archive,
        T::Archived: Portable
            + for<'a> rkyv::bytecheck::CheckBytes<XousValidator<'a>>
            + Deserialize<T, XousDeserializer>,
    {
        let archived = rkyv::api::low::access::<T::Archived, rkyv::rancor::Error>(&self.slice[..self.used])?;
        let value = rkyv::api::low::deserialize(archived)?;
        Ok(value)
    }

    pub fn used(&self) -> usize { self.used }
}

thread_local! {
    static ALLOC_ARENA: RefCell<Arena> = RefCell::new(Arena::new());
}

fn with_arena<R>(f: impl FnOnce(ArenaHandle<'_>) -> R) -> R {
    ALLOC_ARENA.with_borrow_mut(|alloc| f(alloc.acquire()))
}

impl<'a> core::convert::AsRef<[u8]> for Buffer<'a> {
    fn as_ref(&self) -> &[u8] { self.slice }
}

impl<'a> core::convert::AsMut<[u8]> for Buffer<'a> {
    fn as_mut(&mut self) -> &mut [u8] { self.slice }
}

impl<'a> core::ops::Deref for Buffer<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target { &*self.slice }
}

impl<'a> core::ops::DerefMut for Buffer<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut *self.slice }
}

impl<'a> Drop for Buffer<'a> {
    fn drop(&mut self) {
        if self.should_drop {
            unmap_memory(self.pages).expect("Buffer: failed to drop memory");
        }
    }
}

/// A writer that only counts the size of the serialized data without actually writing it.
/// Used to determine buffer size requirements before allocating memory.
#[derive(Debug, Default)]
pub struct SizeOfWriter {
    pos: usize,
}

impl SizeOfWriter {
    pub fn new() -> Self { Self { pos: 0 } }
}

impl Positional for SizeOfWriter {
    fn pos(&self) -> usize { self.pos }
}

impl rkyv::rancor::Fallible for SizeOfWriter {
    type Error = rkyv::rancor::Error;
}

impl Writer<rkyv::rancor::Error> for SizeOfWriter {
    fn write(&mut self, bytes: &[u8]) -> core::result::Result<(), rkyv::rancor::Error> {
        self.pos += bytes.len();
        Ok(())
    }
}
