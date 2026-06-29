use super::{Message, MessageId, MessageSender};

#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct Envelope {
    pub sender: MessageSender,
    pub body: Message,
}

impl Envelope {
    pub fn to_usize(&self) -> [usize; 7] {
        let body_usize = self.body.to_usize();
        [
            self.sender.to_usize(),
            body_usize[0],
            body_usize[1],
            body_usize[2],
            body_usize[3],
            body_usize[4],
            body_usize[5],
        ]
    }

    /// Take the message, throwing away the sender information. If the message is
    /// Blocking (i.e. if message.is_blocking() is `true`), then the process that
    /// sent this message will not automatically get a response. You will need to
    /// manually send a response.
    ///
    /// If the message is non-blocking, then this has no side effects.
    ///
    /// This is equivalent to "leaking" the message, which is not strictly unsafe.
    pub fn take_message(self) -> Message {
        use core::mem::ManuallyDrop;
        // Convert `Self` into something that won't have its "Drop" method called
        let manual_self = ManuallyDrop::new(self);

        // This function only works on memory messages
        unsafe { core::ptr::read(&manual_self.body) }
    }

    pub fn id(&self) -> MessageId { self.body.id() }
}

#[cfg(not(feature = "forget-memory-messages"))]
/// When a MessageEnvelope goes out of scope, return the memory.  It must either
/// go to the kernel (in the case of a Move), or back to the borrowed process
/// (in the case of a Borrow).  Ignore Scalar messages.
impl Drop for Envelope {
    fn drop(&mut self) {
        match &self.body {
            Message::Borrow(x) | Message::MutableBorrow(x) => {
                crate::syscall::return_memory_offset_valid(self.sender, x.buf, x.offset, x.valid).ok(); // avoid panicking if the process is gone before returning memory
            }
            // Move whatever buffer is currently set back to the sender: a reply that
            // replaced it, or else the moved-in buffer echoed back.
            Message::BlockingMove(x) => {
                crate::syscall::return_memory_offset_valid(self.sender, x.buf, x.offset, x.valid).ok();
            }
            Message::Move(msg) => {
                crate::syscall::unmap_memory(msg.buf).expect("couldn't free memory message")
            }
            _ => (),
        }
    }
}
