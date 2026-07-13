// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{ScalarHandler, ServerContext};
use xous::PID;

use super::PermissionRequest;
use crate::Gui;

impl ScalarHandler<PermissionRequest> for Gui {
    fn handle(&mut self, request: PermissionRequest, _sender: PID, _context: &mut ServerContext<Self>) {
        // Resolve it now if the grant state already decides it; otherwise it is queued and
        // prompted one at a time through the shared modal state machine.
        self.queue_permission_request(request.request_id);
    }
}
