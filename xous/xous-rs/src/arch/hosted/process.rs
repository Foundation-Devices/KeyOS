use core::convert::TryFrom;

use super::CHILD_PROCESS_ADDRESS;
use crate::AppId;
pub use crate::PID;

impl From<&str> for AppId {
    fn from(v: &str) -> AppId {
        let mut key = [0u8; 16];
        for (src, dest) in v.as_bytes().chunks(2).zip(key.iter_mut()) {
            *dest = u8::from_str_radix(core::str::from_utf8(src).unwrap(), 16).unwrap();
        }
        AppId(key)
    }
}

/// Describes all parameters that are required to start a new process
/// on this platform.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ProcessInit {
    pub app_id: AppId,
}

#[derive(Debug)]
pub struct ProcessArgs {
    app_id: AppId,
    command: String,
    name: String,
}

impl ProcessArgs {
    pub fn new(app_id: AppId, name: &str, command: &str) -> ProcessArgs {
        ProcessArgs { app_id, command: command.to_owned(), name: name.to_owned() }
    }
}

impl From<&ProcessInit> for [usize; 7] {
    fn from(src: &ProcessInit) -> [usize; 7] {
        let app_id_words: [u32; 4] = (&src.app_id).into();
        [app_id_words[0] as _, app_id_words[1] as _, app_id_words[2] as _, app_id_words[3] as _, 0, 0, 0]
    }
}

impl TryFrom<[usize; 7]> for ProcessInit {
    type Error = crate::Error;

    fn try_from(src: [usize; 7]) -> Result<ProcessInit, Self::Error> {
        let app_id_words = [src[0] as u32, src[1] as u32, src[2] as u32, src[3] as u32];
        Ok(ProcessInit { app_id: app_id_words.into() })
    }
}

/// This is returned when a process is created
#[derive(Debug, PartialEq)]
pub struct ProcessStartup {
    /// The process ID of the new process
    pid: crate::PID,
}

impl ProcessStartup {
    pub fn new(pid: crate::PID) -> Self { ProcessStartup { pid } }

    pub fn pid(&self) -> crate::PID { self.pid }
}

impl core::fmt::Display for ProcessStartup {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { write!(f, "{}", self.pid) }
}

impl From<&[usize; 7]> for ProcessStartup {
    fn from(src: &[usize; 7]) -> ProcessStartup {
        ProcessStartup { pid: crate::PID::new(src[0] as _).unwrap() }
    }
}

impl From<[usize; 8]> for ProcessStartup {
    fn from(src: [usize; 8]) -> ProcessStartup {
        let pid = crate::PID::new(src[1] as _).unwrap();
        ProcessStartup { pid }
    }
}

impl From<&ProcessStartup> for [usize; 7] {
    fn from(startup: &ProcessStartup) -> [usize; 7] { [startup.pid.get() as _, 0, 0, 0, 0, 0, 0] }
}

#[derive(Debug)]
pub struct ProcessHandle(std::process::Child);

impl ProcessHandle {
    pub fn kill(&mut self) -> std::io::Result<()> { self.0.kill() }
}

/// If no connection exists, create a new connection to the server. This means
/// our parent PID will be PID1. Otherwise, reuse the same connection.
pub fn create_process_pre(args: &ProcessArgs) -> core::result::Result<ProcessInit, crate::Error> {
    Ok(ProcessInit { app_id: args.app_id })
}

/// Launch a new process with the current PID as the parent.
pub fn create_process_post(
    args: ProcessArgs,
    init: ProcessInit,
    startup: ProcessStartup,
) -> core::result::Result<(PID, ProcessHandle), crate::Error> {
    use std::process::Command;
    let mut server_env = format!("{}", CHILD_PROCESS_ADDRESS.lock().unwrap());
    if server_env.split(':').last().unwrap() == "0" {
        server_env = std::env::var("XOUS_SERVER").unwrap();
    }
    let pid_env = format!("{}", startup.pid);
    let process_name_env = args.name.to_string();
    let process_key_env: String = format!("{}", init.app_id);
    let (shell, args) = if cfg!(windows) {
        ("cmd", ["/C", &args.command])
    } else if cfg!(unix) {
        ("sh", ["-c", &args.command])
    } else {
        panic!("unrecognized platform -- don't know how to shell out");
    };

    // println!("Launching process...");
    Command::new(shell)
        .args(&args)
        .env("XOUS_SERVER", server_env)
        .env("XOUS_PID", pid_env)
        .env("XOUS_PROCESS_NAME", process_name_env)
        .env("XOUS_PROCESS_KEY", process_key_env)
        .spawn()
        .map(|handle| (startup.pid, ProcessHandle(handle)))
        .map_err(|_| {
            // eprintln!("couldn't start command: {}", e);
            crate::Error::InternalError
        })
}

pub fn wait_process(mut joiner: ProcessHandle) -> crate::SysCallResult {
    joiner.0.wait().or(Err(crate::Error::InternalError)).and_then(|e| {
        if e.success() {
            Ok(crate::Result::Ok)
        } else {
            Err(crate::Error::UnknownError)
        }
    })
}
