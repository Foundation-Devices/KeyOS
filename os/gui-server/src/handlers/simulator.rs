// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::msg::{GetDeviceFrame, SetScaleFactor, SimulateKey, SimulatePowerButton, SimulateScroll};
use rgb_led::RgbColor;
use server::{LendMutHandler, ScalarEventHandler, ScalarHandler, ServerContext};
use xous::PID;

use crate::display::PlatformDisplay;
use crate::{get_frame, Gui};

impl ScalarHandler<SetScaleFactor> for Gui {
    fn handle(&mut self, msg: SetScaleFactor, _sender: PID, _context: &mut ServerContext<Self>) {
        PlatformDisplay::set_scale_factor(msg.0)
    }
}

impl LendMutHandler<GetDeviceFrame> for Gui {
    fn handle(
        &mut self,
        GetDeviceFrame(mut mem): GetDeviceFrame,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        get_frame(true, &mut mem)
    }
}

impl ScalarHandler<SimulatePowerButton> for Gui {
    fn handle(
        &mut self,
        SimulatePowerButton(is_pressed): SimulatePowerButton,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.handle_power_button(is_pressed);
    }
}

impl ScalarHandler<SimulateKey> for Gui {
    fn handle(
        &mut self,
        SimulateKey { key, is_pressed }: SimulateKey,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.dispatch_key_event(is_pressed, key);
    }
}

impl ScalarHandler<SimulateScroll> for Gui {
    fn handle(
        &mut self,
        SimulateScroll { x, y, delta_x, delta_y }: SimulateScroll,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.dispatch_scroll_event(x, y, delta_x, delta_y);
    }
}

impl ScalarEventHandler<RgbColor> for Gui {
    fn handle(&mut self, color: RgbColor, _sender: PID, _context: &mut ServerContext<Self>) {
        PlatformDisplay::set_rgb_led_color(color.into());
    }
}
