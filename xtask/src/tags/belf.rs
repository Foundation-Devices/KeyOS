use std::fmt;
use std::io;

use xous::AppId;

use crate::xous_arguments::{XousArgument, XousArgumentCode, XousSize};

#[derive(Debug)]
pub struct BinaryElf {
    pid: u8, // Only used for a pretty display
    app_id: AppId,
    program_name: String,
    load_offset: u32,
    data: Vec<u8>,
}

impl fmt::Display for BinaryElf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "    PID {:>2}: {}", self.pid, self.program_name)?;
        writeln!(f, "           size: {} bytes", self.data.len())?;
        writeln!(f, "           appId: 0x{}", self.app_id)
    }
}

impl BinaryElf {
    pub fn new(pid: u8, program_name: String, app_id: AppId, data: Vec<u8>) -> BinaryElf {
        BinaryElf { pid, app_id, program_name, data, load_offset: 0 }
    }
}

impl XousArgument for BinaryElf {
    fn code(&self) -> XousArgumentCode { u32::from_le_bytes(*b"BElf") }

    fn length(&self) -> XousSize { (4 + 4 + 16 + 32) as XousSize }

    fn serialize(&self, output: &mut dyn io::Write) -> io::Result<usize> {
        let mut written = 0;
        written += output.write(&self.load_offset.to_le_bytes())?;
        written += output.write(&(self.data.len() as u32).to_le_bytes())?;
        written += output.write(&self.app_id.0)?;
        written += output.write(self.program_name.as_bytes())?;
        for _ in self.program_name.len()..32 {
            written += output.write(&[0])?;
        }
        Ok(written)
    }

    fn last_data(&self) -> &[u8] { &self.data }

    fn finalize(&mut self, offset: usize) -> usize {
        self.load_offset = offset as u32;

        assert!(offset % crate::tags::PAGE_SIZE == 0, "BElf load offset is not aligned");
        self.data = crate::tags::align_data_up(&self.data, 0);
        self.data.len()
    }
}
