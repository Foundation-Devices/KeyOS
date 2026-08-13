// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use gui_server_api::{consts::SCREEN_WIDTH, InputMessage};
use xous::{DropDeallocate, MemoryAddress, MemoryRange, MemorySize, CID, PID};

use crate::{layers::LayerPixelFormat, Gui};

#[derive(Debug)]
pub struct BufferChain {
    cid: CID,
    height: u16,
    // Stored only for logging purposes
    pid: PID,
    state: BufferChainState,
    // The application is sending updates faster than we can render them.
    // Switches on if two updates happen within one VSync period,
    // Switches off if no updates happen within one VSync period.
    throttle_framerate: bool,
    buffer_in_flight: bool,
}

#[derive(Debug, Default)]
enum BufferChainState {
    /// Temporary state that's set during state change
    #[default]
    Invalid,
    /// App window is hidden, no buffers available.
    /// At most one in-flight buffer that will be discarded
    Hidden,
    /// App window was hidden, no buffers available to display yet.
    /// Should have exactly one in-flight buffer (except if allocation failed)
    ToBeShown,
    /// App window will be hidden, but framebuffer is still used by LCDC
    ToBeHidden { front: DropDeallocate },
    /// App window is shown.
    ShownFrontOnly { front: DropDeallocate },
    /// App window is shown.
    /// Should have no in-flight buffers
    Shown { front: DropDeallocate, previous: DropDeallocate },
    /// App window is shown, there was an update since vsync
    /// Should have no in-flight buffers
    ShownUpdatePending {
        front: DropDeallocate,
        next: DropDeallocate,
        /// App has requested a buffer since last vsync
        requested_buffer: bool,
    },
}

impl BufferChain {
    pub fn new(cid: CID, height: u16, pid: PID) -> Self {
        Self {
            cid,
            height,
            pid,
            throttle_framerate: false,
            buffer_in_flight: false,
            state: BufferChainState::Hidden,
        }
    }

    pub fn show(&mut self) {
        let state = self.state.take();
        self.state = match state {
            BufferChainState::Hidden {} => {
                if !self.buffer_in_flight {
                    self.send_fresh_framebuffer();
                }
                BufferChainState::ToBeShown
            }
            BufferChainState::ToBeHidden { front } => BufferChainState::ShownFrontOnly { front },
            BufferChainState::ToBeShown
            | BufferChainState::ShownFrontOnly { .. }
            | BufferChainState::Shown { .. }
            | BufferChainState::ShownUpdatePending { .. } => {
                log::warn!("BufferChain::show called twice for PID={} (state: {:?})", self.pid, state);
                state
            }
            BufferChainState::Invalid => unreachable!(),
        }
    }

    pub fn hide(&mut self) {
        let state = self.state.take();
        self.state = match state {
            BufferChainState::ToBeShown => BufferChainState::Hidden,

            BufferChainState::ShownFrontOnly { front }
            | BufferChainState::Shown { front, previous: _ }
            | BufferChainState::ShownUpdatePending { front, next: _, requested_buffer: _ } => {
                BufferChainState::ToBeHidden { front }
            }
            BufferChainState::Hidden { .. } | BufferChainState::ToBeHidden { .. } => {
                log::warn!("BufferChain::hide called twice for PID={} (state: {:?})", self.pid, state);
                state
            }
            BufferChainState::Invalid => unreachable!(),
        }
    }

    /// Whether `show` was called and `hide` was not, so the window keeps a framebuffer.
    pub fn is_shown(&self) -> bool {
        match &self.state {
            BufferChainState::Hidden | BufferChainState::ToBeHidden { .. } => false,
            BufferChainState::ToBeShown
            | BufferChainState::ShownFrontOnly { .. }
            | BufferChainState::Shown { .. }
            | BufferChainState::ShownUpdatePending { .. } => true,
            BufferChainState::Invalid => unreachable!(),
        }
    }

    pub fn most_recent_buffer(&self) -> Option<MemoryRange> {
        match &self.state {
            BufferChainState::Hidden { .. } => None,
            BufferChainState::ToBeShown => None,
            BufferChainState::ToBeHidden { front, .. }
            | BufferChainState::ShownFrontOnly { front, .. }
            | BufferChainState::Shown { front, .. } => Some(**front),
            BufferChainState::ShownUpdatePending { next, .. } => Some(**next),
            BufferChainState::Invalid => unreachable!(),
        }
    }

    pub fn required_framebuffer_len(&self) -> usize { required_framebuffer_len(self.height) }

    pub fn buffer_requested(&mut self, vsync_time: u64) {
        if self.buffer_in_flight {
            log::warn!("Buffer already in flight when it was requested for PID={}", self.pid);
            return;
        }
        let state = self.state.take();
        self.state = match state {
            BufferChainState::Hidden { .. } | BufferChainState::ToBeHidden { .. } => {
                log::trace!("Not giving buffer to hidden window for PID={}", self.pid);
                state
            }
            BufferChainState::ToBeShown => {
                self.send_fresh_framebuffer();
                BufferChainState::ToBeShown
            }
            BufferChainState::ShownFrontOnly { front } => {
                self.send_fresh_framebuffer();
                BufferChainState::ShownFrontOnly { front }
            }
            BufferChainState::Shown { front, previous } => {
                self.send_framebuffer(previous, false, 0);
                BufferChainState::ShownFrontOnly { front }
            }
            BufferChainState::ShownUpdatePending { front, next, requested_buffer: _ } => {
                if self.throttle_framerate {
                    BufferChainState::ShownUpdatePending { front, next, requested_buffer: true }
                } else {
                    self.send_framebuffer(front, false, vsync_time);
                    self.throttle_framerate = true;
                    BufferChainState::ShownFrontOnly { front: next }
                }
            }
            BufferChainState::Invalid => unreachable!(),
        }
    }

    pub fn buffer_received(&mut self, pid: PID, buffer: MemoryRange) -> bool {
        let buffer = DropDeallocate::new(buffer);
        let expected = self.required_framebuffer_len();
        let actual = buffer.len();
        if actual != expected {
            log::error!(
                "Rejecting SubmitFrame from PID={pid:?}: framebuffer length {actual} does not match expected length {expected}"
            );
            return false;
        }

        if !self.buffer_in_flight {
            log::warn!("No buffer was in flight when buffer_received was called for PID={}", self.pid);
            return false;
        }
        self.buffer_in_flight = false;
        let state = self.state.take();
        self.state = match state {
            BufferChainState::Hidden { .. } | BufferChainState::ToBeHidden { .. } => {
                log::trace!("Dropping update buffer for hidden window for PID={}", self.pid);
                state
            }
            BufferChainState::ToBeShown => BufferChainState::ShownFrontOnly { front: buffer },
            BufferChainState::ShownFrontOnly { front, .. } => {
                BufferChainState::ShownUpdatePending { front, next: buffer, requested_buffer: false }
            }
            // These two cases should never happen, but a panic!() is a bit harsh,
            // because this is recoverable.
            BufferChainState::Shown { front, .. } | BufferChainState::ShownUpdatePending { front, .. } => {
                log::warn!(
                    "Three framebuffers on a single window for PID={}. Dropping last update",
                    self.pid
                );
                BufferChainState::ShownUpdatePending { front, next: buffer, requested_buffer: false }
            }
            BufferChainState::Invalid => unreachable!(),
        };
        true
    }

    pub fn vsync(&mut self) {
        let state = self.state.take();
        self.state = match state {
            BufferChainState::ToBeHidden { front: _ } => BufferChainState::Hidden {},
            BufferChainState::ShownUpdatePending { front, next, requested_buffer } => {
                if requested_buffer {
                    self.send_framebuffer(front, false, 0);
                    BufferChainState::ShownFrontOnly { front: next }
                } else {
                    BufferChainState::Shown { front: next, previous: front }
                }
            }
            BufferChainState::Hidden { .. }
            | BufferChainState::ToBeShown
            | BufferChainState::ShownFrontOnly { .. }
            | BufferChainState::Shown { .. } => {
                self.throttle_framerate = false;
                state
            }
            BufferChainState::Invalid => unreachable!(),
        }
    }

    fn send_fresh_framebuffer(&mut self) {
        match xous::map_memory(
            None,
            None,
            self.required_framebuffer_len(),
            xous::MemoryFlags::W | xous::MemoryFlags::POPULATE | xous::MemoryFlags::PLAINTEXT,
        ) {
            Ok(buf) => {
                self.send_framebuffer(DropDeallocate::new(buf), true, 0);
            }
            Err(e) => {
                log::error!("Could not allocate framebuffer for PID={}: {e:?}", self.pid);
            }
        }
    }

    fn send_framebuffer(&mut self, framebuffer: DropDeallocate, is_new: bool, vsync_time: u64) {
        let buffer = framebuffer.leak();
        let msg = xous::Message::new_move(
            InputMessage::FrameBuffer as usize,
            buffer,
            // vsync_time is truncated here that will need to be handled properly on the receiving side.
            MemoryAddress::new(vsync_time as usize),
            MemorySize::new(is_new as _),
        );
        match xous::send_message(self.cid, msg) {
            Ok(_) => self.buffer_in_flight = true,
            Err(e) => {
                log::error!("Failed to send buffer to app PID={}: {e:?}", self.pid);
                xous::unmap_memory(buffer).ok();
            }
        }
    }
}

fn required_framebuffer_len(height: u16) -> usize {
    (SCREEN_WIDTH * usize::from(height) * LayerPixelFormat::Argb8888.bytes_per_pixel())
        .next_multiple_of(xous::PAGE_SIZE)
}

impl BufferChainState {
    pub fn take(&mut self) -> Self { core::mem::take(self) }
}

impl Gui {
    pub fn vsync_framebuffers(&mut self) {
        for w in self.windows.values_mut() {
            w.buffers.vsync()
        }
        if let Some(w) = &mut self.control_center_window {
            w.buffers.vsync()
        }
        if let Some(w) = &mut self.keyboard_window {
            w.buffers.vsync()
        }
    }
}

#[cfg(test)]
mod tests {
    use gui_server_api::consts::DEFAULT_KEYBOARD_HEIGHT;

    use super::*;

    fn map_test_buffer(len: usize) -> MemoryRange {
        xous::map_memory(None, None, len, xous::MemoryFlags::W).unwrap()
    }

    #[test]
    fn wrong_sized_submit_is_rejected_before_becoming_visible() {
        let height = u16::try_from(DEFAULT_KEYBOARD_HEIGHT).unwrap();
        let expected = required_framebuffer_len(height);
        let pid = PID::new(1).unwrap();
        let mut buffers = BufferChain::new(0, height, pid);
        buffers.state = BufferChainState::ToBeShown;
        buffers.buffer_in_flight = true;

        assert!(!buffers.buffer_received(pid, map_test_buffer(xous::PAGE_SIZE)));
        assert!(matches!(buffers.state, BufferChainState::ToBeShown));
        assert!(buffers.buffer_in_flight);

        assert!(buffers.buffer_received(pid, map_test_buffer(expected)));
        assert!(matches!(buffers.state, BufferChainState::ShownFrontOnly { .. }));
        assert!(!buffers.buffer_in_flight);
        assert_eq!(buffers.most_recent_buffer().unwrap().len(), expected);
    }
}
