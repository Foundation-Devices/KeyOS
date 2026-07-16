#![cfg_attr(any(feature = "no_std", target_os = "none"), no_std)]

use core::fmt::Write;
use core::num::NonZeroUsize;
use core::ptr::addr_of;

use cursor::BufferWrapper;
use num_traits::ToPrimitive;
use xous_api_ticktimer::Ticktimer;

pub mod api;
mod cursor;
pub mod reader;

#[derive(Debug)]
pub enum LogError {
    LoggerExists,
    NoConnection,
}

struct XousLogger {
    package: &'static str,
    connection: u32,
    pid: Option<xous::PID>,
    ticktimer: Option<Ticktimer>,
}
static mut XOUS_LOGGER: XousLogger = XousLogger { package: "-", connection: 0, ticktimer: None, pid: None };

impl XousLogger {
    fn log_impl(&self, record: &log::Record) {
        let interrupts_enabled = interrupts_enabled();
        let mut buf =
            xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE).unwrap();
        let mut wrapper = BufferWrapper::new(buf.as_slice_mut());
        let level = match record.level() {
            log::Level::Error => "ERR",
            log::Level::Warn => "WRN",
            log::Level::Info => "INF",
            log::Level::Debug => "DBG",
            log::Level::Trace => "TRC",
        };
        let pid = match self.pid {
            Some(pid) => pid.get(),
            None => 0,
        };
        if interrupts_enabled {
            let ticks = self.ticktimer.as_ref().map_or(0, |tt| tt.elapsed_ms());
            let ticks_s = ticks / 1000;
            let ticks_ms = ticks % 1000;
            write!(wrapper, "[{ticks_s:>4}.{ticks_ms:03}] {level} {pid:>2} ",).ok();
        } else {
            write!(wrapper, "[   IRQ  ] {level} {pid:>2} ",).ok();
        }

        let module = record.module_path().unwrap_or_default();
        if !module.starts_with(self.package) {
            write!(wrapper, "{}..", self.package).ok();
        };
        writeln!(wrapper, "{module}: {}", record.args()).ok();
        let len = wrapper.len();
        let msg = xous::Message::new_move(
            crate::api::Opcode::StandardError.to_usize().unwrap(),
            buf,
            None,
            NonZeroUsize::new(len),
        );

        if interrupts_enabled {
            xous::send_message(self.connection, msg).unwrap();
        } else {
            // We are running in an interrupt handler or PID1.
            // This may drop logs if the log server's queue is full, but we must not block here.
            // Also swallow any errors because we shouldn't panic in interrupt handlers.
            if xous::try_send_message(self.connection, msg).is_err() {
                xous::unmap_memory(buf).ok();
            }
        }
    }
}

impl log::Log for XousLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool { true }

    fn log(&self, record: &log::Record) { self.log_impl(record); }

    fn flush(&self) {}
}

pub fn init(package: &'static str) -> Result<(), LogError> {
    init_common(
        package,
        xous::try_connect(xous::SID::from_bytes(b"xous-log-server ").unwrap())
            .or(Err(LogError::NoConnection))?,
        Some(Ticktimer::default()),
    )
}

pub fn init_wait(package: &'static str) -> Result<(), LogError> {
    init_common(
        package,
        xous::connect(xous::SID::from_bytes(b"xous-log-server ").unwrap()).or(Err(LogError::NoConnection))?,
        Some(Ticktimer::default()),
    )
}

pub fn init_wait_noticks(package: &'static str) -> Result<(), LogError> {
    init_common(
        package,
        xous::connect(xous::SID::from_bytes(b"xous-log-server ").unwrap()).or(Err(LogError::NoConnection))?,
        None,
    )
}

fn init_common(package: &'static str, connection: u32, ticktimer: Option<Ticktimer>) -> Result<(), LogError> {
    unsafe {
        XOUS_LOGGER.ticktimer = ticktimer;
        XOUS_LOGGER.connection = connection;
        XOUS_LOGGER.package = package;
        XOUS_LOGGER.pid = Some(xous::current_pid().unwrap());
    };

    log::set_logger(unsafe { &*addr_of!(XOUS_LOGGER) }).map_err(|_| LogError::LoggerExists)?;
    log::set_max_level(log::LevelFilter::Info);
    Ok(())
}

#[cfg(keyos)]
fn interrupts_enabled() -> bool {
    let cpsr: u32;
    unsafe {
        core::arch::asm!(
            "mrs {cpsr}, cpsr",
            cpsr = out(reg) cpsr,
        )
    }
    // Check IRQ and FIQ mask bits (bits 6 and 7)
    cpsr & 0xC0 == 0
}

#[cfg(not(keyos))]
fn interrupts_enabled() -> bool { true }
