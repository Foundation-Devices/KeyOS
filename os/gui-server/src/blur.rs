// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::rc::Rc;

use gui_server_api::consts::{DEFAULT_KEYBOARD_HEIGHT, SCREEN_HEIGHT, SCREEN_WIDTH};
use xous::{DropDeallocate, MemoryFlags, MemoryRange, Message, ScalarMessage, CID, PID, SID};

use crate::{handlers::BlurDone, Gui};

#[derive(Debug, Default)]
pub(crate) struct BlurThread {
    pub cid: Option<CID>,
}

#[derive(Debug, Default)]
pub(crate) struct BlurBufferState {
    next_buffer: Option<Rc<DropDeallocate>>,
    displaying_buffer: Option<Rc<DropDeallocate>>,
    should_blur: bool,
    buffer_stale: bool,
    ongoing: bool,
    ongoing_stale: bool,
}

#[derive(Debug)]
struct BlurRequest {
    pid: PID,
    buffer: MemoryRange,
    height: usize,
}

impl Gui {
    pub fn blur_vsync(&mut self) {
        let Some(blur_cid) = self.blur_thread.cid else {
            log::warn!("Blur thread is not yet initialized");
            return;
        };
        let active_pid = self.active_app_pid();
        let modal_bg_pid = self.blurred_modal_background_pid();
        let control_center_blur = self.is_control_center_blur_active();
        for (pid, window) in &mut self.windows {
            if let Some(buffer) = window.buffers.most_recent_buffer() {
                window.blur_state.should_blur =
                    modal_bg_pid == Some(*pid) || control_center_blur && active_pid == Some(*pid);
                window.blur_state.on_vsync(blur_cid, *pid, buffer, SCREEN_HEIGHT);
            }
        }
        if let Some(keyboard) = &mut self.keyboard_window
            && let Some(buffer) = keyboard.buffers.most_recent_buffer()
        {
            keyboard.blur_state.should_blur = control_center_blur;
            keyboard.blur_state.on_vsync(blur_cid, keyboard.pid, buffer, DEFAULT_KEYBOARD_HEIGHT);
        }
    }

    pub(crate) fn handle_blur_done(&mut self, blur_done: BlurDone) {
        let buffer = DropDeallocate::new(blur_done.buffer);
        if let Some(keyboard) = &mut self.keyboard_window {
            if keyboard.pid == blur_done.pid {
                log::trace!("Blur done on a keyboard, state={:?}", keyboard.blur_state);
                keyboard.blur_state.handle_blur_done(buffer);
                self.update_layers();
                return;
            }
        }
        if let Some(window) = self.windows.get_mut(&blur_done.pid) {
            log::trace!("Blur done on a window, pid={:?}, state={:?}", blur_done.pid, window.blur_state);
            window.blur_state.handle_blur_done(buffer);
            self.update_layers();
            return;
        };
        log::warn!("Blur done on a window that does not exist (pid={:?})", blur_done.pid);
    }
}

impl BlurBufferState {
    pub fn handle_blur_done(&mut self, buffer: DropDeallocate) {
        self.ongoing = false;
        if !self.should_blur {
            log::debug!("Blur done but no longer needed");
            return;
        }
        self.buffer_stale = self.ongoing_stale;
        self.next_buffer = Some(Rc::new(buffer));
    }

    pub fn on_vsync(&mut self, cid: CID, pid: PID, display_buffer: MemoryRange, height: usize) {
        self.displaying_buffer = self.next_buffer.clone();

        if !self.should_blur {
            if self.next_buffer.is_some() {
                log::trace!("Unblurring window, state={self:?}");
            }
            self.next_buffer = None;
            return;
        }

        if self.ongoing || !self.buffer_stale && self.next_buffer.is_some() {
            return;
        }

        let fb_size = SCREEN_WIDTH * height * 4;
        let buffer = match xous::map_memory(
            None,
            None,
            fb_size.next_multiple_of(0x1000),
            MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::PLAINTEXT,
        ) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("Could not allocate blur buffer: {e:?}");
                return;
            }
        };
        buffer
            .subrange(0, fb_size)
            .unwrap()
            .as_slice_mut::<u32>()
            .copy_from_slice(display_buffer.subrange(0, fb_size).unwrap().as_slice());

        self.ongoing = true;
        self.ongoing_stale = false;
        log::trace!("Requesting blur on window (pid={pid}), state={self:?}");
        xous::send_message(cid, Message::Scalar(BlurRequest { pid, buffer, height }.into())).unwrap();
    }

    pub fn blurred_buf(&self) -> Option<MemoryRange> { self.next_buffer.as_ref().map(|b| ***b) }

    pub fn mark_stale(&mut self) {
        self.buffer_stale = true;
        self.ongoing_stale = true;
    }
}

impl BlurThread {
    pub fn start(&mut self, gui_server_sid: SID) {
        let blur_sid = xous::create_server().unwrap();
        let gui_server_cid = xous::connect(gui_server_sid).unwrap();
        std::thread::spawn(move || {
            xous::set_thread_priority(xous::ThreadPriority::AppBackground0).unwrap();
            loop {
                let envelope = xous::receive_message(blur_sid).unwrap();
                let Message::Scalar(msg) = envelope.body else {
                    log::warn!("Unknown message received: {envelope:?}");
                    continue;
                };
                let mut request = BlurRequest::from(msg);
                let height = request.buffer.len() / 4 / SCREEN_WIDTH;
                libblur::stack_blur(
                    request.buffer.as_slice_mut(),
                    (SCREEN_WIDTH * 4) as _,
                    SCREEN_WIDTH as _,
                    height as _,
                    16,
                    libblur::FastBlurChannels::Channels4,
                    libblur::ThreadingPolicy::Single,
                );
                #[cfg(keyos)]
                xous::flush_cache(request.buffer, xous::CacheOperation::Clean).ok();
                server::try_send_scalar(
                    gui_server_cid,
                    BlurDone { pid: request.pid, buffer: request.buffer },
                )
                .unwrap();
            }
        });
        self.cid = Some(xous::connect(blur_sid).unwrap());
    }
}

impl From<ScalarMessage> for BlurRequest {
    fn from(value: ScalarMessage) -> Self {
        Self {
            pid: PID::new(value.arg1 as _).unwrap(),
            buffer: unsafe { MemoryRange::new(value.arg2, value.arg3).unwrap() },
            height: value.arg4,
        }
    }
}

impl From<BlurRequest> for ScalarMessage {
    fn from(value: BlurRequest) -> Self {
        Self {
            id: 0,
            arg1: value.pid.get() as usize,
            arg2: value.buffer.as_ptr() as usize,
            arg3: value.buffer.len(),
            arg4: value.height,
        }
    }
}
