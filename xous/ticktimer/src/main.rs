use std::collections::BTreeMap;

use log::{error, info};
use xous_api_ticktimer::*;

mod platform;
use platform::XousTickTimer;

// Any sleeping below 100 us is considered zero.
const MIN_SLEEP_NS: u64 = 100_000;

// 10s watchdog reset period
#[cfg(keyos)]
const WDT_RESET_PERIOD_MS: usize = 10 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimerRequest {
    Timeout { sender: xous::MessageSender, condvar: Option<usize> },
    Callback { cid: xous::CID, message_id: usize, message_data: usize },
}

impl TimerRequest {
    fn respond(self, timed_out: bool) {
        match self {
            TimerRequest::Timeout { sender, .. } => {
                xous::return_scalar(sender, if timed_out { 1 } else { 0 }).ok();
            }
            TimerRequest::Callback { cid, message_id, message_data } => {
                // This will fail if the process has exited, and also if its message queue is full.
                // In the latter case we will lose callbacks, which is potentially bad, but the
                // alternative is locking up the ticktimer (and potentially the whole system), which
                // is way worse.
                if let Err(e) = xous::try_send_message(
                    cid,
                    xous::Message::Scalar(xous::ScalarMessage {
                        id: message_id,
                        arg1: message_data,
                        arg2: 0,
                        arg3: 0,
                        arg4: 0,
                    }),
                ) {
                    if let Ok(pid) = xous::get_remote_pid(cid) {
                        log::error!(
                            "Error sending message {message_id}/{message_data} to {cid} (PID {pid}): {e:?}"
                        );
                    } else {
                        log::error!("Error sending message {message_id}/{message_data} to {cid}: {e:?}");
                    }
                }
            }
        }
    }
}

/// Recalculate the sleep timer, optionally adding an element to the heap.
/// All expired elements are responded to.
/// The sleep timer is started if there is somethign to wait on.
pub fn recalculate_sleep(
    xtt: &XousTickTimer,
    sleep_heap: &mut BTreeMap<u64, TimerRequest>, // min-heap with Reverse
    new: Option<(u64, TimerRequest)>,
) {
    let uptime = xtt.elapsed_ns();
    // If we have a new sleep request, add it to the heap.
    if let Some((timeout, request)) = new {
        // Ensure that each timeout only exists once inside the tree
        let mut target_uptime = uptime + timeout;
        while sleep_heap.contains_key(&target_uptime) {
            target_uptime += 1;
        }

        sleep_heap.insert(target_uptime, request);
    }
    while let Some((target_uptime, _request)) = sleep_heap.first_key_value() {
        if *target_uptime < uptime + MIN_SLEEP_NS {
            let (_, request) = sleep_heap.pop_first().unwrap();
            request.respond(true);
        } else {
            xtt.start_sleep(target_uptime - uptime);
            return;
        }
    }
}

fn main() -> ! {
    log_server::init_wait_noticks(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::Highest).unwrap();

    let ticktimer_server =
        xous::create_server_with_sid(xous::SID::from_bytes(b"ticktimer-server").unwrap(), 0..16)
            .expect("Couldn't create Ticktimer server");

    info!("Server started with SID {:?}", ticktimer_server);

    // Connect to our own server so we can send the "Recalculate" message
    let ticktimer_client =
        xous::connect(xous::SID::from_bytes(b"ticktimer-server").unwrap()).expect("couldn't connect to self");

    xous::register_system_event_handler(
        xous::SystemEvent::Disconnected,
        ticktimer_server,
        api::Opcode::Disconnected as _,
    )
    .expect("couldn't subscribe to disconnect event");
    // Create a new ticktimer object
    #[cfg(keyos)]
    let ticktimer = XousTickTimer::new(ticktimer_client);
    #[cfg(not(keyos))]
    let ticktimer = XousTickTimer::new(ticktimer_client);
    #[cfg(not(keyos))]
    let ticktimer = &ticktimer;

    // A list of all sleep requests in the system, sorted by the uptime at which it
    // expires, in nanoseconds.
    let mut sleep_heap: BTreeMap<u64, TimerRequest> = BTreeMap::new();

    #[cfg(keyos)]
    let wdt_callback = TicktimerCallback::new(ticktimer_server).expect("couldn't create callback");
    #[cfg(keyos)]
    wdt_callback.request(WDT_RESET_PERIOD_MS, api::Opcode::WatchdogReset as _, 0);

    loop {
        let msg = xous::receive_message(ticktimer_server).unwrap();
        let Some(scalar) = msg.body.scalar_message() else { continue };
        let blocking = msg.body.is_blocking();
        match num_traits::FromPrimitive::from_usize(msg.body.id()).unwrap_or(api::Opcode::InvalidCall) {
            api::Opcode::Elapsed => {
                if !blocking {
                    log::warn!("ElapsedMs request was not blocking");
                    continue;
                }
                let time = ticktimer.elapsed_ns();
                xous::return_scalar2(msg.sender, (time & 0xFFFF_FFFF) as usize, (time >> 32) as usize).ok();
            }

            api::Opcode::Sleep => {
                if !blocking {
                    log::warn!("SleepMs request was not blocking");
                    continue;
                }
                let timeout = scalar.arg1 as u64 | ((scalar.arg2 as u64) << 32);
                recalculate_sleep(
                    ticktimer,
                    &mut sleep_heap,
                    Some((timeout, TimerRequest::Timeout { sender: msg.sender, condvar: None })),
                )
            }

            api::Opcode::WaitForCondition => {
                if !blocking {
                    log::warn!("WaitForCondition request was not blocking");
                    continue;
                };
                let condvar = scalar.arg1;
                let timeout = scalar.arg2 as u64 | ((scalar.arg3 as u64) << 32);

                recalculate_sleep(
                    ticktimer,
                    &mut sleep_heap,
                    Some((timeout, TimerRequest::Timeout { sender: msg.sender, condvar: Some(condvar) })),
                )
            }

            api::Opcode::NotifyCondition => {
                if !blocking {
                    log::warn!("NotifyCondition request was not blocking");
                    continue;
                };
                let pid = msg.sender.pid();

                let condvar = scalar.arg1;
                let requested_count = scalar.arg2;
                let mut notified = 0;
                for _ in 0..requested_count {
                    if let Some((key, _entry)) = sleep_heap.iter().find(|(_uptime, entry)| {
                        matches!(entry, TimerRequest::Timeout { sender, condvar: Some(c) } if sender.pid() == pid && condvar == *c)
                    }) {
                        let key = *key;
                        let entry = sleep_heap.remove(&key).unwrap();
                        entry.respond(false);
                        notified += 1;
                    } else {
                        break;
                    }
                }
                xous::return_scalar(msg.sender, notified).ok();
                recalculate_sleep(ticktimer, &mut sleep_heap, None);
            }

            api::Opcode::GetSystemTime => {
                if !blocking {
                    log::warn!("GetSystemTime request was not blocking");
                    continue;
                };
                let time_ns = ticktimer.get_system_time_ns();
                xous::return_scalar2(msg.sender, (time_ns & 0xFFFFFFFF) as usize, (time_ns >> 32) as usize)
                    .ok();
            }

            api::Opcode::SetSystemTime => {
                if !blocking {
                    log::warn!("SetSystemTime request was not blocking");
                    continue;
                };
                let time_nanos = scalar.arg1 as u64 | ((scalar.arg2 as u64) << 32);
                ticktimer.set_system_time_ns(time_nanos);
                xous::return_scalar(msg.sender, 0).ok();
            }

            api::Opcode::RequestCallback => {
                let msec = scalar.arg1 as u64;
                let nsec = msec * 1000_000;
                let message_id = scalar.arg2;
                let message_data = scalar.arg3;
                let cid = scalar.arg4 as xous::CID;

                if xous::get_remote_pid(cid) != msg.sender.pid().ok_or(xous::Error::UnknownError) {
                    log::warn!("CID did not belong to the sender");
                    if blocking {
                        xous::return_scalar(msg.sender, 1).ok();
                    }
                    continue;
                }

                sleep_heap.retain(|_, v| {
                    !matches!(v, TimerRequest::Callback { message_id: m, cid: c, .. } if *m == message_id && *c == cid)
                });
                recalculate_sleep(
                    ticktimer,
                    &mut sleep_heap,
                    if nsec > 0 {
                        Some((nsec, TimerRequest::Callback { message_id, message_data, cid }))
                    } else {
                        None
                    },
                );

                if blocking {
                    xous::return_scalar(msg.sender, 0).ok();
                }
            }

            api::Opcode::Disconnected => {
                let disconnected_cid = scalar.arg1 as _;
                sleep_heap.retain(
                    |_, v| !matches!(v, TimerRequest::Callback { cid, .. } if disconnected_cid == *cid),
                );
                recalculate_sleep(ticktimer, &mut sleep_heap, None);

                // Disconnect as many times as needed, since there could have been multiple connect calls,
                // increasing the refcount.
                while xous::disconnect(disconnected_cid).is_ok() {}
            }

            api::Opcode::TimerInterrupt => {
                if blocking {
                    log::warn!("RecalculateSleep was blocking");
                    xous::return_scalar(msg.sender, 0).ok();
                    continue;
                }
                recalculate_sleep(ticktimer, &mut sleep_heap, None);
            }

            #[cfg(keyos)]
            api::Opcode::WatchdogReset => {
                ticktimer.restart_wdt();

                wdt_callback.request(WDT_RESET_PERIOD_MS, api::Opcode::WatchdogReset as _, 0);
            }

            api::Opcode::InvalidCall => {
                error!("couldn't convert opcode");
                if blocking {
                    xous::return_scalar(msg.sender, 0).ok();
                }
            }
        }
    }
}
