// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::array;

use camera::{
    messages::*, Frame, SubscriptionError, CAMERA_BYTES_PER_PX, CAMERA_HEIGHT, CAMERA_MARGIN, CAMERA_WIDTH,
};
use image::ImageBuffer;
use nokhwa::{nokhwa_initialize, utils::RequestedFormat, Camera};
use server::{
    ArchiveHandler, BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, ScalarHandler,
    ScalarSubList, ServerContext,
};
use {
    log::debug,
    std::sync::{Arc, Mutex},
};

const PHYSICAL_CAMERA_WIDTH: usize = 640;

#[derive(server::Server)]
#[name = "os/camera"]
pub struct CameraServer(Arc<Mutex<State>>);

struct State {
    is_enabled: bool,
    is_visible: bool,
    subscribers: ScalarSubList<Frame>,
    /// Stored camera parameters (for API consistency with real hardware)
    camera_params: CameraParams,
}

impl Default for CameraServer {
    fn default() -> Self {
        debug!("Initializing camera");
        let state = State {
            is_enabled: true,
            is_visible: false,
            subscribers: Default::default(),
            camera_params: Default::default(),
        };
        Self(Arc::new(Mutex::new(state)))
    }
}

impl CameraServer {
    pub fn start(&mut self, _context: &mut ServerContext<CameraServer>) {
        debug!("Running camera app");

        nokhwa_initialize(|allowed| log::info!("Nokhwa initialized: allowed={allowed:?}"));
        let state = self.0.clone();
        std::thread::spawn(move || Self::camera_thread(state));
    }

    fn camera_thread(state: Arc<Mutex<State>>) -> ! {
        let mut camera = Camera::new(
            nokhwa::utils::CameraIndex::Index(0),
            RequestedFormat::new::<nokhwa::pixel_format::RgbAFormat>(
                nokhwa::utils::RequestedFormatType::HighestResolution(nokhwa::utils::Resolution {
                    width_x: PHYSICAL_CAMERA_WIDTH as _,
                    height_y: CAMERA_HEIGHT as _,
                }),
            ),
        )
        .unwrap();
        let mut started = false;
        let frames: [(Frame, &mut [u8]); 2] = array::from_fn(|_| Frame::allocate());

        let mut current_frame = 0;

        loop {
            let state_lock = state.lock().unwrap();
            if state_lock.is_enabled && state_lock.is_visible {
                drop(state_lock);
                if !started {
                    camera.open_stream().unwrap();
                    started = true;
                }
                let new_frame =
                    camera.frame().unwrap().decode_image::<nokhwa::pixel_format::RgbAFormat>().unwrap();
                const FROM: usize = CAMERA_MARGIN * CAMERA_WIDTH * CAMERA_BYTES_PER_PX;
                const TO: usize = (CAMERA_MARGIN + CAMERA_HEIGHT) * CAMERA_WIDTH * CAMERA_BYTES_PER_PX;
                let work_area = &mut frames[current_frame].1[FROM..TO];

                // Copy the result into a buffer
                let mut work_image =
                    ImageBuffer::from_raw(CAMERA_WIDTH as _, CAMERA_HEIGHT as _, work_area).unwrap();

                image::imageops::replace(&mut work_image, &new_frame, -100, 0);
                state.lock().unwrap().subscribers.send(&frames[current_frame].0);
                current_frame ^= 1;
            } else {
                drop(state_lock);
                if started {
                    camera.stop_stream().unwrap();
                    started = false;
                }

                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

impl server::ScalarEventHandler<settings::global::CameraEnabled> for CameraServer {
    fn handle(
        &mut self,
        msg: settings::global::CameraEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.0.lock().unwrap().is_enabled = msg.0;
    }
}

impl ScalarHandler<SetEnabled> for CameraServer {
    fn handle(&mut self, msg: SetEnabled, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.0.lock().unwrap().is_enabled = msg.0;
    }
}
impl ScalarHandler<NotifyVisible> for CameraServer {
    fn handle(&mut self, msg: NotifyVisible, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.0.lock().unwrap().is_visible = msg.0;
    }
}
impl BlockingScalarHandler<IsEnabled> for CameraServer {
    fn handle(
        &mut self,
        _msg: IsEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <IsEnabled as BlockingScalar>::Response {
        self.0.lock().unwrap().is_enabled
    }
}
impl BlockingScalarHandler<IsInUse> for CameraServer {
    fn handle(
        &mut self,
        _msg: IsInUse,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <IsInUse as BlockingScalar>::Response {
        let state = self.0.lock().unwrap();
        state.is_enabled && state.is_visible
    }
}

impl server::ScalarEventSubscriptionHandler<Subscribe> for CameraServer {
    fn handle(
        &mut self,
        _msg: Subscribe,
        subscriber: server::ScalarEventSubscriber<Frame>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), SubscriptionError> {
        self.0.lock().unwrap().subscribers.push(subscriber);
        Ok(())
    }
}
// Camera config handlers (stored for API consistency, but no real effect on simulator)
impl BlockingArchiveHandler<GetParams> for CameraServer {
    fn handle(
        &mut self,
        _msg: GetParams,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetParams as server::BlockingArchive>::Response {
        self.0.lock().unwrap().camera_params
    }
}

impl ArchiveHandler<SetParams> for CameraServer {
    fn handle(
        &mut self,
        msg: server::Owned<SetParams>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Ok(msg) = msg.deserialize() else { return };
        self.0.lock().unwrap().camera_params = msg.0;
    }
}
