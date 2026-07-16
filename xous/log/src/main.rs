#![no_std]

use core::{fmt::Write, num::NonZeroUsize};

use num_traits::FromPrimitive;
use xous::{MemoryFlags, MemoryRange};
use xous_api_log::api;

const RINGBUFFER_SIZE: usize = 16 * 1024;
const LOG_RECORD_TERMINATOR: u8 = 0x1e;

macro_rules! log {
    ($server:expr, $($arg:tt)*) => {{
        $server.ring.log_internal(format_args!($($arg)*));
    }};
}

struct LogServer {
    ring: RingStore,
    readers: [LogReader; 4],
    panic_buffer: MemoryRange,
}

struct RingStore {
    ringbuffer: [u8; RINGBUFFER_SIZE],
    write_offset: usize,
    buffer_filled: bool,
}

#[derive(Default)]
struct LogReader {
    pid: Option<xous::PID>,
    read_offset: usize,
    read_msg: Option<xous::MessageEnvelope>,
}

impl Default for RingStore {
    fn default() -> Self { Self { ringbuffer: [0; RINGBUFFER_SIZE], write_offset: 0, buffer_filled: false } }
}

impl RingStore {
    fn write_bytes(&mut self, b: &[u8]) {
        let len = b.len().min(RINGBUFFER_SIZE);

        // Part 1: from current cursor to end
        //        [         |--->]
        let part1 = len.min(RINGBUFFER_SIZE - self.write_offset);
        self.ringbuffer[self.write_offset..self.write_offset + part1].copy_from_slice(&b[..part1]);

        // Part 2: from beginning
        //        [---->    |    ]
        let part2 = len - part1;
        if part2 > 0 {
            self.ringbuffer[..part2].copy_from_slice(&b[part1..part1 + part2]);
        }
        // Note: we might overtake the readers' read_offset here, but that means that they
        //       are way too slow, so we will be losing logs anyway, we might as well lose
        //       a full ringbuffer's worth.
        self.write_offset += len;
        if self.write_offset > RINGBUFFER_SIZE {
            self.buffer_filled = true;
            self.write_offset %= RINGBUFFER_SIZE;
        }
    }

    fn write_terminated(&mut self, payload: &[u8]) {
        self.write_bytes(payload);
        self.write_bytes(&[LOG_RECORD_TERMINATOR]);
    }

    fn log_internal(&mut self, args: core::fmt::Arguments<'_>) {
        core::fmt::write(self, args).ok();
        self.write_bytes(&[LOG_RECORD_TERMINATOR]);
    }
}

impl Write for RingStore {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

impl Default for LogServer {
    fn default() -> Self {
        Self {
            ring: Default::default(),
            readers: Default::default(),
            panic_buffer: xous::map_memory(None, None, 0x1000, MemoryFlags::W | MemoryFlags::POPULATE)
                .unwrap(),
        }
    }
}
impl LogServer {
    fn handle_message(&mut self, opcode: api::Opcode, envelope: xous::MessageEnvelope) {
        use api::Opcode::*;
        match opcode {
            StandardOutput | StandardError => {
                let Some(mem) = envelope.body.memory_message() else {
                    return;
                };
                let len = mem.valid.map(|v| v.get()).unwrap_or_default().min(mem.buf.len());
                self.ring.write_terminated(&mem.buf.as_slice()[..len]);
            }
            ProgramName => {}

            // Deprecated panic messages that are still sent by the stdlib.
            // These are discarded and we rely only on kernel functionality to get and print panics.
            PanicStarted | PanicMessage0 | PanicMessage1 | PanicMessage2 | PanicMessage3 | PanicMessage4
            | PanicMessage5 | PanicMessage6 | PanicMessage7 | PanicMessage8 | PanicMessage9
            | PanicMessage10 | PanicMessage11 | PanicMessage12 | PanicMessage13 | PanicMessage14
            | PanicMessage15 | PanicMessage16 | PanicMessage17 | PanicMessage18 | PanicMessage19
            | PanicMessage20 | PanicMessage21 | PanicMessage22 | PanicMessage23 | PanicMessage24
            | PanicMessage25 | PanicMessage26 | PanicMessage27 | PanicMessage28 | PanicMessage29
            | PanicMessage30 | PanicMessage31 | PanicMessage32 | PanicFinished => {}

            ReadLogs => {
                let Some(pid) = envelope.sender.pid() else {
                    return;
                };
                for reader in &mut self.readers {
                    if reader.pid.is_none() {
                        // We reached the end, allocate into this slot
                        reader.pid = Some(pid);
                        if self.ring.buffer_filled {
                            // We already filled the ringbuffer, send the whole thing:
                            // [---->WR----]
                            reader.read_offset = (self.ring.write_offset + 1) % RINGBUFFER_SIZE;
                        } else {
                            // We are still filling the buffer:
                            // [R---->W    ]
                            reader.read_offset = 0;
                        }
                    }
                    if reader.pid == Some(pid) {
                        // If read_msg is already filled (very unlikely), dropping it is OK, it will just
                        // return the blocking call.
                        reader.read_msg = Some(envelope);
                        return;
                    }
                }
            }
            ProcessDisconnected => {
                if let Ok((panic_pid, panic_size)) = xous::get_panic_message(self.panic_buffer) {
                    if xous::PID::new(panic_pid) == envelope.sender.pid() {
                        write!(self.ring, "[LOG] PANIC in PID {panic_pid}: ").ok();
                        self.ring.write_bytes(&self.panic_buffer.as_slice()[..panic_size]);
                        self.ring.write_bytes(&[b'\n', LOG_RECORD_TERMINATOR]);
                    }
                }
            }
        };
    }

    fn send_logs(&mut self) {
        for reader in &mut self.readers {
            if reader.read_offset == self.ring.write_offset {
                continue;
            }
            let Some(mut envelope) = reader.read_msg.take() else {
                continue;
            };
            let Some(mem) = envelope.body.memory_message_mut() else {
                continue;
            };

            // Cases with big enough message buffer:
            // [   R--->W   ]
            // [->W     R---]

            // Cases with small message buffer:
            // [   R--->  W ]
            // [  W R--->   ]
            // [->  W   R---]

            // Part 1: from current read cursor to end or write cursor
            let part1_end = if reader.read_offset <= self.ring.write_offset {
                self.ring.write_offset
            } else {
                RINGBUFFER_SIZE
            };
            let part1_len = (part1_end - reader.read_offset).min(mem.buf.len());
            mem.buf.as_slice_mut()[..part1_len]
                .copy_from_slice(&self.ring.ringbuffer[reader.read_offset..reader.read_offset + part1_len]);
            mem.valid = NonZeroUsize::new(part1_len);
            reader.read_offset = (reader.read_offset + part1_len) % RINGBUFFER_SIZE;

            // Part 2: from beginning to write cursor
            if reader.read_offset == 0 {
                let part2_len = self.ring.write_offset.min(mem.buf.len() - part1_len);
                mem.buf.as_slice_mut()[part1_len..part1_len + part2_len]
                    .copy_from_slice(&self.ring.ringbuffer[..part2_len]);
                mem.valid = NonZeroUsize::new(part1_len + part2_len);
                reader.read_offset = part2_len;
            }
        }
    }

    fn run(&mut self) -> ! {
        xous::set_thread_priority(xous::ThreadPriority::System8).unwrap();
        log!(self, "[LOG] Starting with PID {}", xous::process::id());
        let server_addr = xous::create_server_with_sid(
            xous::SID::from_bytes(b"xous-log-server ").unwrap(),
            0..api::Opcode::ReadLogs as _,
        )
        .expect("create server");
        xous::register_server_event_handler(
            xous::ServerEvent::Disconnected,
            server_addr,
            api::Opcode::ProcessDisconnected as usize,
        )
        .expect("register_system_event_handler");
        log!(self, "[LOG] Server listening on address {:?}", server_addr);

        let mut counter: usize = 0;
        loop {
            if counter.trailing_zeros() >= 12 {
                log!(self, "[LOG] Counter tick: {}", counter);
            }
            counter += 1;
            let envelope = xous::syscall::receive_message(server_addr).expect("couldn't get address");
            if let Some(opcode) = FromPrimitive::from_usize(envelope.body.id()) {
                self.handle_message(opcode, envelope);
            } else {
                log!(
                    self,
                    "[LOG] Unrecognized opcode from process {}: {}",
                    envelope.sender.pid().map(|v| v.get()).unwrap_or_default(),
                    envelope.body.id()
                );
            }
            self.send_logs();
        }
    }
}

fn main() -> ! { LogServer::default().run() }
