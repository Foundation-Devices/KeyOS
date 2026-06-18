// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

pub mod irq;
pub mod mem;
pub mod process;
pub mod rand;
pub mod syscall;

use core::sync::atomic::AtomicU64;
use std::cell::RefCell;
use std::convert::TryInto;
use std::env;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::thread_local;

use crossbeam_channel::{unbounded, Receiver, RecvError, RecvTimeoutError, Sender};
use xous::{AppId, ProcessInit, Result, SysCall, PID, TID};

use crate::arch::process::Process;
use crate::services::SystemServices;

enum ThreadMessage {
    SysCall(PID, TID, SysCall),
    NewConnection(TcpStream, AppId),
    ReloadApp(String),
}

#[derive(Debug)]
enum NewPidMessage {
    NewPid(PID),
}

#[derive(Debug)]
enum ExitMessage {
    Exit,
}

thread_local!(static SEND_ADDR: RefCell<Option<Sender<SocketAddr>>> = RefCell::new(None));

/// Set the network address for this particular thread.
#[allow(dead_code)]
pub fn set_send_addr(send_addr: Sender<SocketAddr>) {
    SEND_ADDR.with(|sa| {
        *sa.borrow_mut() = Some(send_addr);
    });
}

static LOCAL_RNG_STATE: AtomicU64 = AtomicU64::new(2);

#[allow(dead_code)]
pub fn current_pid() -> PID { crate::arch::process::current_pid() }

/// Each client gets its own connection and its own thread, which is handled here.
/// Blocks reading syscalls off the socket until it closes (the kernel shutting down
/// closes every client socket), then terminates the virtual process.
fn handle_connection(mut conn: TcpStream, pid: PID, chn: Sender<ThreadMessage>) {
    loop {
        let mut raw_data = [0u8; 9 * std::mem::size_of::<usize>()];

        // read_exact fails when the connection closes; stop and terminate the process.
        if let Err(e) = conn.read_exact(&mut raw_data) {
            if e.kind() != std::io::ErrorKind::UnexpectedEof {
                eprintln!("KERNEL: PID {pid} client disconnected: {e} -- shutting down virtual process");
            }
            break;
        }

        let mut packet_data = [0usize; 9];
        for (bytes, word) in raw_data.chunks_exact(std::mem::size_of::<usize>()).zip(packet_data.iter_mut()) {
            *word = usize::from_le_bytes(bytes.try_into().unwrap());
        }
        let thread_id = packet_data[0] as TID;
        let mut call = match crate::SysCall::from_args(
            packet_data[1],
            packet_data[2],
            packet_data[3],
            packet_data[4],
            packet_data[5],
            packet_data[6],
            packet_data[7],
            packet_data[8],
        ) {
            Ok(call) => call,
            Err(e) => {
                eprintln!("KERNEL: Received invalid syscall from PID {pid}: {e:?}");
                eprintln!(
                    "Raw packet: {:08x} {} {} {} {} {} {} {}",
                    packet_data[0],
                    packet_data[1],
                    packet_data[2],
                    packet_data[3],
                    packet_data[4],
                    packet_data[5],
                    packet_data[6],
                    packet_data[7]
                );
                continue;
            }
        };

        if let Some(mem) = call.memory() {
            let mut data = vec![0u8; mem.len()];
            if conn.read_exact(&mut data).is_err() {
                break;
            }

            let sliced_data = data.into_boxed_slice();
            assert_eq!(
                sliced_data.len(),
                mem.len(),
                "deconstructed data {} != message buf length {}",
                sliced_data.len(),
                mem.len()
            );
            unsafe {
                call.replace_memory(
                    xous::MemoryRange::new(Box::into_raw(sliced_data) as *mut u8 as usize, mem.len())
                        .unwrap(),
                )
            };
        }

        chn.send(ThreadMessage::SysCall(pid, thread_id, call)).unwrap();
    }
    eprintln!("KERNEL: PID {pid} exited");
    chn.send(ThreadMessage::SysCall(pid, 1, xous::SysCall::TerminateProcess(0))).unwrap();
}

fn listen_thread(
    listen_addr: SocketAddr,
    chn: Sender<ThreadMessage>,
    mut local_addr_sender: Option<Sender<SocketAddr>>,
    new_pid_channel: Receiver<NewPidMessage>,
    exit_channel: Receiver<ExitMessage>,
) {
    let listener = TcpListener::bind(listen_addr).unwrap_or_else(|e| {
        panic!("Unable to create server: {e}");
    });
    // Notify the host what our kernel address is, if a listener exists.
    if let Some(las) = local_addr_sender.take() {
        las.send(listener.local_addr().unwrap()).unwrap();
    }

    let mut clients = vec![];

    fn accept_new_connection(
        mut conn: TcpStream,
        chn: &Sender<ThreadMessage>,
        new_pid_channel: &Receiver<NewPidMessage>,
        clients: &mut Vec<(std::thread::JoinHandle<()>, TcpStream)>,
    ) -> bool {
        let thr_chn = chn.clone();

        // Read the challenge access key from the client
        let mut access_key = [0u8; 16];
        conn.read_exact(&mut access_key).unwrap();
        conn.set_nodelay(true).unwrap();

        // Spawn a new process. This process will start out in the "Allocated" state.
        chn.send(ThreadMessage::NewConnection(
            conn.try_clone().expect("couldn't make a copy of the network connection for the kernel"),
            AppId(access_key),
        ))
        .expect("couldn't request a new PID");

        // The kernel will immediately respond with a new PID.
        let NewPidMessage::NewPid(new_pid) =
            new_pid_channel.recv().expect("couldn't receive message from main thread");
        let conn_copy = conn.try_clone().expect("couldn't duplicate connection");
        let jh = std::thread::Builder::new()
            .name(format!("kernel PID {} listener", new_pid))
            .spawn(move || handle_connection(conn, new_pid, thr_chn))
            .expect("couldn't spawn listen thread");
        clients.push((jh, conn_copy));
        false
    }

    fn exit_server(clients: Vec<(std::thread::JoinHandle<()>, TcpStream)>) {
        for (jh, conn) in clients {
            use std::net::Shutdown;
            conn.shutdown(Shutdown::Both).ok();
            jh.join().expect("couldn't join client thread");
        }
    }

    // Use `listener` in a nonblocking setup so that we can exit when doing tests
    enum ClientMessage {
        NewConnection(TcpStream),
        Exit,
    }
    let (sender, receiver) = unbounded();
    let tcp_sender = sender.clone();
    let exit_sender = sender;

    let (shutdown_listener, shutdown_listener_receiver) = unbounded();

    // `listener.accept()` has no way to break, so we must put it in nonblocking mode
    listener.set_nonblocking(true).unwrap();

    std::thread::Builder::new()
        .name("kernel accept thread".to_owned())
        .spawn(move || {
            loop {
                match listener.accept() {
                    Ok((conn, _addr)) => {
                        conn.set_nonblocking(false).unwrap();
                        tcp_sender.send(ClientMessage::NewConnection(conn)).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        match shutdown_listener_receiver.recv_timeout(std::time::Duration::from_millis(500)) {
                            Err(RecvTimeoutError::Timeout) => continue,
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        // Windows generates this error -- WSACancelBlockingCall -- when a
                        // connection is shut down while `accept()` is running. This should
                        // only happen when the system is shutting down, so ignore it.
                        if cfg!(windows) {
                            if let Some(10004) = e.raw_os_error() {
                                return;
                            }
                        }
                        eprintln!("error accepting connections: {e} ({e:?}) ({:?})", e.kind());
                        return;
                    }
                }
            }
        })
        .unwrap();

    // Spawn a thread to listen for the `exit` command, and relay that
    // to the main thread. This prevents us from needing to poll, since
    // all messages are coalesced into a single channel.
    std::thread::Builder::new()
        .name("kernel exit listener".to_owned())
        .spawn(move || match exit_channel.recv() {
            Ok(ExitMessage::Exit) => exit_sender.send(ClientMessage::Exit).unwrap(),
            Err(RecvError) => eprintln!("error receiving exit command"),
        })
        .unwrap();

    for msg in receiver {
        match msg {
            ClientMessage::NewConnection(conn) => {
                if accept_new_connection(conn, &chn, &new_pid_channel, &mut clients) {
                    break;
                }
            }
            ClientMessage::Exit => break,
        }
    }
    shutdown_listener.send(()).unwrap();
    exit_server(clients);
}

/// The idle function is run when there are no directly-runnable processes
/// that kmain can activate. In a hosted environment,this is the primary
/// thread that handles network communications, and this function never returns.
pub fn idle() -> bool {
    // Start listening.
    let (sender, message_receiver) = unbounded();
    let (new_pid_sender, new_pid_receiver) = unbounded();
    let (exit_sender, exit_receiver) = unbounded();

    let mut process_registry: std::collections::HashMap<
        PID,
        (app_manifest::HostedService, xous::arch::ProcessHandle),
    > = Default::default();
    // Permanent record of every spawnable process: binary_name -> service. Unlike
    // process_registry, entries here survive crashes so a crashed process can be
    // re-spawned by a hot-reload request even when it is no longer running.
    let mut process_specs: std::collections::HashMap<String, app_manifest::HostedService> =
        Default::default();
    let mut pending_reloads: std::collections::HashSet<PID> = Default::default();

    let pid1_init = ProcessInit { app_id: AppId([0u8; 16]) };
    let process_1 = SystemServices::with_mut(|ss| ss.create_process(pid1_init)).unwrap();
    assert_eq!(process_1.pid().get(), 1);
    crate::arch::process::set_current_pid(process_1.pid());

    let listen_addr = env::var("XOUS_LISTEN_ADDR")
        .map(|s| {
            s.to_socket_addrs()
                .expect("invalid server address")
                .next()
                .expect("unable to resolve server address")
        })
        .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0));

    let address_receiver = {
        let (sender, receiver) = unbounded();
        set_send_addr(sender);
        receiver
    };

    let reload_sender = sender.clone();
    let listen_thread_handle = SEND_ADDR.with(|sa| {
        let sa = sa.borrow_mut().take();
        std::thread::Builder::new()
            .name("kernel network listener".to_owned())
            .spawn(move || listen_thread(listen_addr, sender, sa, new_pid_receiver, exit_receiver))
            .expect("couldn't spawn listen thread")
    });

    let address = address_receiver.recv().unwrap();
    xous::arch::set_xous_address(address);
    println!("KERNEL: Xous server listening on {address}");
    println!("KERNEL: Starting initial processes:");

    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_else(|| "keyos-kernel".to_owned());
    let services_path = args.next().unwrap_or_else(|| {
        eprintln!("Usage: {argv0} <services.json>");
        std::process::exit(1);
    });
    let services: Vec<app_manifest::HostedService> = serde_json::from_reader(
        std::fs::File::open(&services_path)
            .unwrap_or_else(|e| panic!("couldn't open hosted-services manifest {services_path}: {e}")),
    )
    .unwrap_or_else(|e| panic!("couldn't parse hosted-services manifest {services_path}: {e}"));

    // Set the current PID to 1, which was created above. This ensures all init processes
    // are owned by PID1.
    crate::arch::process::set_current_pid(process_1.pid());

    // Spawn each service. Failures here will halt the entire system.
    println!("  PID  |  App ID  |  Command");
    println!("-------+----------+-------");
    for service in services {
        let app_id = AppId(service.app_id);
        let init = xous::ProcessInit { app_id };
        let new_process = SystemServices::with_mut(|ss| ss.create_process(init)).unwrap();
        println!(" {new_process:2} |  {app_id}  |  {0}", service.path);
        let process_args = xous::ProcessArgs::new(app_id, "program", &service.path);
        let (pid, handle) =
            xous::arch::create_process_post(process_args, init, new_process).expect("couldn't spawn");
        if service.syscalls != 0 {
            SystemServices::with_mut(|ss| {
                ss.process_mut(pid).unwrap().set_syscall_permissions(service.syscalls)
            });
        }
        if let Some(name) = std::path::Path::new(&service.path).file_name() {
            process_specs.insert(name.to_string_lossy().into_owned(), service.clone());
        }
        process_registry.insert(pid, (service, handle));
    }

    // Hot-reload socket: accepts crate names and triggers kill+relaunch
    #[cfg(unix)]
    {
        let socket_path = "/tmp/keyos-sim-reload.sock";
        let _ = std::fs::remove_file(socket_path);
        match std::os::unix::net::UnixListener::bind(socket_path) {
            Ok(listener) => {
                eprintln!("KERNEL: Hot-reload socket at {}", socket_path);
                std::thread::Builder::new()
                    .name("hot-reload listener".to_owned())
                    .spawn(move || {
                        use std::io::BufRead;
                        for stream in listener.incoming() {
                            if let Ok(stream) = stream {
                                let mut reader = std::io::BufReader::new(stream);
                                let mut line = String::new();
                                if reader.read_line(&mut line).is_ok() {
                                    let name = line.trim().to_owned();
                                    if !name.is_empty() {
                                        reload_sender.send(ThreadMessage::ReloadApp(name)).ok();
                                    }
                                }
                            }
                        }
                    })
                    .unwrap();
            }
            Err(e) => eprintln!("KERNEL: Could not bind reload socket: {}", e),
        }
    }

    while let Ok(msg) = message_receiver.recv() {
        match msg {
            ThreadMessage::NewConnection(conn, access_key) => {
                // The new process should already have a PID registered. Convert its access key
                // into a PID, and register the connection with the server.
                let new_pid = crate::arch::process::register_connection_for_key(conn, access_key).unwrap();

                // Inform the backchannel of the new process ID.
                new_pid_sender
                    .send(NewPidMessage::NewPid(new_pid))
                    .expect("couldn't send new pid to new connection");
            }
            ThreadMessage::SysCall(pid, thread_id, call) => {
                crate::arch::process::set_current_pid(pid);

                // If the call being made is to terminate the current process, we need to know
                // because we won't be able to send a response.
                let is_terminate = matches!(call, SysCall::TerminateProcess(_));
                let is_shutdown = match call {
                    #[allow(unused_variables)]
                    SysCall::Shutdown(code) => {
                        #[cfg(feature = "integration-test")]
                        std::process::exit(code);

                        #[allow(unreachable_code)]
                        true
                    }
                    _ => false,
                };

                // For a "Shutdown" command, send the response before we issue the shutdown.
                // This is because the "process" will be "terminated" (the network socket will be closed),
                // and we won't be able to send the response after we're done.
                if is_shutdown {
                    let mut process = Process::current();
                    let mut response_vec = Vec::new();
                    response_vec.extend_from_slice(&thread_id.to_le_bytes());
                    for word in Result::Ok.to_args().iter_mut() {
                        response_vec.extend_from_slice(&word.to_le_bytes());
                    }
                    process.send(&response_vec).unwrap_or_else(|e| {
                        // If we're unable to send data to the process, assume it's dead and terminate it.
                        println!("Unable to send response to process: {e:?} -- terminating");
                        crate::syscall::handle(thread_id, SysCall::TerminateProcess(0)).ok();
                    });
                }

                {
                    let current_process = crate::arch::process::Process::current();
                    if current_process.thread_exists(thread_id) {
                        crate::arch::process::Process::current().set_tid(thread_id).unwrap();
                    }
                }

                // Handle the syscall within the Xous kernel
                let response = crate::syscall::handle(
                    // If this is a fake thread ID for the injected CreateThread call from xous-rs,
                    // pretend it came from the main thread.
                    if thread_id < 0xffff { thread_id } else { crate::process::INITIAL_TID },
                    call,
                )
                .unwrap_or_else(Result::Error);

                // Send the response back to the target.
                if response != Result::ResumeProcess && !is_terminate && !is_shutdown {
                    Process::current().set_thread_result(thread_id, response);
                }

                if is_shutdown {
                    exit_sender.send(ExitMessage::Exit).expect("couldn't send shutdown signal");
                    break;
                }

                // Clean up the registry entry; re-spawn on hot-reload, or take the whole
                // system down if a system service exited.
                if is_terminate {
                    if let Some((service, _)) = process_registry.remove(&pid) {
                        let app_id = AppId(service.app_id);
                        if pending_reloads.remove(&pid) {
                            let init = xous::ProcessInit { app_id };
                            match SystemServices::with_mut(|ss| ss.create_process(init)) {
                                Ok(new_process) => {
                                    let new_args = xous::ProcessArgs::new(app_id, "program", &service.path);
                                    match xous::arch::create_process_post(new_args, init, new_process) {
                                        Ok((new_pid, new_handle)) => {
                                            eprintln!(
                                                "KERNEL: Hot-reloaded {} as PID {}",
                                                service.path, new_pid
                                            );
                                            process_registry.insert(new_pid, (service, new_handle));
                                        }
                                        Err(e) => {
                                            eprintln!("KERNEL: Failed to re-spawn process: {:?}", e)
                                        }
                                    }
                                }
                                Err(e) => eprintln!("KERNEL: Failed to allocate process slot: {:?}", e),
                            }
                        } else if service.system {
                            eprintln!(
                                "KERNEL: system service PID {pid} ({}) exited -- shutting down",
                                service.path
                            );
                            exit_sender.send(ExitMessage::Exit).expect("couldn't send shutdown signal");
                            break;
                        }
                    }
                }
            }
            ThreadMessage::ReloadApp(crate_name) => {
                // A system service exit is meant to take the whole sim down, so it can't
                // be hot-reloaded in place.
                if process_specs.get(&crate_name).is_some_and(|s| s.system) {
                    eprintln!("KERNEL: refusing to hot-reload system service '{}'", crate_name);
                    continue;
                }
                let entry = process_registry.iter_mut().find(|(_, (service, _))| {
                    std::path::Path::new(&service.path)
                        .file_name()
                        .map_or(false, |n| n == crate_name.as_str())
                });
                if let Some((&pid, (_, handle))) = entry {
                    eprintln!(
                        "KERNEL: Hot-reload requested for '{}' (PID {}), sending SIGKILL",
                        crate_name, pid
                    );
                    pending_reloads.insert(pid);
                    handle.kill().ok();
                } else if let Some(service) = process_specs.get(&crate_name) {
                    // Process is not running (crashed). Spawn it directly.
                    let service = service.clone();
                    let app_id = AppId(service.app_id);
                    eprintln!("KERNEL: '{}' not running (crashed?), re-spawning directly", crate_name);
                    let init = xous::ProcessInit { app_id };
                    match SystemServices::with_mut(|ss| ss.create_process(init)) {
                        Ok(new_process) => {
                            let new_args = xous::ProcessArgs::new(app_id, "program", &service.path);
                            match xous::arch::create_process_post(new_args, init, new_process) {
                                Ok((new_pid, new_handle)) => {
                                    eprintln!("KERNEL: Re-spawned {} as PID {}", service.path, new_pid);
                                    process_registry.insert(new_pid, (service, new_handle));
                                }
                                Err(e) => eprintln!("KERNEL: Failed to re-spawn process: {:?}", e),
                            }
                        }
                        Err(e) => eprintln!("KERNEL: Failed to allocate process slot: {:?}", e),
                    }
                } else {
                    eprintln!("KERNEL: No spec found for '{}', was it started by the kernel?", crate_name);
                }
            }
        }
    }

    listen_thread_handle.join().expect("error waiting for listen thread to return");

    false
}
