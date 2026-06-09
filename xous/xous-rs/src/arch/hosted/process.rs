use core::convert::TryFrom;

use super::CHILD_PROCESS_ADDRESS;
use crate::AppId;
pub use crate::PID;

impl core::fmt::Display for AppId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for i in self.0 {
            write!(f, "{:02x}", i)?;
        }

        Ok(())
    }
}

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
    // Capture the binary path before `args` is shadowed below; the hosted quit
    // watchdog uses it to spot the window-owning simulator processes.
    #[cfg(unix)]
    let command_path = args.command.clone();
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
        .map(|handle| {
            #[cfg(unix)]
            watch_window_process(&command_path, handle.id());
            (startup.pid, ProcessHandle(handle))
        })
        .map_err(|_| {
            // eprintln!("couldn't start command: {}", e);
            crate::Error::InternalError
        })
}

/// Hosted simulator quit watchdog. Spawned for the processes that own a desktop
/// window — the gui-server device screen and the simulator control panel: if one
/// exits for ANY reason (close button, Cmd-Q, or even a hard kill that runs no
/// cleanup), tear the whole simulator down by SIGINT-ing our process group — the
/// same teardown as terminal Ctrl-C — so both windows close together instead of
/// leaving one orphaned. Non-window processes are ignored. A name mismatch is a
/// harmless no-op (the in-window close handlers still cover the normal cases).
#[cfg(unix)]
fn watch_window_process(command_path: &str, os_pid: u32) {
    let basename =
        std::path::Path::new(command_path).file_name().and_then(|name| name.to_str()).unwrap_or(command_path);
    // Exact basename match so e.g. "simulator-cli" doesn't trip it.
    if !matches!(basename, "gui-server" | "simulator" | "foundation-simulator") {
        return;
    }
    let pid = os_pid as i32;
    let _ = std::thread::Builder::new().name(format!("sim quit watchdog (pid {pid})")).spawn(move || {
        // Block until this window-owning child exits, reaping it. We must NOT
        // poll `kill(pid, 0)`: the hosted kernel never reaps its children, so
        // once a window process exits it lingers as a zombie and
        // `kill(pid, 0)` keeps reporting it alive — the watchdog would never
        // fire (that's why Cmd-Q on the control panel didn't close the device
        // screen). `waitpid` returns on (and reaps) the real exit instead.
        let mut status: libc::c_int = 0;
        loop {
            let result = unsafe { libc::waitpid(pid, &mut status as *mut libc::c_int, 0) };
            // Retry only if interrupted; any other return means it's gone.
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        // The window process is gone — tear down the whole simulator group,
        // the same teardown as terminal Ctrl-C.
        // SAFETY: kill(2) with pid 0 sends SIGINT to our entire process group.
        unsafe { libc::kill(0, libc::SIGINT) };
    });
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
