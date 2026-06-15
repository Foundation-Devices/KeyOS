// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::Write,
    sync::atomic::{AtomicU8, Ordering},
};

use fs::Location;
use mass_storage::{BlockDeviceCommands, MassStorageEmulation, UsbEmulationCommands};
use server::{BlockingArchiveHandler, MessageId as _, Server, ServerMessages};
use settings::global::AirlockMode;
use usb::device::{
    api::{EndpointDirection, EndpointType},
    messages::{EndpointProperties, SetupPacketCallback},
};
use xous::{MemoryFlags, MemoryRange};

fs::use_api!();
settings::use_api!();
usb::use_device_api!();

const INTERFACE_CLASS: u8 = 0x08; // Mass storage
const INTERFACE_SUBCLASS: u8 = 0x06; // SCSI passthrough
const INTERFACE_PROTOCOL: u8 = 0x50; // Bulk-only
const INTERFACE_NUMBER: u8 = usb::device::interface_numbers::MASS_STORAGE;

#[cfg(feature = "production")]
const EXPOSED_LOCATIONS: [Location; 1] = [Location::Airlock];
#[cfg(not(feature = "production"))]
const EXPOSED_LOCATIONS: [Location; 3] = [Location::Airlock, Location::System, Location::EncryptedRoot];

static MAX_LUN: AtomicU8 = AtomicU8::new(0);

const DUMMY_DISK_BLOCKS: usize = 1024 * 1024 / 512;
const README_FILE_NAME: &str = "Airlock disabled.txt";
const README_FILE_CONTENTS: &[u8] = b"Airlock is disabled. To enable it:
1. Go to Files
2. Navigate to Airlock
3. Click the menu
4. Click \"Airlock Read&Write\" or \"Airlock Read Only\"
";

const ENDPOINTS: [EndpointProperties; 2] = [
    EndpointProperties {
        ep_type: EndpointType::Bulk,
        ep_direction: EndpointDirection::In,
        max_packet_len: 512,
        interval: 0,
        use_dma: true,
    },
    EndpointProperties {
        ep_type: EndpointType::Bulk,
        ep_direction: EndpointDirection::Out,
        max_packet_len: 512,
        interval: 0,
        use_dma: true,
    },
];

#[derive(Default)]
pub(crate) struct SetupResponder;
impl ServerMessages for SetupResponder {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] {
        &[(SetupPacketCallback::ID, server::handle_blocking_archive_message::<SetupPacketCallback, _>)]
    }
}
impl Server for SetupResponder {}

impl BlockingArchiveHandler<SetupPacketCallback> for SetupResponder {
    fn handle(
        &mut self,
        SetupPacketCallback(msg): SetupPacketCallback,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        log::debug!("Setup packet: {msg:02x?}");
        // Get Max LUN (see Universal Serial Bus Mass Storage Class Bulk-Only Transport Table 3.2)
        if msg.request_type == 0b10100001 && msg.request == 0b11111110 {
            Some(vec![MAX_LUN.load(std::sync::atomic::Ordering::SeqCst)])
        } else {
            None
        }
    }
}

struct BufferWrapper(xous::MemoryRange);

impl mass_storage::Buffer for BufferWrapper {
    fn new(size: usize) -> Self {
        Self(xous::map_memory(None, None, size, MemoryFlags::W | MemoryFlags::POPULATE).unwrap())
    }

    fn as_slice(&self) -> &[u8] { self.0.as_slice() }

    fn as_slice_mut(&mut self) -> &mut [u8] { self.0.as_slice_mut() }
}

impl Drop for BufferWrapper {
    fn drop(&mut self) { xous::unmap_memory(self.0).ok(); }
}

struct UsbWrapper<'a> {
    ep_in: &'a mut UsbEmulatedEndpoint,
    ep_out: &'a mut UsbEmulatedEndpoint,
}

impl<'a> UsbEmulationCommands<BufferWrapper> for UsbWrapper<'a> {
    fn bulk_rx(
        &mut self,
        buffer: &mut BufferWrapper,
        len: usize,
    ) -> core::result::Result<usize, mass_storage::UsbError> {
        self.ep_out.read_buf(buffer.0, len as u16).map_err(|e| {
            log::debug!("Error while reading {len} bytes: {e:?}");
            match e {
                usb::error::UsbError::HostDisconnected => mass_storage::UsbError::Disconnected,
                _ => mass_storage::UsbError::Other,
            }
        })
    }

    fn bulk_tx(
        &mut self,
        buffer: &BufferWrapper,
        len: usize,
    ) -> core::result::Result<usize, mass_storage::UsbError> {
        self.ep_in.write_buf(buffer.0, len).map_err(|e| {
            log::debug!("Error while writing {len} bytes: {e:?}");
            match e {
                usb::error::UsbError::HostDisconnected => mass_storage::UsbError::Disconnected,
                _ => mass_storage::UsbError::Other,
            }
        })
    }
}

struct BlockDeviceEmulator {
    fs: FileSystem,
    read_only: bool,
}

impl BlockDeviceEmulator {
    pub fn new(read_only: bool) -> Self { Self { fs: FileSystem::default(), read_only } }
}

impl BlockDeviceCommands<BufferWrapper> for BlockDeviceEmulator {
    fn read_blocks(
        &mut self,
        lun: u8,
        buffer: &mut BufferWrapper,
        block_idx: u32,
        block_num: usize,
    ) -> Result<(), mass_storage::BlockDeviceError> {
        match self.fs.read_blocks(EXPOSED_LOCATIONS[lun as usize], block_idx, block_num, buffer.0) {
            Ok(_) => Ok(()),
            Err(e) => {
                log::error!("Error reading blocks: {e}");
                Err(mass_storage::BlockDeviceError::Other)
            }
        }
    }

    fn write_blocks(
        &mut self,
        lun: u8,
        buffer: &BufferWrapper,
        block_idx: u32,
        block_num: usize,
    ) -> Result<(), mass_storage::BlockDeviceError> {
        if self.read_only {
            return Err(mass_storage::BlockDeviceError::Other);
        }
        match self.fs.write_blocks(EXPOSED_LOCATIONS[lun as usize], block_idx, block_num, buffer.0) {
            Ok(_) => Ok(()),
            Err(e) => {
                log::error!("Error writing blocks: {e}");
                Err(mass_storage::BlockDeviceError::Other)
            }
        }
    }

    fn flush(&mut self, lun: u8) -> Result<(), mass_storage::BlockDeviceError> {
        match self.fs.flush(EXPOSED_LOCATIONS[lun as usize]) {
            Ok(_) => Ok(()),
            Err(e) => {
                log::error!("Error flushing blocks: {e}");
                Err(mass_storage::BlockDeviceError::Other)
            }
        }
    }

    fn block_count(&self, lun: u8) -> usize {
        self.fs.block_count(EXPOSED_LOCATIONS[lun as usize]).unwrap_or(0)
    }

    fn max_luns(&self) -> u8 { EXPOSED_LOCATIONS.len() as u8 - 1 }

    fn allowed_access(&self) -> mass_storage::AllowedAccess {
        if self.read_only {
            mass_storage::AllowedAccess::ReadOnly
        } else {
            mass_storage::AllowedAccess::ReadWrite
        }
    }
}

struct DummyDisk {
    image: MemoryRange,
}

impl BlockDeviceCommands<BufferWrapper> for DummyDisk {
    fn read_blocks(
        &mut self,
        _lun: u8,
        buffer: &mut BufferWrapper,
        block_idx: u32,
        block_num: usize,
    ) -> Result<(), mass_storage::BlockDeviceError> {
        buffer
            .0
            .subrange(0, block_num * 512)
            .ok_or(mass_storage::BlockDeviceError::OutOfRange)?
            .as_slice_mut::<u32>()
            .copy_from_slice(
                self.image
                    .subrange(block_idx as usize * 512, block_num * 512)
                    .ok_or(mass_storage::BlockDeviceError::OutOfRange)?
                    .as_slice(),
            );
        Ok(())
    }

    fn write_blocks(
        &mut self,
        _lun: u8,
        _buffer: &BufferWrapper,
        _block_idx: u32,
        _block_num: usize,
    ) -> Result<(), mass_storage::BlockDeviceError> {
        Err(mass_storage::BlockDeviceError::Other)
    }

    fn flush(&mut self, _lun: u8) -> Result<(), mass_storage::BlockDeviceError> { Ok(()) }

    fn block_count(&self, _lun: u8) -> usize { DUMMY_DISK_BLOCKS }

    fn max_luns(&self) -> u8 { 0 }

    fn allowed_access(&self) -> mass_storage::AllowedAccess { mass_storage::AllowedAccess::ReadOnly }
}

fn prepare_dummy_disk_image() -> MemoryRange {
    let mut result = xous::map_memory(None, None, DUMMY_DISK_BLOCKS * 512, MemoryFlags::W).unwrap();
    fatfs::format_volume(
        std::io::Cursor::new(result.as_slice_mut()),
        fatfs::FormatVolumeOptions::new()
            .volume_label(*b"AIRLOCK    ")
            .total_sectors(DUMMY_DISK_BLOCKS as u32)
            .fat_type(fatfs::FatType::Fat32),
    )
    .expect("format dummy fs");
    let fs = fatfs::FileSystem::new(std::io::Cursor::new(result.as_slice_mut()), fatfs::FsOptions::new())
        .expect("open dummy fs");
    fs.root_dir()
        .create_file(README_FILE_NAME)
        .expect("create readme file")
        .write_all(README_FILE_CONTENTS)
        .expect("write readme text");

    core::mem::drop(fs);

    result
}

fn run_emulation(
    ep_in: &mut UsbEmulatedEndpoint,
    ep_out: &mut UsbEmulatedEndpoint,
    disk: impl BlockDeviceCommands<BufferWrapper>,
) {
    MAX_LUN.store(disk.max_luns(), Ordering::SeqCst);
    let e = MassStorageEmulation::new(UsbWrapper { ep_in, ep_out }, disk).run();
    log::info!("Backend exited with {e:?}");
}

pub fn start() -> Result<(), crate::error::MassStorageEmulationError> {
    FileSystem::default().wait_for_filesystem(Location::User);
    let mut usb_api = UsbDeviceEmulation::default();
    let (usb_interface, [mut ep_in, mut ep_out]) = usb_api.register_interface(
        UsbInterfaceConfig::new(
            INTERFACE_NUMBER,
            INTERFACE_CLASS,
            INTERFACE_SUBCLASS,
            INTERFACE_PROTOCOL,
            &ENDPOINTS,
        )
        .with_setup_responder(Some(SetupResponder)),
    )?;

    let mut fs = FileSystem::default();

    let worker = worker::WorkerHandle::default();

    worker
        .spawn({
            let mut airlock_updates = worker
                .subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
                    settings::messages::SubscribeAirlockMode,
                );
            async move {
                let mut previous_mode = None;
                while let Some(mode) = airlock_updates.next().await {
                    if previous_mode != Some(mode) {
                        log::debug!("Airlock config changed, resetting controller");
                        UsbDeviceEmulation::default().reset_controller();
                        previous_mode = Some(mode);
                    }
                }
            }
        })
        .detach();

    let mut _umount_task;

    let settings = SettingsApi::default();

    let dummy_disk_image = prepare_dummy_disk_image();
    usb_interface.set_enabled(true)?;

    loop {
        log::info!("Waiting for connection");
        usb_api.wait_for_connection()?;
        let mode = settings.get_airlock_mode();
        log::info!("Starting mass storage emulation in {mode:?} mode");
        match mode {
            AirlockMode::Disabled => {
                run_emulation(&mut ep_in, &mut ep_out, DummyDisk { image: dummy_disk_image })
            }
            AirlockMode::ReadOnly | AirlockMode::ReadWrite => {
                _umount_task = None;
                if let Err(e) = fs.unmount_airlock() {
                    log::error!("Couldn't unmount airlock: {e:?}");
                    // There was a problem setting up airlock.
                    // Let's wait for a disconnection before retrying.
                    while usb_api.is_connected()? {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    continue;
                }
                run_emulation(
                    &mut ep_in,
                    &mut ep_out,
                    BlockDeviceEmulator::new(mode == AirlockMode::ReadOnly),
                );
                _umount_task = Some(worker.spawn({
                    let mut fs = fs.clone();
                    let sleep = worker.sleep(std::time::Duration::from_millis(2000));
                    async move {
                        sleep.await;
                        fs.mount_airlock().ok();
                    }
                }))
            }
        };
    }
}
