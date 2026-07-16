// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::{
    msg::{
        AnimateNextFrame, CloseApp, RequestRedraw, Shutdown, SwapBuffers, SwitchTo, SwitchToLauncher,
        UpdateKioskPolicy,
    },
    InputMessage,
};
use log::{error, info, warn};
use server::{
    BlockingScalar, BlockingScalarAsyncHandler, BlockingScalarHandler, BlockingScalarRequest, ScalarHandler,
    ServerContext,
};
use xous::PID;

use super::{BlurDone, CloseAppTimeout, ForceShutdownTimeout, OnFreeMemoryBelowThreshold};
use crate::{
    handlers::{DisconnectHandlerMessage, OnVsyncMessage, PowerButtonTimerCallback},
    Gui, KioskPolicy,
};

impl BlockingScalarAsyncHandler<SwapBuffers> for Gui {
    fn handle(&mut self, msg: BlockingScalarRequest<SwapBuffers>, _context: &mut ServerContext<Self>) {
        self.handle_update_buffers(msg);
    }

    fn default_response() -> <SwapBuffers as BlockingScalar>::Response { None }
}

impl ScalarHandler<SwitchTo> for Gui {
    fn handle(
        &mut self,
        SwitchTo { next_pid, x, y }: SwitchTo,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        if let Some(pid) = PID::new(next_pid as u8) {
            let called_from_launcher = self.app_registry.is_launcher_app(sender);
            let called_from_switcher = self.app_registry.is_switcher_app(sender);
            if !called_from_launcher && !called_from_switcher {
                warn!(
                    "PID {} tried to call SwitchTo while not being registered as a launcher or switcher app (PIDs {:?} and {:?} respectively)",
                    sender, self.app_registry.launcher_app_pid(), self.app_registry.switcher_app_pid()
                );
                return;
            }
            // TODO
            let _ = x;
            let _ = y;

            self.switch_to_window(pid);
        } else {
            error!("Invalid PID={next_pid}");
        }
    }
}

impl BlockingScalarHandler<SwitchToLauncher> for Gui {
    fn handle(
        &mut self,
        _msg: SwitchToLauncher,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <SwitchToLauncher as BlockingScalar>::Response {
        if self.app_registry.launcher_app_pid().is_some() {
            self.switch_to_launcher();
            return true;
        }

        false
    }
}

impl ScalarHandler<UpdateKioskPolicy> for Gui {
    fn handle(&mut self, update: UpdateKioskPolicy, sender: PID, _context: &mut ServerContext<Self>) {
        fn update_kiosk_policy(policy: &mut KioskPolicy, update: UpdateKioskPolicy) {
            if let Some(auto_lock_enabled) = update.auto_lock_enabled {
                policy.auto_lock_enabled = auto_lock_enabled;
            }
            if let Some(control_center_enabled) = update.control_center_enabled {
                policy.control_center_enabled = control_center_enabled;
            }
            if let Some(home_button_enabled) = update.home_button_enabled {
                policy.home_button_enabled = home_button_enabled;
            }
            if let Some(power_button_enabled) = update.power_button_enabled {
                policy.power_button_enabled = power_button_enabled;
            }
        }
        use std::collections::hash_map::Entry;

        info!("Kiosk policy set to {update:?} by PID={sender}");
        match self.kiosk_policies.entry(sender) {
            Entry::Occupied(mut entry) => {
                update_kiosk_policy(entry.get_mut(), update);
                if *entry.get() == KioskPolicy::default() {
                    entry.remove();
                }
            }
            Entry::Vacant(entry) => {
                let mut policy = KioskPolicy::default();
                update_kiosk_policy(&mut policy, update);
                if policy != KioskPolicy::default() {
                    entry.insert(policy);
                }
            }
        }

        if update.auto_lock_enabled.is_some() {
            self.reset_auto_lock();
        }
    }
}

impl ScalarHandler<RequestRedraw> for Gui {
    fn handle(&mut self, _msg: RequestRedraw, sender: PID, _context: &mut ServerContext<Self>) {
        let app_input_cid = if Some(sender) == self.control_center_window.as_ref().map(|w| w.pid) {
            self.control_center_window.as_ref().map(|w| w.input_cid)
        } else if Some(sender) == self.keyboard_window.as_ref().map(|w| w.pid) {
            self.keyboard_window.as_ref().map(|w| w.input_cid)
        } else {
            self.windows.get_mut(&sender).map(|w| w.input_cid)
        };

        if let Some(cid) = app_input_cid {
            let msg = xous::Message::new_scalar(InputMessage::RedrawRequested as usize, 0, 0, 0, 0);
            if let Err(e) = xous::send_message(cid, msg) {
                error!("Failed to send the input event to the app PID={sender}: {e:?}");
            }
        } else {
            warn!("Redraw requested by an unknown app with PID={sender}");
        }
    }
}

impl BlockingScalarHandler<Shutdown> for Gui {
    fn handle(
        &mut self,
        msg: Shutdown,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <Shutdown as BlockingScalar>::Response {
        info!("Shutdown requested by PID={sender}");
        self.shutdown(msg.reboot);
    }
}

impl ScalarHandler<CloseApp> for Gui {
    fn handle(&mut self, CloseApp { pid }: CloseApp, _sender: PID, _context: &mut ServerContext<Self>) {
        if let Some(pid) = PID::new(pid as u8) {
            self.close_app(pid);
        } else {
            error!("Invalid PID={pid}");
        }
    }
}

impl ScalarHandler<AnimateNextFrame> for Gui {
    fn handle(
        &mut self,
        AnimateNextFrame { animation_kind }: AnimateNextFrame,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.handle_animate_next_frame(sender, animation_kind);
    }
}

impl ScalarHandler<PowerButtonTimerCallback> for Gui {
    fn handle(&mut self, _msg: PowerButtonTimerCallback, _sender: PID, _context: &mut ServerContext<Self>) {
        self.handle_power_button_callback();
    }
}

impl ScalarHandler<DisconnectHandlerMessage> for Gui {
    fn handle(&mut self, _msg: DisconnectHandlerMessage, sender: PID, _context: &mut ServerContext<Self>) {
        self.on_app_disconnected(sender);
    }
}

impl ScalarHandler<OnVsyncMessage> for Gui {
    fn handle(&mut self, _msg: OnVsyncMessage, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.on_vsync();
    }
}

impl ScalarHandler<OnFreeMemoryBelowThreshold> for Gui {
    fn handle(
        &mut self,
        _msg: OnFreeMemoryBelowThreshold,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        log::warn!("Free system memory below threshold");
        self.close_least_recently_used_app();
    }
}

impl ScalarHandler<CloseAppTimeout> for Gui {
    fn handle(&mut self, _msg: CloseAppTimeout, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        log::debug!("Got CloseAppTimeout");
        self.close_app_timed_out();
    }
}

impl ScalarHandler<ForceShutdownTimeout> for Gui {
    fn handle(&mut self, _msg: ForceShutdownTimeout, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        log::debug!("Got ForceShutdownTimeout");
        self.force_shutdown_timeout();
    }
}

impl ScalarHandler<BlurDone> for Gui {
    fn handle(&mut self, msg: BlurDone, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.handle_blur_done(msg);
    }
}
