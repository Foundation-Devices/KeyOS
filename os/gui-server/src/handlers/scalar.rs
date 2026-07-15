// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::{
    AnimateNextFrame, CloseApp, IsLocked, RequestRedraw, SetControlCenterColor, Shutdown, SwitchTo,
    SwitchToLauncher, UpdateKioskPolicy,
};
use log::{error, info, warn};
use server::{BlockingScalar, BlockingScalarHandler, ScalarHandler, ServerContext};
use xous::PID;

use super::{BlurDone, CloseAppTimeout, ForceShutdownTimeout, OnFreeMemoryBelowThreshold};
use crate::registry::AppRole;
use crate::{
    handlers::{DisconnectHandlerMessage, OnVsyncMessage, PowerButtonTimerCallback},
    Gui,
};

impl ScalarHandler<SwitchTo> for Gui {
    fn handle(
        &mut self,
        SwitchTo { next_pid, x, y }: SwitchTo,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        if let Some(pid) = PID::new(next_pid as u8) {
            // TODO
            let _ = x;
            let _ = y;

            #[cfg(not(feature = "recovery-os"))]
            if self.is_locked() {
                // An app is opened when the screen is locked, postpone switching to it to after an unlock.
                if self.startup_state == crate::StartupState::Started {
                    self.app_registry.set_pre_lock_app_pid(Some(pid));
                }
                return;
            }

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
        if self.app_registry.pid(AppRole::Launcher).is_some() {
            self.switch_to_launcher();
            return true;
        }

        false
    }
}

impl BlockingScalarHandler<IsLocked> for Gui {
    fn handle(
        &mut self,
        _msg: IsLocked,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <IsLocked as BlockingScalar>::Response {
        self.is_locked()
    }
}

impl ScalarHandler<UpdateKioskPolicy> for Gui {
    fn handle(&mut self, update: UpdateKioskPolicy, sender: PID, _context: &mut ServerContext<Self>) {
        info!("Kiosk policy set to {update:?} by PID={sender}");
        let Some(window) = self.windows.get_mut(&sender) else {
            warn!("Ignoring kiosk policy update from unknown PID={sender}");
            return;
        };

        if let Some(home_button_enabled) = update.home_button_enabled {
            window.kiosk_policy.home_button_enabled = home_button_enabled;
        }
        if let Some(power_button_enabled) = update.power_button_enabled {
            window.kiosk_policy.power_button_enabled = power_button_enabled;
        }
        if let Some(control_center_enabled) = update.control_center_enabled {
            window.kiosk_policy.control_center_enabled = control_center_enabled;
        }
        if let Some(auto_lock_enabled) = update.auto_lock_enabled {
            window.kiosk_policy.auto_lock_enabled = auto_lock_enabled;
        }

        if update.auto_lock_enabled.is_some() {
            self.reset_auto_lock();
        }
    }
}

impl ScalarHandler<RequestRedraw> for Gui {
    fn handle(&mut self, _msg: RequestRedraw, sender: PID, _context: &mut ServerContext<Self>) {
        self.update_vsync_states();

        if let Some(window) = &mut self.control_center_window
            && window.pid == sender
        {
            window.buffers.buffer_requested(self.last_vsync_time);
        } else if let Some(window) = &mut self.keyboard_window
            && window.pid == sender
        {
            window.buffers.buffer_requested(self.last_vsync_time);
        } else if let Some(window) = self.windows.get_mut(&sender) {
            window.buffers.buffer_requested(self.last_vsync_time);
        } else {
            log::error!("RequestRedraw from unknown PID={sender}");
        }
    }
}

impl ScalarHandler<SetControlCenterColor> for Gui {
    fn handle(
        &mut self,
        SetControlCenterColor { color }: SetControlCenterColor,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Some(window) = self.windows.get_mut(&sender) else {
            warn!("Ignoring Control Center color update from unknown PID={sender}");
            return;
        };

        window.control_center_color = color;
        self.notify_control_center_color();
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

impl BlockingScalarHandler<CloseApp> for Gui {
    fn handle(
        &mut self,
        CloseApp { pid }: CloseApp,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <CloseApp as BlockingScalar>::Response {
        if let Some(pid) = u8::try_from(pid).ok().and_then(PID::new) {
            self.close_app(pid)
        } else {
            error!("Invalid PID={pid}");
            Err(gui_server_api::GuiServerError::AppNotFound)
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
