// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::SubscribeFilesystemEvent;
use server::{xous, ScalarEventSubscriptionHandler};

use crate::{FileSystemEvent, Server, SubscriberDisconnected};

impl ScalarEventSubscriptionHandler<SubscribeFilesystemEvent> for Server {
    fn handle(
        &mut self,
        msg: SubscribeFilesystemEvent,
        subscriber: server::ScalarEventSubscriber<FileSystemEvent>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        if let Some(event) = self.last_fs_event.get(&msg.0) {
            subscriber.send(&FileSystemEvent { location: msg.0, event_type: *event }).ok();
        }
        self.fs_event_subscribers.entry(msg.0).or_default().push(subscriber);
        Ok(())
    }
}

impl server::ScalarHandler<SubscriberDisconnected> for Server {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        for subscribers in self.fs_event_subscribers.values_mut() {
            subscribers.retain(|s| s.cid() != msg.0);
        }
    }
}

impl Server {
    pub(crate) fn send_filesystem_event(&mut self, event: FileSystemEvent) {
        self.last_fs_event.insert(event.location, event.event_type);
        if let Some(subscribers) = self.fs_event_subscribers.get_mut(&event.location) {
            subscribers.retain(|s| s.send(&event).is_ok());
        }
    }
}
