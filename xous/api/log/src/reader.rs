use num_traits::ToPrimitive;

pub struct LogReader(xous::CID);

impl Default for LogReader {
    fn default() -> Self {
        let names = xous_api_names::XousNames::new().unwrap();
        let cid = names.request_connection_blocking("os/log").unwrap();
        Self(cid)
    }
}

impl LogReader {
    pub fn read(&self, buffer: xous::MemoryRange) -> usize {
        let result = xous::send_message(
            self.0,
            xous::Message::new_lend_mut(crate::api::Opcode::ReadLogs.to_usize().unwrap(), buffer, None, None),
        )
        .unwrap();
        if let xous::Result::MemoryReturned(_offset, valid) = result {
            valid.map(|v| v.get()).unwrap_or_default()
        } else {
            0
        }
    }
}
