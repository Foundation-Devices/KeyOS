#![cfg_attr(target_os = "none", no_std)]

pub mod api;

use core::ops::Deref;

use num_traits::ToPrimitive;
use xous::{send_message, Error, CID};

#[derive(Debug)]
pub struct Ticktimer {
    conn: CID,
}

impl Default for Ticktimer {
    fn default() -> Self {
        let conn = xous::connect(xous::SID::from_bytes(b"ticktimer-server").unwrap())
            .expect("Could not connect to ticktimer");
        Ticktimer { conn }
    }
}

impl Ticktimer {
    /// Return the number of milliseconds that have elapsed since boot. The returned
    /// value is guaranteed to always be the same or greater than the previous value,
    /// even through suspend/resume cycles.
    ///
    /// # Returns:
    ///
    /// * A `u64` that is the number of nanoseconds elapsed since boot.
    pub fn elapsed_ns(&self) -> u64 {
        let response = send_message(
            self.conn,
            xous::Message::new_blocking_scalar(api::Opcode::Elapsed.to_usize().unwrap(), 0, 0, 0, 0),
        )
        .expect("Ticktimer: failure to send message to Ticktimer");
        if let xous::Result::Scalar2(lower, upper) = response {
            lower as u64 | ((upper as u64) << 32)
        } else {
            panic!("Ticktimer elapsed_ms(): unexpected return value.");
        }
    }

    pub fn elapsed_ms(&self) -> u64 { self.elapsed_ns() / 1000000 }

    /// Sleep for at least `ns` nanoseconds. Blocks until the requested time has passed.
    ///
    /// # Arguments:
    ///
    /// * ns: how many nanoseconds to sleep for
    pub fn sleep_ns(&self, ns: u64) -> Result<(), Error> {
        send_message(
            self.conn,
            xous::Message::new_blocking_scalar(
                api::Opcode::Sleep.to_usize().unwrap(),
                (ns & 0xFFFFFFFF) as usize,
                (ns >> 32) as usize,
                0,
                0,
            ),
        )
        .map(|_| ())
    }
}

#[derive(Debug)]
pub struct TicktimerPrivileged(Ticktimer);

impl Default for TicktimerPrivileged {
    fn default() -> Self {
        let names = xous_api_names::XousNames::new().unwrap();
        let conn = names.request_connection_blocking("os/ticktimer").unwrap();
        Self(Ticktimer { conn })
    }
}

impl Deref for TicktimerPrivileged {
    type Target = Ticktimer;

    fn deref(&self) -> &Self::Target { &self.0 }
}

impl TicktimerPrivileged {
    /// Set the current system time with a timestamp in nanoseconds since the unix epoch
    pub fn set_system_time(&self, ns: u64) {
        send_message(
            self.conn,
            xous::Message::new_blocking_scalar(
                api::Opcode::SetSystemTime.to_usize().unwrap(),
                (ns & 0xFFFFFFFF) as usize,
                (ns >> 32) as usize,
                0,
                0,
            ),
        )
        .expect("Couldn't set system time");
    }
}

pub struct TicktimerCallback {
    ticktimer: Ticktimer,
    pid: xous::PID,
    cid_ticktimer_side: xous::CID,
}

impl TicktimerCallback {
    pub fn new(receiving_server: xous::SID) -> Result<Self, Error> {
        let ticktimer = Ticktimer::default();
        let pid = xous::get_remote_pid(ticktimer.conn)?;
        let cid_ticktimer_side = xous::connect_for_process(pid, receiving_server)?;
        Ok(Self { ticktimer, cid_ticktimer_side, pid })
    }

    pub fn pid(&self) -> xous::PID { self.pid }

    /// Request ticktimer to send a non-blocking scalar message with msg_id after waiting for `timeout_ms`
    /// Calling it multiple times with the same msg_id resets the timer.
    /// Calling it with timeout_ms = 0 cancels the callback.
    pub fn request(&self, timeout_ms: usize, msg_id: usize, data: usize) {
        xous::allow_messages_on_connection(self.pid, self.cid_ticktimer_side, msg_id..(msg_id + 1)).unwrap();
        send_message(
            self.ticktimer.conn,
            xous::Message::new_scalar(
                api::Opcode::RequestCallback.to_usize().unwrap(),
                timeout_ms,
                msg_id,
                data,
                self.cid_ticktimer_side as usize,
            ),
        )
        .expect("Couldn't send RequestCallback message");
    }

    pub fn cancel(&self, msg_id: usize) { self.request(0, msg_id, 0) }
}

impl Drop for Ticktimer {
    fn drop(&mut self) { xous::disconnect(self.conn).unwrap(); }
}
