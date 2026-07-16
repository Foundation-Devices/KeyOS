use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, Instant},
};

use xous_api_ticktimer::api::Opcode;

static TIME_OFFSET: AtomicI64 = AtomicI64::new(0);

pub struct XousTickTimer {
    start: Instant,
    sleep_comms: std::sync::mpsc::Sender<Duration>,
}

impl XousTickTimer {
    pub fn new(cid: xous::CID) -> XousTickTimer {
        let (sleep_sender, sleep_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut sleep_duration = sleep_receiver.recv().unwrap();
            loop {
                match sleep_receiver.recv_timeout(sleep_duration) {
                    Ok(new_duration) => sleep_duration = new_duration,
                    Err(e) => match e {
                        std::sync::mpsc::RecvTimeoutError::Timeout => {
                            xous::try_send_message(
                                cid,
                                xous::Message::Scalar(xous::ScalarMessage {
                                    id: Opcode::TimerInterrupt as usize,
                                    arg1: 0,
                                    arg2: 0,
                                    arg3: 0,
                                    arg4: 0,
                                }),
                            )
                            .ok();
                        }
                        std::sync::mpsc::RecvTimeoutError::Disconnected => break,
                    },
                }
            }
        });
        XousTickTimer { start: Instant::now(), sleep_comms: sleep_sender }
    }

    pub fn elapsed_ns(&self) -> u64 { self.start.elapsed().as_nanos() as u64 }

    pub fn start_sleep(&self, ns: u64) { self.sleep_comms.send(Duration::from_nanos(ns)).unwrap(); }

    pub fn get_system_time_ns(&self) -> u64 {
        let real_time =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
                as i64;
        return (real_time - TIME_OFFSET.load(Ordering::SeqCst)) as u64;
    }

    pub fn set_system_time_ns(&self, time: u64) {
        let real_time =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
                as i64;
        TIME_OFFSET.store(real_time - time as i64, Ordering::SeqCst);
    }
}
