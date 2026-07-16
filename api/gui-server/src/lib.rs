// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    num_derive::FromPrimitive,
    num_traits::FromPrimitive,
    server::{AsScalar, CheckedConn, CheckedPermissions, FromScalar, MessageAllowed},
    xous::{CID, PID, SID},
};

pub mod consts;
pub mod error;
pub mod msg;
pub mod navigation;
#[cfg(not(keyos))]
pub mod simulator;
pub mod touch;
pub mod utils;

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

#[derive(Copy, Clone, Debug)]
pub struct DoubleBuffer {
    pub disp_buf: usize,
    pub work_buf: usize,
}

impl DoubleBuffer {
    /// # Safety
    ///
    /// This function is unsafe because it operates on raw pointers.
    pub unsafe fn fill_with(&mut self, fill: u8, len: usize) -> Result<(), xous::Error> {
        let mut work_buf_range = xous::MemoryRange::new(self.work_buf, len)?;
        let mut disp_buf_range = xous::MemoryRange::new(self.disp_buf, len)?;

        work_buf_range.as_slice_mut().fill(fill);
        disp_buf_range.as_slice_mut().fill(fill);

        #[cfg(keyos)]
        xous::flush_cache(work_buf_range, xous::CacheOperation::Clean)?;
        #[cfg(keyos)]
        xous::flush_cache(disp_buf_range, xous::CacheOperation::Clean)?;

        Ok(())
    }
}

#[derive(Copy, Clone, Debug)]
pub struct DoubleBufferVMA {
    pub disp_buf: VMALocation,
    pub work_buf: VMALocation,
}

impl DoubleBufferVMA {
    pub fn from_single(buf: VMALocation) -> DoubleBufferVMA {
        DoubleBufferVMA { disp_buf: buf, work_buf: buf }
    }

    pub fn swap(&mut self) -> &mut DoubleBufferVMA {
        *self = Self { disp_buf: self.work_buf, work_buf: self.disp_buf };
        self
    }

    /// Returns the virtual memory portion of the double buffer.
    pub fn to_double_buf_virt(&self) -> DoubleBuffer {
        DoubleBuffer { disp_buf: self.disp_buf.virt_addr, work_buf: self.work_buf.virt_addr }
    }
}

pub type AppName = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum AppKind {
    App,
    ControlCenter,
    Keyboard,
    Camera,
    Launcher,
    Settings,
    Onboarding,
    Switcher,
    LockScreen,
    Alerts,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RegisterApp {
    pub app_kind: AppKind,
    pub cid: CID,
    pub name: AppName,
    pub bufs: DoubleBufferRegistration,
}

#[cfg(not(keyos))]
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DoubleBufferRegistration {
    pub work_buf_id: [u8; 32],
    pub disp_buf_id: [u8; 32],
    pub size: usize,
}

#[cfg(keyos)]
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DoubleBufferRegistration {
    pub work_buf_id: usize,
    pub disp_buf_id: usize,
    pub size: usize,
}

impl DoubleBufferRegistration {
    #[cfg(not(keyos))]
    pub fn into_bufs(self) -> Result<DoubleBuffer, GuiServerError> {
        let disp_id = utils::str_from_u8_nul_utf8(&self.disp_buf_id)?;
        let work_id = utils::str_from_u8_nul_utf8(&self.work_buf_id)?;
        Ok(DoubleBuffer {
            work_buf: utils::fb_id_to_addr(disp_id, self.size)?,
            disp_buf: utils::fb_id_to_addr(work_id, self.size)?,
        })
    }

    #[cfg(keyos)]
    pub fn into_bufs(self) -> Result<DoubleBuffer, GuiServerError> {
        Ok(DoubleBuffer { work_buf: self.work_buf_id, disp_buf: self.disp_buf_id })
    }

    #[cfg(keyos)]
    pub fn create_mirrors(&self, gui_server_pid: PID) -> Result<DoubleBufferRegistration, GuiServerError> {
        let work_buf_range = unsafe { xous::MemoryRange::new(self.work_buf_id, self.size)? };
        let work_buf_mirror = xous::mirror_memory_to_pid(work_buf_range, gui_server_pid)?;

        let disp_buf_range = unsafe { xous::MemoryRange::new(self.disp_buf_id, self.size)? };
        let disp_buf_mirror = xous::mirror_memory_to_pid(disp_buf_range, gui_server_pid)?;

        Ok(DoubleBufferRegistration {
            work_buf_id: work_buf_mirror.as_ptr() as _,
            disp_buf_id: disp_buf_mirror.as_ptr() as _,
            size: self.size,
        })
    }

    pub fn swap(&mut self) -> &mut Self {
        *self = Self { disp_buf_id: self.work_buf_id, work_buf_id: self.disp_buf_id, size: self.size };
        self
    }
}

#[derive(Copy, Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct VMALocation {
    pub virt_addr: usize,
    pub phys_addr: usize,
}

impl VMALocation {
    pub fn new(virt_addr: usize, phys_addr: usize) -> VMALocation { Self { virt_addr, phys_addr } }

    pub fn new_vma(virt_addr: usize) -> Result<VMALocation, GuiServerError> { utils::to_vma(virt_addr) }

    pub fn shift_by(self, shift: usize) -> VMALocation {
        VMALocation { virt_addr: self.virt_addr + shift, phys_addr: self.phys_addr + shift }
    }

    /// # Safety
    ///
    /// This function is unsafe because it operates on raw pointers.
    pub unsafe fn fill_with(&mut self, fill: u32, len: usize) -> Result<(), xous::Error> {
        let mut work_buf_range = xous::MemoryRange::new(self.virt_addr, len)?;
        work_buf_range.as_slice_mut::<u32>().fill(fill);

        #[cfg(keyos)]
        xous::flush_cache(work_buf_range, xous::CacheOperation::Clean)?;

        Ok(())
    }
}

impl DoubleBuffer {
    pub fn swap(&mut self) -> &mut DoubleBuffer {
        *self = Self { disp_buf: self.work_buf, work_buf: self.disp_buf };
        self
    }

    pub fn into_vma(self) -> Result<DoubleBufferVMA, GuiServerError> {
        let work_buf = utils::to_vma(self.work_buf)?;
        let disp_buf = utils::to_vma(self.disp_buf)?;

        Ok(DoubleBufferVMA { work_buf, disp_buf })
    }

    #[cfg(keyos)]
    pub fn work_buf(&self, _size: usize) -> usize { self.work_buf }
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
#[derive(Debug, Default)]
pub struct GuiApiLight<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

/// Full GUI API, usable by GUI apps
#[derive(Debug)]
pub struct GuiApi<P: CheckedPermissions> {
    inner: GuiApiLight<P>,
    sid: SID,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vsync {
    /// Always wait for the LCDC to give back the backbuffer before returning from this call. This prevents
    /// visual artifacts because the app can be sure it doesn't write to in-use buffers.
    Wait = 0,
    /// Never wait for buffer swaps. Can cause artifacting.
    DontWait = 1,
    /// If the previous swap is hasn't been rendered at least once (i.e. the app is calling swap faster than
    /// the LCDC can consume it), wait before returning. This is a middle-ground between Wait and
    /// DontWait; it is equivalent to Wait if the app is fast, and to DontWait if it's slow.
    CapFPS = 2,
}

impl<P: CheckedPermissions> GuiApiLight<P> {
    /// Switches the focus to the app window of the given PID and the app zoom-in start position.
    /// Used by the app launcher and app switcher.
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

    pub fn update_kiosk_policy(&self, policy: msg::UpdateKioskPolicy) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::UpdateKioskPolicy>,
    {
        self.conn.try_send_scalar(policy)?;
        Ok(())
    }
}

impl<P: CheckedPermissions> GuiApi<P> {
    pub fn register(
        app_kind: AppKind,
        name: &str,
        fb_size: usize,
    ) -> Result<(Self, DoubleBufferRegistration), GuiServerError>
    where
        P: MessageAllowed<msg::RegisterAppMessage>,
    {
        let bufs = utils::allocate_double_framebuffer(fb_size).expect("allocate app double framebuffer");
        let sid = xous::create_server()?.to_array();
        let api = Self { inner: GuiApiLight::default(), sid: sid.into() };
        let gui_server_pid = api.inner.conn.get_remote_pid();

        let gui_server_cid = xous::connect_for_process(gui_server_pid, api.sid)?;
        xous::allow_messages_on_connection(gui_server_pid, gui_server_cid, 0..64)?;

        let registration = RegisterApp {
            app_kind,
            cid: gui_server_cid,
            name: name.into(),
            #[cfg(keyos)]
            bufs: bufs.create_mirrors(gui_server_pid)?,
            #[cfg(not(keyos))]
            bufs: bufs.clone(),
        };

        api.inner.conn.send_archive(msg::RegisterAppMessage(registration))?;

        Ok((api, bufs))
    }

    pub fn sid(&self) -> SID { self.sid }

    /// Swap display buffers. Returns
    /// - Ok(Some(timestamp)) if everything went well, the timestamp being Ticktimer::elapsed_ms() when the
    ///   buffer was last sent to the LCDC
    /// - Ok(None) if the message was passed, but no actual swap took place,
    /// - Err() in case of a fatal error
    pub fn swap_buffers(&self, vsync: Vsync) -> Result<Option<u64>, GuiServerError>
    where
        P: MessageAllowed<msg::SwapBuffers>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::SwapBuffers { vsync })?)
    }

    pub fn is_camera_ready(&self) -> Result<bool, GuiServerError>
    where
        P: MessageAllowed<msg::IsCameraReady>,
    {
        Ok(self.conn.try_send_blocking_scalar(msg::IsCameraReady)?)
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

    pub fn update_keyboard(&self, kind: KeyboardKind, request_caps: bool) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::UpdateKeyboard>,
    {
        self.conn.try_send_scalar(msg::UpdateKeyboard { kind, request_caps })?;
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

    /// Closes the app window of the given PID.
    /// Used by the app launcher when an app is crashed to perform necessary cleanups.
    ///
    /// Can't be called by other apps that are not the launcher.
    pub fn close_app(&self, pid: PID) -> Result<(), GuiServerError>
    where
        P: MessageAllowed<msg::CloseApp>,
    {
        self.conn.try_send_scalar(msg::CloseApp { pid: pid.get() as usize })?;
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

    /// The apps that block on input can unblock themselves by requesting a redraw from the `gui-server`.
    /// The `gui-server` will send this input message to an app that requested a redraw to unblock
    /// its event loop thread.
    RedrawRequested,

    /// The app is brought into the foreground.
    Visible,

    /// The app is getting minimized and hidden in the background.
    Hidden,

    Custom1,
    Custom2,
    Custom3,
    Custom4,

    /// The app should exit gracefully after receiving this.
    CloseRequested,
}

#[derive(Debug, Copy, Clone)]
pub enum Key {
    Char(usize),
    Backspace,
    Delete,
    CursorLeft,
    CursorRight,
}

impl server::AsScalar<2> for Key {
    fn as_scalar(&self) -> [u32; 2] {
        match self {
            Key::Char(c) => [0, *c as _],
            Key::Backspace => [1, 0],
            Key::Delete => [2, 0],
            Key::CursorLeft => [3, 0],
            Key::CursorRight => [4, 0],
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
            _ => Key::Char(value[1] as _),
        }
    }
}

impl<P: CheckedPermissions> Drop for GuiApi<P> {
    fn drop(&mut self) { xous::destroy_server(self.sid).unwrap(); }
}

#[derive(Debug, Copy, Clone, FromPrimitive, Default)]
pub enum NextFrameAnimationKind {
    #[default]
    SlideInLeft = 0,
    SlideInRight,
    SlideOutLeft,
    SlideOutRight,
}

#[derive(Debug, Copy, Clone, FromPrimitive, Default, PartialEq, Eq)]
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
