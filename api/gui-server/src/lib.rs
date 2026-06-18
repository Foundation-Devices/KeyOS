// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    num_derive::FromPrimitive,
    num_traits::FromPrimitive,
    server::{AsScalar, CheckedConn, CheckedPermissions, FromScalar, MessageAllowed},
    xous::{MemoryRange, CID, PID, SID},
};

pub mod consts;
pub mod error;
pub mod msg;
pub mod navigation;
#[cfg(not(keyos))]
pub mod simulator;
pub mod touch;

pub use error::GuiServerError;

#[macro_export]
macro_rules! use_api {
    ($gui:path, $server:path) => {
        mod gui_permissions {
            use gui_server_api::msg::*;
            pub use $gui as gui_server_api;
            use $server as server;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/gui-server"]
            pub struct GuiPermissions;
        }
        type GuiApi = gui_permissions::gui_server_api::GuiApi<gui_permissions::GuiPermissions>;
        type GuiApiLight = gui_permissions::gui_server_api::GuiApiLight<gui_permissions::GuiPermissions>;
    };
    () => {
        gui_server_api::use_api!(gui_server_api, server);
    };
}

pub type AppName = String;

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RegisterApp {
    pub cid: CID,
    pub name: AppName,
    pub height: usize,
}

#[derive(Debug, Copy, Clone, FromPrimitive, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Default)]
pub enum ModalStyle {
    /// A regular modal card that slides up from the bottom of the screen.
    /// The user can drag it and dismiss it by dragging or clicking away.
    #[default]
    SlideUpDraggablePopup = 0,

    /// A modal card that slides up from the bottom of the screen.
    /// The user can't drag it and dismiss it by clicking away.
    SlideUpFixedPopup,

    /// A modal card that slides up from the bottom of the screen and takes the entire screen.
    SlideUpFullscreen,

    /// A modal that appears instantly with no animation.
    Instant,
}

/// Reduced GUI API, usable by non-gui daemons
#[derive(Clone, Debug, Default)]
pub struct GuiApiLight<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

/// Full GUI API, usable by GUI apps
#[derive(Debug)]
pub struct GuiApi<P: CheckedPermissions> {
    inner: GuiApiLight<P>,
    cid_self: CID,
    sid: SID,
}

impl<P: CheckedPermissions> GuiApiLight<P> {
    /// Blocking connect: waits until gui-server has registered its name. gui-server is a
    /// mandatory system service, so callers wait for it instead of timing out and failing.
    pub fn connect() -> Self { Self { conn: CheckedConn::default() } }

    /// Switches the focus to the app window of the given PID and the app zoom-in start position.
    /// Used by the app launcher, app switcher, and usb-debug protocol.
    pub fn switch_to(&self, next_pid: PID, x: usize, y: usize) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::SwitchTo>,
    {
        self.conn.try_send_scalar(msg::SwitchTo { next_pid: next_pid.get() as usize, x, y })?;
        Ok(())
    }

    /// Switches the focus to the launcher app window. Used by apps.
    pub fn switch_to_launcher(&self) -> Result<bool, GuiServerError>
    where
        P: MessageAllowed<msg::SwitchToLauncher>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::SwitchToLauncher)?)
    }

    pub fn is_locked(&self) -> Result<bool, GuiServerError>
    where
        P: MessageAllowed<msg::IsLocked>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::IsLocked)?)
    }

    pub fn shutdown(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::Shutdown>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::Shutdown { reboot: false })?)
    }

    pub fn reboot(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::Shutdown>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::Shutdown { reboot: true })?)
    }

    /// Closes the app window of the given PID.
    /// Used by the launcher, switcher, and usb-debug protocol to gracefully close apps.
    pub fn close_app(&self, pid: PID) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::CloseApp>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::CloseApp { pid: pid.get() as usize })??)
    }

    /// Captures the current composited screen as raw pixel data.
    /// Returns a `DropDeallocate` of length `FB_SIZE` (SCREEN_WIDTH * SCREEN_HEIGHT * 4)
    /// that auto-unmaps on drop. Dereferences to `MemoryRange` / `&[u8]`.
    pub fn capture_screen(&self) -> Result<xous::DropDeallocate, GuiServerError>
    where
        P: MessageAllowed<msg::CaptureScreen>,
    {
        let mem = xous::map_memory(None, None, consts::FB_SIZE, xous::MemoryFlags::W)?;
        self.conn.lend_mut(msg::CaptureScreen(mem));
        Ok(xous::DropDeallocate::new(mem))
    }

    /// Injects a touch event as if it came from the hardware touch controller.
    pub fn inject_touch(&self, touch: touch::Touch) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::InjectTouch>,
    {
        self.conn.try_send_scalar(msg::InjectTouch(touch))?;
        Ok(())
    }

    /// Injects a key press or release event into the active app.
    pub fn inject_key(&self, is_pressed: bool, key: Key) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::InjectKey>,
    {
        self.conn.try_send_scalar(msg::InjectKey { is_pressed, key })?;
        Ok(())
    }

    /// Injects a power button press or release into gui-server's power-button state machine.
    pub fn inject_power_button(&self, is_pressed: bool) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::InjectPowerButton>,
    {
        self.conn.try_send_scalar(msg::InjectPowerButton(is_pressed))?;
        Ok(())
    }

    pub fn update_kiosk_policy(&self, policy: msg::UpdateKioskPolicy) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::UpdateKioskPolicy>,
    {
        self.conn.try_send_scalar(policy)?;
        Ok(())
    }
}

impl<P: CheckedPermissions> GuiApi<P> {
    /// Registers an ordinary app window.
    pub fn register(name: &str, height: usize) -> Result<Self, GuiServerError>
    where
        P: MessageAllowed<msg::RegisterAppMessage>,
    {
        let (api, cid) = Self::register_inner()?;
        api.inner.conn.send_blocking_archive(msg::RegisterAppMessage(RegisterApp {
            cid,
            name: name.into(),
            height,
        }))?;
        Ok(api)
    }

    /// Registers as the control center, which gui-server tracks as a dedicated
    /// overlay window rather than an ordinary app.
    pub fn register_control_center(height: usize) -> Result<Self, GuiServerError>
    where
        P: MessageAllowed<msg::RegisterControlCenter>,
    {
        let (api, cid) = Self::register_inner()?;
        api.inner.conn.send_blocking_archive(msg::RegisterControlCenter { cid, height })?;
        Ok(api)
    }

    /// Registers as the keyboard, which gui-server tracks as a dedicated overlay
    /// window rather than an ordinary app.
    pub fn register_keyboard(height: usize) -> Result<Self, GuiServerError>
    where
        P: MessageAllowed<msg::RegisterKeyboard>,
    {
        let (api, cid) = Self::register_inner()?;
        api.inner.conn.send_blocking_archive(msg::RegisterKeyboard { cid, height })?;
        Ok(api)
    }

    /// Claims a privileged role, then registers an ordinary app window. The role is
    /// granted per message type, so an app can only claim a role its manifest permits.
    pub fn register_with_role<M>(name: &str, height: usize) -> Result<Self, GuiServerError>
    where
        M: msg::RoleClaim,
        P: MessageAllowed<msg::RegisterAppMessage> + MessageAllowed<M>,
    {
        let (api, cid) = Self::register_inner()?;
        api.inner.conn.send_blocking_scalar(M::default());
        api.inner.conn.send_blocking_archive(msg::RegisterAppMessage(RegisterApp {
            cid,
            name: name.into(),
            height,
        }))?;
        Ok(api)
    }

    fn register_inner() -> Result<(Self, CID), GuiServerError> {
        let sid = xous::create_server()?;
        let cid_self = xous::connect(sid)?;
        let inner = GuiApiLight::connect();
        let api = Self { inner, sid, cid_self };
        let gui_server_pid = api.inner.conn.get_remote_pid();

        let gui_server_cid = xous::connect_for_process(gui_server_pid, api.sid)?;
        xous::allow_messages_on_connection(gui_server_pid, gui_server_cid, 0..64)?;

        Ok((api, gui_server_cid))
    }

    pub fn sid(&self) -> SID { self.sid }

    /// Submit a frame for display.
    pub fn submit_frame(&self, buffer: MemoryRange) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::SubmitFrame>,
    {
        Ok(self.conn.try_send_move(msg::SubmitFrame { buffer })?)
    }

    pub fn show_camera(&self, y_pos: u16) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::ShowCamera>,
    {
        self.conn.try_send_scalar(msg::ShowCamera { y_pos })?;
        Ok(())
    }

    pub fn hide_camera(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::HideCamera>,
    {
        self.conn.try_send_scalar(msg::HideCamera)?;
        Ok(())
    }

    pub fn update_keyboard(&self, msg: msg::UpdateKeyboard) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::UpdateKeyboard>,
    {
        self.conn.try_send_archive(msg)?;
        Ok(())
    }

    pub fn hide_keyboard(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::HideKeyboard>,
    {
        self.conn.try_send_scalar(msg::HideKeyboard)?;
        Ok(())
    }

    pub fn notify_login_success(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::LoginSuccess>,
    {
        self.conn.try_send_scalar(msg::LoginSuccess)?;
        Ok(())
    }

    pub fn wake_event_loop(&self) {
        let msg = xous::Message::new_scalar(InputMessage::Noop as usize, 0, 0, 0, 0);
        if let Err(e) = xous::send_message(self.cid_self, msg) {
            log::error!("Failed to send wake event to self: {e:?}");
        }
    }

    pub fn request_redraw(&self) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::RequestRedraw>,
    {
        self.conn.try_send_scalar(msg::RequestRedraw)?;
        Ok(())
    }

    pub fn try_receive_input(&self) -> Option<(InputMessage, xous::MessageEnvelope)> {
        if let Ok(Some(msg)) = xous::try_receive_message(self.sid) {
            let opcode = FromPrimitive::from_usize(msg.body.id());
            return opcode.map(|opcode| (opcode, msg));
        }

        None
    }

    pub fn receive_input(&self) -> Result<(InputMessage, xous::MessageEnvelope), GuiServerError> {
        xous::receive_message(self.sid)
            .map(|msg| {
                let opcode = FromPrimitive::from_usize(msg.body.id());
                (opcode.expect("input opcode"), msg)
            })
            .map_err(Into::into)
    }

    pub fn key_pressed(&self, key: Key) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::KeyPressed>,
    {
        self.conn.try_send_scalar(msg::KeyPressed(Some(key)))?;
        Ok(())
    }

    pub fn key_released(&self, key: Key) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::KeyReleased>,
    {
        self.conn.try_send_scalar(msg::KeyReleased(Some(key)))?;
        Ok(())
    }

    pub fn animate_next_frame(&self, animation_kind: NextFrameAnimationKind) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::AnimateNextFrame>,
    {
        self.conn.try_send_scalar(msg::AnimateNextFrame { animation_kind })?;
        Ok(())
    }
}

impl<P: CheckedPermissions> std::ops::Deref for GuiApi<P> {
    type Target = GuiApiLight<P>;

    fn deref(&self) -> &Self::Target { &self.inner }
}

#[derive(Debug, PartialEq, num_derive::FromPrimitive, num_derive::ToPrimitive, Copy, Clone)]
pub enum InputMessage {
    Touch = 0,
    KeyPress,
    KeyRelease,

    /// Another app has navigated to this app, and the app is now in modal focus.
    /// This input message is a notification to check the `GuiApi` for a navigation event.
    NavigationFocused,

    /// The app is being navigated away from and is no longer in modal focus.
    NavigationCancelled,

    /// The apps that block on input can unblock themselves by sending this message to themselves.
    Noop,

    /// The app is brought into the foreground.
    Visible,

    /// The app is getting minimized and hidden in the background.
    Hidden,

    /// A new framebuffer the app can draw to.
    /// Can be the same as a previous buffer or a completely new one.
    FrameBuffer,

    Custom1,
    Custom2,
    Custom3,
    Custom4,

    /// The app should exit gracefully after receiving this.
    CloseRequested,

    /// Mouse/trackpad scroll in the emulator (hosted mode only).
    /// Scalar args: arg1 = x (physical px), arg2 = y (physical px),
    ///              arg3 = delta_x (f32 bits), arg4 = delta_y (f32 bits).
    Scroll,
}

#[derive(Debug, Copy, Clone)]
pub enum Key {
    Char(usize),
    Backspace,
    Delete,
    CursorLeft,
    CursorRight,
    Enter,
    Tab,
}

impl server::AsScalar<2> for Key {
    fn as_scalar(&self) -> [u32; 2] {
        match self {
            Key::Char(c) => [0, *c as _],
            Key::Backspace => [1, 0],
            Key::Delete => [2, 0],
            Key::CursorLeft => [3, 0],
            Key::CursorRight => [4, 0],
            Key::Enter => [5, 0],
            Key::Tab => [6, 0],
        }
    }
}

impl server::FromScalar<2> for Key {
    fn from_scalar(value: [u32; 2]) -> Self {
        match value[0] {
            1 => Key::Backspace,
            2 => Key::Delete,
            3 => Key::CursorLeft,
            4 => Key::CursorRight,
            5 => Key::Enter,
            6 => Key::Tab,
            _ => Key::Char(value[1] as _),
        }
    }
}

impl<P: CheckedPermissions> Drop for GuiApi<P> {
    fn drop(&mut self) {
        if let Err(e) = xous::destroy_server(self.sid) {
            log::error!("Error destroying gui api event server: {e:?}");
        }
    }
}

#[derive(Debug, Copy, Clone, FromPrimitive, Default)]
pub enum NextFrameAnimationKind {
    #[default]
    SlideInLeft = 0,
    SlideInRight,
    SlideOutLeft,
    SlideOutRight,
}

#[derive(
    Debug,
    Copy,
    Clone,
    FromPrimitive,
    Default,
    PartialEq,
    Eq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub enum KeyboardKind {
    #[default]
    Alphanumeric = 0,
    Password,
    Numbers,
    Decimal,
    Email,
}

impl FromScalar<1> for KeyboardKind {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from_u32(value).unwrap_or_default() }
}

impl AsScalar<1> for KeyboardKind {
    fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
}

impl From<&ArchivedKeyboardKind> for KeyboardKind {
    fn from(archived: &ArchivedKeyboardKind) -> Self {
        rkyv::deserialize::<_, rkyv::rancor::Error>(archived).unwrap()
    }
}
