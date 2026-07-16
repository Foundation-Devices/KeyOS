use std::{collections::HashMap, io, sync::LazyLock};

use utralib::map::{MEMORY_REGIONS, PERIPHERALS};
use xous::syscall::SysCallNumber;

use crate::xous_arguments::{XousArgument, XousArgumentCode, XousSize};

pub struct MemoryPermission {
    pid: u8,
    regions: Vec<(u32, u32, &'static str)>,
}

impl MemoryPermission {
    pub fn new(pid: u8, regions: &[String]) -> Self {
        Self {
            pid,
            regions: regions
                .iter()
                .map(|region_name| {
                    let region_desc = MEMORY_REGIONS
                        .iter()
                        .chain(PERIPHERALS.iter())
                        .find(|m| m.0 == region_name)
                        .unwrap_or_else(|| panic!("Could not find memory region {region_name}"));
                    (region_desc.1.start as u32, region_desc.1.end as u32, region_desc.0)
                })
                .collect(),
        }
    }
}

impl std::fmt::Display for MemoryPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "        memory permissions: ")?;
        for (start, end, name) in &self.regions {
            write!(f, "{name}({start:08x}-{end:08x}), ")?;
        }
        writeln!(f)
    }
}

impl XousArgument for MemoryPermission {
    fn code(&self) -> XousArgumentCode { u32::from_le_bytes(*b"PMem") }

    fn length(&self) -> XousSize { 4 + 8 * self.regions.len() as u32 }

    fn serialize(&self, output: &mut dyn io::Write) -> io::Result<usize> {
        output.write_all(&[self.pid, 0, 0, 0])?;
        for region in &self.regions {
            output.write_all(&region.0.to_le_bytes())?;
            output.write_all(&region.1.to_le_bytes())?;
        }
        Ok(4 + 8 * self.regions.len())
    }
}

// XXX: The way we create this mapping (using the debug formatting of the enum) is pretty hacky, but this was
// the most straightforward way I could think of to not have to import some macro crate to xous-rs.
// (It is used by the stdlib, so it should have very minimal dependencies)
static SYSCALL_MAP: LazyLock<HashMap<String, u8>> = LazyLock::new(|| {
    (0..64)
        .filter_map(|n| {
            let enum_n = SysCallNumber::from(n);
            if enum_n != SysCallNumber::Invalid {
                Some((format!("{enum_n:?}"), n as u8))
            } else {
                None
            }
        })
        .collect()
});

/// Resolve a list of syscall names into a bitmask. Panics if a name is unknown.
pub fn syscall_mask(names: &[String]) -> u64 {
    let mut mask = 0u64;
    for name in names {
        let number = *SYSCALL_MAP.get(name).unwrap_or_else(|| panic!("Could not find the '{name}' syscall"));
        mask |= 1 << number;
    }
    mask
}

pub struct SyscallPermission {
    pid: u8,
    syscalls: Vec<String>,
    mask: u64,
}

impl SyscallPermission {
    pub fn new(pid: u8, syscalls: &[String]) -> Self {
        Self { pid, mask: syscall_mask(syscalls), syscalls: syscalls.to_vec() }
    }
}

impl std::fmt::Display for SyscallPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "        syscall permissions: ")?;
        for name in &self.syscalls {
            let number = SYSCALL_MAP.get(name).unwrap();
            write!(f, "{name}({number}), ")?;
        }
        writeln!(f)
    }
}

impl XousArgument for SyscallPermission {
    fn code(&self) -> XousArgumentCode { u32::from_le_bytes(*b"PSys") }

    fn length(&self) -> XousSize { 4 + 8 }

    fn serialize(&self, output: &mut dyn io::Write) -> io::Result<usize> {
        let mut written = output.write(&[self.pid, 0, 0, 0])?;
        written += output.write(&self.mask.to_le_bytes())?;
        Ok(written)
    }
}
