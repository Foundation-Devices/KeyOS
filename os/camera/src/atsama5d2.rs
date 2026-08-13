// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::array;
use std::thread;
use std::time::Duration;

use atsama5d27::isc::{ClkSel, DmaBuffer, DmaControlConfig, DmaView, ISCStatus, Isc};
use camera::{
    messages::*, Frame, SubscriptionError, CAMERA_BYTES_PER_PX, CAMERA_FB_SIZE_BYTES, CAMERA_HEIGHT,
    CAMERA_MARGIN, CAMERA_WIDTH,
};
use gpio::{GpioPin, PinSettings};
use i2c::Peripheral;
use ovm7690_rs::Ovm7690;
use server::ArchiveHandler;
use server::BlockingArchiveHandler;
use server::{BlockingScalar, BlockingScalarHandler, CheckedConn, MessageId, ScalarHandler, ServerContext};
use utralib::utra::isc::HW_ISC_BASE;
use xous::{arch::irq::IrqNumber, keyos::PAGE_SIZE, MemoryFlags, MemoryRange};

const ISC_MASTER_CLK_DIV: u8 = 13; // This gives around 30 fps
const ISC_MASTER_CLK_SEL: ClkSel = ClkSel::Hclock;
const ISC_ISP_CLK_DIV: u8 = 2;
const ISC_ISP_CLK_SEL: ClkSel = ClkSel::Hclock;

i2c::use_api!();
gpio::use_api!();
power_manager::use_api!();

#[derive(server::Server)]
#[name = "os/camera"]
pub struct CameraServer {
    bufs: TripleBuffer,
    is_enabled: bool,
    is_visible: bool,
    hw_state: HwState,
    isc: Isc,
    isc_address: u32,
    gpio: GpioApi,
    power_manager: PowerManagerApi,
    ovm: Ovm7690<I2cPeripheral>,
    subscribers: Vec<FrameSubscriber>,
    frame_in_dma: usize,
    camera_params: CameraParams,
}

type TripleBuffer = [MemoryRange; 3];

struct FrameSubscriber {
    subscriber: server::ScalarEventSubscriber<Frame>,
    buffers: TripleBuffer,
}

#[derive(Debug, server::Message)]
struct FrameCaptured;

#[derive(Debug, server::Message)]
struct SubscriberDisconnected(xous::CID);

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "os/camera"]
#[all_permissions]
struct InternalPermissions;

struct InterruptContext {
    conn: CheckedConn<InternalPermissions>,
    isc: Isc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HwState {
    Enabled,
    DisableAfterNextFrame,
    Disabled,
}

impl Default for CameraServer {
    fn default() -> Self {
        log::debug!("Initializing camera");

        let gpio = gpio::GpioApi::default();
        log::trace!("Claiming GPIO pins");
        gpio.claim_pin(GpioPin::CamPwdn, PinSettings::OutputLow, false).unwrap();
        gpio.claim_pin(GpioPin::CamLdoPwdnB, PinSettings::OutputHigh, false).unwrap();

        log::trace!("Enabling image sensor interface clock");
        let power_manager = PowerManagerApi::default();
        power_manager.enable_peripheral(atsama5d27::pmc::PeripheralId::Isi).unwrap();

        log::trace!("Mapping ISC");
        let mem = xous::map_memory(
            xous::MemoryAddress::new(HW_ISC_BASE),
            None,
            0x1000,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .unwrap();
        let isc_address = mem.as_ptr() as u32;
        log::trace!("Mapped ISC to 0x{:08x}", isc_address);
        let mut isc = Isc::with_alt_base_addr(isc_address);
        isc.setup_clocks(ISC_MASTER_CLK_DIV, ISC_MASTER_CLK_SEL, ISC_ISP_CLK_DIV, ISC_ISP_CLK_SEL);
        isc.enable_clock();
        isc.configure(true);
        isc.set_cropping_area(0, 0, CAMERA_WIDTH as u32, CAMERA_HEIGHT as u32);
        isc.enable_interrupt(ISCStatus::DDONE);
        log::trace!("ISC initialized");

        let bufs = array::from_fn(|_| {
            xous::map_memory(
                None,
                None,
                CAMERA_FB_SIZE_BYTES.next_multiple_of(PAGE_SIZE),
                xous::MemoryFlags::W | xous::MemoryFlags::POPULATE | xous::MemoryFlags::PLAINTEXT,
            )
            .unwrap()
        });

        let dma_desc_mem = xous::map_memory(
            None,
            None,
            0x1000,
            MemoryFlags::W
                | MemoryFlags::NO_CACHE
                | MemoryFlags::DEV
                | MemoryFlags::POPULATE
                | MemoryFlags::PLAINTEXT,
        )
        .unwrap();
        let dma_desc_addr = dma_desc_mem.as_ptr() as usize;
        let dma_desc_phys_addr = xous::virt_to_phys(dma_desc_addr).unwrap();

        let isc_dma: [DmaBuffer; 3] = array::from_fn(|i| {
            DmaBuffer::new(
                (dma_desc_addr + size_of::<DmaView>() * i) as u32,
                (dma_desc_phys_addr + size_of::<DmaView>() * i) as u32,
                xous::virt_to_phys(
                    bufs[i].as_ptr() as usize + CAMERA_MARGIN * CAMERA_WIDTH * CAMERA_BYTES_PER_PX,
                )
                .unwrap() as u32,
            )
        });

        isc.configure_dma(
            &isc_dma,
            &DmaControlConfig { descriptor_enable: true, ..Default::default() },
            || {
                xous::syscall::flush_cache(dma_desc_mem, xous::CacheOperation::Clean)
                    .expect("invalidate cache dma");
            },
        );

        log::trace!("Claiming I2C camera peripheral");
        let i2c = i2c::I2cApi::default().claim_peripheral(Peripheral::Camera).unwrap();

        let mut ovm = Ovm7690::new(i2c);
        log::trace!("Verifying camera connection");
        ovm.verify_chip_id().unwrap();

        let mut result = Self {
            bufs,
            is_enabled: false,
            is_visible: false,
            hw_state: HwState::Disabled,
            isc,
            isc_address,
            gpio,
            power_manager,
            ovm,
            subscribers: Default::default(),
            frame_in_dma: 0,
            camera_params: CameraParams::default(),
        };

        log::trace!("Init done, disabling power and clocks");
        result.disable_hw();

        log::info!("Camera initialized");

        result
    }
}

impl CameraServer {
    pub fn start(&mut self, context: &mut ServerContext<Self>) {
        let int_ctx = Box::into_raw(Box::new(InterruptContext {
            conn: CheckedConn::default(),
            isc: Isc::with_alt_base_addr(self.isc_address),
        }));
        xous::claim_interrupt(IrqNumber::Isi, handle_isc_irq, int_ctx as *mut usize).unwrap();
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .unwrap();
    }

    fn enable_hw(&mut self) {
        self.power_manager.enable_peripheral(atsama5d27::pmc::PeripheralId::Isi).unwrap();
        self.isc.enable_clock();
        self.gpio.set_pin(GpioPin::CamPwdn, false).unwrap();
        self.gpio.set_pin(GpioPin::CamLdoPwdnB, true).unwrap();
        thread::sleep(Duration::from_millis(1));
        self.ovm.sw_reset().unwrap();
        thread::sleep(Duration::from_millis(1));
        self.ovm.init().unwrap();
        thread::sleep(Duration::from_millis(100));
    }

    fn disable_hw(&mut self) {
        self.isc.disable_clock();
        PowerManagerApi::default().disable_peripheral(atsama5d27::pmc::PeripheralId::Isi).unwrap();
        self.gpio.set_pin(GpioPin::CamLdoPwdnB, false).unwrap();
        self.gpio.set_pin(GpioPin::CamPwdn, true).unwrap();
    }

    fn update_hw_state(&mut self) {
        log::debug!(
            "Update HW state called: enabled={:?} visible={:?} hw_state={:?}",
            self.is_enabled,
            self.is_visible,
            self.hw_state
        );
        if self.is_enabled && self.is_visible {
            if self.hw_state != HwState::Enabled {
                if self.hw_state == HwState::Disabled {
                    log::debug!("Turning ON");
                    self.enable_hw();
                }
                self.isc.start_capture();
                self.hw_state = HwState::Enabled;
            }
        } else if self.hw_state == HwState::Enabled {
            log::debug!("Turning OFF after next frame");
            self.hw_state = HwState::DisableAfterNextFrame;
            self.isc.stop_capture();
        }
    }

    fn apply_camera_params(&mut self) {
        let params = self.camera_params;
        if let Err(e) = self.ovm.set_auto_controls(params.auto_controls) {
            log::error!("Error applying auto controls: {e:?}");
        }
        if let Err(e) = self.ovm.set_agc_ceiling(params.agc_ceiling) {
            log::error!("Error applying AGC ceiling: {e:?}");
        }
        if let Err(e) = self.ovm.set_brightness(params.brightness) {
            log::error!("Error applying brightness: {e:?}");
        }
        if let Err(e) = self.ovm.set_contrast(params.contrast) {
            log::error!("Error applying contrast: {e:?}");
        }
        if let Err(e) = self.ovm.set_saturation(params.saturation) {
            log::error!("Error applying saturation: {e:?}");
        }
        if let Err(e) = self.ovm.set_auto_edge_denoise(params.auto_sharpness, params.auto_denoise) {
            log::error!("Error applying auto edge/denoise: {e:?}");
        }
        // Only apply manual values if auto mode is disabled
        if !params.auto_sharpness {
            if let Err(e) = self.ovm.set_sharpness(params.sharpness) {
                log::error!("Error applying sharpness: {e:?}");
            }
        }
        if !params.auto_denoise {
            if let Err(e) = self.ovm.set_denoise(params.denoise) {
                log::error!("Error applying denoise: {e:?}");
            }
        }
    }
}

impl ScalarHandler<FrameCaptured> for CameraServer {
    fn handle(&mut self, _msg: FrameCaptured, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        let newest_frame = self.frame_in_dma;
        self.frame_in_dma = (self.frame_in_dma + 1) % 3;
        match self.hw_state {
            HwState::Enabled => {
                // ISC only writes the centre; invalidating the margins would discard the
                // zeroes filled into them before they reach DRAM.
                xous::syscall::flush_cache(
                    Frame::new(self.bufs[newest_frame]).content(),
                    xous::CacheOperation::Invalidate,
                )
                .expect("invalidate cache");
                self.subscribers.retain(|s| s.subscriber.send(&Frame::new(s.buffers[newest_frame])).is_ok());
            }
            HwState::DisableAfterNextFrame => {
                log::debug!("Turning OFF (discarding last frame)");
                for buf in &mut self.bufs {
                    buf.as_slice_mut::<u32>().fill(0);
                }
                self.disable_hw();
                self.hw_state = HwState::Disabled;
            }
            HwState::Disabled => {}
        }
    }
}

impl server::ScalarEventSubscriptionHandler<Subscribe> for CameraServer {
    fn handle(
        &mut self,
        _msg: Subscribe,
        subscriber: server::ScalarEventSubscriber<Frame>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), SubscriptionError> {
        let mirrors = self
            .bufs
            .iter()
            .map(|b| xous::mirror_memory_to_pid(*b, subscriber.pid()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                log::error!("Error mirroring memory to pid {}: {e:?}", subscriber.pid());
                SubscriptionError::OutOfMemory
            })?;
        self.subscribers.push(FrameSubscriber { subscriber, buffers: mirrors.try_into().unwrap() });
        Ok(())
    }
}

impl server::ScalarHandler<SubscriberDisconnected> for CameraServer {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.subscribers.retain(|s| s.subscriber.cid() != msg.0);
    }
}

impl server::ScalarEventHandler<settings::global::CameraEnabled> for CameraServer {
    fn handle(
        &mut self,
        msg: settings::global::CameraEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.is_enabled = msg.0;
        self.update_hw_state();
    }
}

impl ScalarHandler<SetEnabled> for CameraServer {
    fn handle(&mut self, msg: SetEnabled, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.is_enabled = msg.0;
        self.update_hw_state();
    }
}
impl ScalarHandler<NotifyVisible> for CameraServer {
    fn handle(&mut self, msg: NotifyVisible, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.is_visible = msg.0;
        self.update_hw_state();
    }
}
impl BlockingScalarHandler<IsEnabled> for CameraServer {
    fn handle(
        &mut self,
        _msg: IsEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <IsEnabled as BlockingScalar>::Response {
        self.is_enabled
    }
}
impl BlockingScalarHandler<IsInUse> for CameraServer {
    fn handle(
        &mut self,
        _msg: IsInUse,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <IsInUse as BlockingScalar>::Response {
        self.hw_state != HwState::Disabled
    }
}
impl BlockingArchiveHandler<GetParams> for CameraServer {
    fn handle(
        &mut self,
        _msg: GetParams,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetParams as server::BlockingArchive>::Response {
        // Return stored params (not reading from HW as it may be disabled)
        self.camera_params
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
        self.camera_params = msg.0;
        if self.hw_state == HwState::Enabled {
            self.apply_camera_params();
        }
    }
}

/// Handles IRQs from ISC.
fn handle_isc_irq(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &mut *(arg as *mut InterruptContext) };
    let status = context.isc.interrupt_status();

    // DMA transfer of the camera frame is complete
    if status.contains(ISCStatus::DDONE) {
        context.conn.send_scalar_nowait(FrameCaptured).ok();
    }
}
