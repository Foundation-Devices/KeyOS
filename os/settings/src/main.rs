// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod store;
mod sys;

use std::str::FromStr;

use server::{MessageId as _, ServerContext};
use settings::messages::*;
use store::Store;
use xous_ticktimer::TicktimerCallback;

use crate::sys::GlobalSubscriptions;

const FLUSH_INTERVAL_MS: usize = 2000;

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    let log_level = option_env!("RUST_LOG")
        .and_then(|l| log::LevelFilter::from_str(&l).ok())
        .unwrap_or(log::LevelFilter::Info);
    log::set_max_level(log_level);

    xous::set_thread_priority(xous::ThreadPriority::System4).unwrap();

    server::listen_with(Server::new);
}

impl server::Server for Server {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.flush_dirty_files(false);
        self.store.fs.subscribe_filesystem_events(context, fs::Location::AppData);
    }
}

#[derive(server::Server)]
#[name = "os/settings"]
pub(crate) struct Server {
    pub(crate) subscriptions: GlobalSubscriptions,
    pub(crate) store: Store,
    callback: TicktimerCallback,
}

impl Server {
    fn new(sid: xous::SID) -> Self {
        let callback = TicktimerCallback::new(sid).unwrap();
        xous::register_system_event_handler(xous::SystemEvent::Disconnected, sid, SubscriberDisconnected::ID)
            .unwrap();
        Self { store: Store::default(), subscriptions: GlobalSubscriptions::default(), callback }
    }

    /// flushes all dirty files + schedules a tick timer callback
    pub(crate) fn flush_dirty_files(&mut self, force: bool) {
        log::debug!("flushing dirty files force={force}");
        self.store.flush_dirty_files(force);
        self.callback.request(FLUSH_INTERVAL_MS, <FlushAll as server::MessageId>::ID, 0);
    }
}

impl server::BlockingScalarHandler<ResetSettings> for Server {
    fn handle(
        &mut self,
        _msg: ResetSettings,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <ResetSettings as server::BlockingScalar>::Response {
        *self.store.get_system() = Default::default();
        if let Some(mut encrypted) = self.store.get_encrypted() {
            *encrypted = Default::default();
        }
        self.store.flush_dirty_files(true);
    }
}

impl server::ScalarHandler<FlushAll> for Server {
    fn handle(&mut self, msg: FlushAll, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.flush_dirty_files(msg.force);
    }
}

impl server::ScalarHandler<SubscriberDisconnected> for Server {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.subscriptions.remove_cid(msg.0);
    }
}

impl server::BlockingScalarHandler<GetPrimeColor> for Server {
    fn handle(
        &mut self,
        _msg: GetPrimeColor,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetPrimeColor as server::BlockingScalar>::Response {
        crate::sys::load_prime_color()
    }
}

impl server::BlockingScalarHandler<GetBoardRevision> for Server {
    fn handle(
        &mut self,
        _msg: GetBoardRevision,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetBoardRevision as server::BlockingScalar>::Response {
        crate::sys::load_board_revision()
    }
}

impl server::ScalarEventHandler<fs::FileSystemEvent> for Server {
    fn handle(
        &mut self,
        msg: fs::FileSystemEvent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if msg.location == fs::Location::AppData && msg.event_type == fs::FileSystemEventType::Mounted {
            self.store
                .try_mount_encrypted()
                .inspect_err(|e| log::error!("failed to mount encrypted settings {e:?}"))
                .ok();

            // Developer Mode must not survive a power cycle on production firmware: a shipped unit
            // that was once unlocked with it on would otherwise keep the usb-debug interface
            // reachable. Clear it as the encrypted store mounts, before subscribers are notified.
            #[cfg(feature = "production")]
            if let Some(mut encrypted) = self.store.get_encrypted() {
                if encrypted.developer_mode.0 {
                    encrypted.developer_mode = settings::global::DeveloperMode(false);
                }
            }

            if let Some(settings) = self.store.get_encrypted() {
                self.subscriptions.notify_encrypted_subscribers(&settings);
            }
        }
    }
}

impl server::BlockingArchiveHandler<LookupTimeZone> for Server {
    fn handle(
        &mut self,
        LookupTimeZone { name, offset_minutes }: LookupTimeZone,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <LookupTimeZone as server::BlockingArchive>::Response {
        if let Some((name, data)) = jiff_tzdb::get(&name) {
            return settings::global::TimeZone { name: name.into(), data: data.to_vec() };
        }
        let now = jiff::Timestamp::now();
        nearest_by_offset_minutes(now, offset_minutes)
    }
}

impl server::BlockingArchiveHandler<ListTimeZone> for Server {
    fn handle(
        &mut self,
        ListTimeZone { offset, count }: ListTimeZone,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <ListTimeZone as server::BlockingArchive>::Response {
        jiff_tzdb::available()
            .skip(offset.unwrap_or(0) as usize)
            .take(count.map(|c| c as usize).unwrap_or_else(|| jiff_tzdb::available().count()))
            .map(|name| jiff_tzdb::get(name).unwrap())
            .map(|(name, data)| settings::global::TimeZone { name: name.into(), data: data.to_vec() })
            .collect()
    }
}

fn nearest_by_offset_minutes(now: jiff::Timestamp, offset_minutes: i32) -> settings::global::TimeZone {
    let mut closest_tz: Option<(&'static str, &'static [u8])> = None;
    let mut smallest_diff = i32::MAX;

    for name in jiff_tzdb::available() {
        let (name, data) = jiff_tzdb::get(name).unwrap();
        let tz_jiff = jiff::tz::TimeZone::tzif(name, data).unwrap();
        let info = tz_jiff.to_offset_info(now);
        let tz_offset_seconds = info.offset().seconds();
        let tz_offset_minutes = tz_offset_seconds / 60;
        let diff = (tz_offset_minutes - offset_minutes).abs();

        if diff < smallest_diff {
            smallest_diff = diff;
            closest_tz = Some((name, data));
        }
    }

    let (name, data) = closest_tz.unwrap();
    settings::global::TimeZone { name: name.into(), data: data.to_vec() }
}

#[derive(Debug, server::Message)]
pub struct SubscriberDisconnected(pub xous::CID);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn default_timezone() {
        let (_name, data) = jiff_tzdb::get("America/New_York").unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("api/settings/src/america_new_york.tzif");
        std::fs::write(&path, &data).unwrap();
        println!("Wrote {} bytes to {}", data.len(), path.display());
    }

    #[test]
    fn tz_nearest_by_offset_minutes() {
        // UTC-7
        let offset = -420;

        // Summer timestamp: 2024-07-03
        // In summer, America/Boise uses UTC-6 (MDT), so America/Creston (always UTC-7) comes first
        let summer_ts = jiff::Timestamp::constant(1720000000, 0);
        let summer_result = nearest_by_offset_minutes(summer_ts, offset);
        assert_eq!(&summer_result.name, "America/Creston");

        // Winter timestamp: 2023-12-15
        // In winter, America/Boise uses UTC-7 (MST), and comes before Creston alphabetically
        let winter_ts = jiff::Timestamp::constant(1702598400, 0);
        let winter_result = nearest_by_offset_minutes(winter_ts, offset);
        assert_eq!(&winter_result.name, "America/Boise");

        assert_eq!(summer_result.timezone().to_offset_info(summer_ts).offset().seconds() / 60, offset);
        assert_eq!(winter_result.timezone().to_offset_info(winter_ts).offset().seconds() / 60, offset);
    }
}
