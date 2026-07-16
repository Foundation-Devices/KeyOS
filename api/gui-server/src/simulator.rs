// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{consts, msg, GuiServerError};

#[derive(Default)]
pub struct SimulatorApi<P: server::CheckedPermissions>(server::CheckedConn<P>);

impl<P: server::CheckedPermissions> SimulatorApi<P> {
    /// Captures the full device frame (including bezels) as raw ARGB8888.
    /// For screen-only capture, use `GuiApiLight::capture_screen()` instead.
    pub fn device_frame(&self) -> Result<Vec<u8>, GuiServerError>
    where
        P: server::MessageAllowed<msg::GetDeviceFrame>,
    {
        let mem = xous::map_memory(
            None,
            None,
            consts::DEVICE_WIDTH as usize * consts::DEVICE_HEIGHT as usize * 4,
            xous::MemoryFlags::W,
        )?;
        self.0.lend_mut(msg::GetDeviceFrame(mem));

        let vec = mem.as_slice().to_vec();
        xous::unmap_memory(mem)?;

        Ok(vec)
    }

    pub fn set_scale_factor(&self, scale_factor: f32) -> Result<(), GuiServerError>
    where
        P: server::MessageAllowed<msg::SetScaleFactor>,
    {
        self.0.try_send_scalar(msg::SetScaleFactor((scale_factor * 256.0) as usize))?;
        Ok(())
    }

    pub fn simulate_scroll(&self, x: u32, y: u32, delta_x: f32, delta_y: f32) -> Result<(), GuiServerError>
    where
        P: server::MessageAllowed<msg::SimulateScroll>,
    {
        self.0.try_send_scalar(msg::SimulateScroll { x, y, delta_x, delta_y })?;
        Ok(())
    }

    pub fn simulate_key(&self, key: crate::Key, is_pressed: bool) -> Result<(), GuiServerError>
    where
        P: server::MessageAllowed<msg::SimulateKey>,
    {
        self.0.try_send_scalar(msg::SimulateKey { key, is_pressed })?;
        Ok(())
    }

    pub fn simulate_power_button(&self, is_pressed: bool) -> Result<(), GuiServerError>
    where
        P: server::MessageAllowed<msg::SimulatePowerButton>,
    {
        self.0.try_send_scalar(msg::SimulatePowerButton(is_pressed))?;

        Ok(())
    }
}
