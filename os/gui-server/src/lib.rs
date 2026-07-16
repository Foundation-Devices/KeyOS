// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

mod animation;
mod auto_lock;
mod blur;
#[cfg(not(feature = "recovery-os"))]
mod camera;
mod close;
mod control_center;
mod display;
#[cfg(keyos)]
mod gpio;
mod handlers;
mod keyboard;
mod layers;
mod modal;
mod navigation;
mod pwrbutton;
mod registry;
mod rgbled;
mod switcher;
mod touch;
mod virtbutton;

use {
    crate::{
        animation::{BacklightAnimation, SwitchingAnimation},
        control_center::ControlCenterWindow,
        display::PlatformDisplay,
        handlers::*,
        keyboard::{KeyboardState, KeyboardWindow},
        modal::ModalState,
        pwrbutton::PowerButtonState,
        registry::AppRegistry,
        rgbled::RgbLedState,
        touch::TouchState,
    },
    animation::{AnimationCompleteAction, NextFrameAnimationState, ProgressControl},
    auto_lock::AutoLockState,
    blur::{BlurBufferState, BlurThread},
    gui_server_api::{
        consts::FB_SIZE_BYTES, msg::*, AppName, DoubleBufferVMA, GuiServerError, InputMessage, KeyboardKind,
        RegisterApp, Vsync,
    },
    log::{debug, error, info, warn},
    server::{ArchiveRequest, BlockingScalarRequest, MessageId as _, Server, ServerContext},
    std::{
        collections::{HashMap, HashSet},
        time::{Duration, Instant},
    },
    xous::{MemoryFlags, MemoryRange, SystemEvent, CID, PID},
    xous_ticktimer::{Ticktimer, TicktimerCallback},
};

app_manager::use_api!();
#[cfg(not(feature = "recovery-os"))]
fs::use_api!();
haptics::use_api!();
power_manager::use_api!();
#[cfg(not(feature = "recovery-os"))]
security::use_api!();
#[cfg(not(feature = "recovery-os"))]
settings::use_api!();
#[cfg(not(feature = "recovery-os"))]
bt::use_api!();

const HAPTICS_CONNECTION_TIMEOUT_MS: u64 = 1000;

#[derive(Debug)]
pub struct AppWindow {
    name: AppName,
    state: AppState,
    input_cid: CID,
    bufs: DoubleBufferVMA,
    blur_state: BlurBufferState,
    keyboard_state: KeyboardState,
    display_control_center: bool,
    #[cfg(not(feature = "recovery-os"))]
    camera_state: crate::camera::CameraState,
}

#[derive(Debug)]
pub(crate) enum AppState {
    /// Registered, but first frame not displayed
    Starting,
    /// Received at least one frame
    Active { last_activated: Instant },
    /// CloseRequested sent to the app, waiting for it to close
    Closing,
    /// Closing state timed out, terminate_process called on it.
    Terminating,
}

#[derive(Debug)]
pub(crate) enum GuiState {
    BootSplash,
    BootFade {
        to: PID,
        progress: usize,
    },

    /// The current window is being displayed.
    SingleWindow {
        pid: PID,
        next_frame_animation: NextFrameAnimationState,
        navigation_request: Option<ArchiveRequest<NavigateTo>>,
    },
    Switching {
        from: PID,
        to: PID,
        progress: usize,
        animation: SwitchingAnimation,
        navigation_request: Option<ArchiveRequest<NavigateTo>>,
    },

    /// Displaying one app on top of the other (as a modal)
    Modal(ModalState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupState {
    InitialLockScreen,
    WaitingForOnboardingPID,
    WaitingForLauncherPID,
    Started,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct KioskPolicy {
    auto_lock_enabled: bool,
    control_center_enabled: bool,
    home_button_enabled: bool,
    power_button_enabled: bool,
}

impl Default for KioskPolicy {
    fn default() -> Self {
        Self {
            auto_lock_enabled: true,
            control_center_enabled: true,
            home_button_enabled: true,
            power_button_enabled: true,
        }
    }
}

#[derive(server::Server)]
#[name = "os/gui-server"]
pub struct Gui {
    sid: Option<xous::SID>,
    windows: HashMap<PID, AppWindow>,

    app_registry: AppRegistry,

    waiting_for_pid: Option<(PID, Option<ArchiveRequest<NavigateTo>>)>,

    control_center_window: Option<ControlCenterWindow>,
    keyboard_window: Option<KeyboardWindow>,

    #[cfg(not(feature = "recovery-os"))]
    camera_window: Option<crate::camera::CameraWindow>,

    display: PlatformDisplay,
    vsync_waiters: Vec<BlockingScalarRequest<SwapBuffers>>,
    cap_fps_phase1: HashSet<PID>,
    cap_fps_phase2: HashSet<PID>,
    last_vsync: u64,
    ticktimer: Ticktimer,

    state: GuiState,
    animation_fb: MemoryRange,
    touch_state: TouchState,
    rgb_led: RgbLedState,
    power_button_state: PowerButtonState,
    kiosk_policies: HashMap<PID, KioskPolicy>,
    auto_lock: AutoLockState,
    close_app_callback: Option<TicktimerCallback>,
    shutting_down: Option<bool>,
    blur_thread: BlurThread,
    backlight_animation: Option<BacklightAnimation>,

    #[cfg(not(feature = "recovery-os"))]
    security: Security,
    #[cfg(not(feature = "recovery-os"))]
    settings: SettingsApi,
    startup_state: StartupState,
}

impl Server for Gui {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.sid = Some(context.sid());

        xous::register_system_event_handler(
            SystemEvent::Disconnected,
            context.sid(),
            DisconnectHandlerMessage::ID,
        )
        .expect("register children crash handler");

        xous::register_system_event_handler(
            SystemEvent::LowFreeMemory,
            context.sid(),
            OnFreeMemoryBelowThreshold::ID,
        )
        .expect("register free memory alert handler");

        #[cfg(keyos)]
        self.subscribe_to_gpio(context);

        self.power_button_state.init(context.sid()).expect("Failed to initialize timer state");

        #[cfg(not(feature = "recovery-os"))]
        {
            self.settings.server_subscribe_screen_brightness(context);
            self.settings.server_subscribe_touch_offset(context);
            self.settings.server_subscribe_onboarding_status(context);
            FileSystem::default().subscribe_filesystem_events(context, fs::Location::AppData);
        }

        self.init_auto_lock(context);

        self.display.subscribe_to_vsync(context);

        self.close_app_callback =
            Some(TicktimerCallback::new(context.sid()).expect("Cannot register close app callback"));

        self.blur_thread.start(context.sid());

        #[cfg(not(keyos))]
        let _ = context;
    }
}

impl Gui {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Result<Self, GuiServerError> {
        let display = PlatformDisplay::init(Self::boot_splash_layer());
        #[cfg(not(feature = "recovery-os"))]
        let security = security::Security::default();
        #[cfg(not(feature = "recovery-os"))]
        let startup_state = if let security::MasterKeyState::Normal = security.master_key_state() {
            StartupState::InitialLockScreen
        } else {
            Self::launch_onboarding();
            StartupState::WaitingForOnboardingPID
        };
        #[cfg(feature = "recovery-os")]
        let startup_state = StartupState::WaitingForLauncherPID;

        let animation_fb = xous::map_memory(
            None,
            None,
            FB_SIZE_BYTES,
            MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::PLAINTEXT,
        )
        .expect("Could not allocate animation buffer");

        Ok(Gui {
            sid: None,
            state: GuiState::BootSplash,
            windows: HashMap::new(),
            app_registry: Default::default(),
            keyboard_window: None,
            control_center_window: None,
            #[cfg(not(feature = "recovery-os"))]
            camera_window: None,
            waiting_for_pid: None,
            animation_fb,

            display,
            vsync_waiters: Default::default(),
            cap_fps_phase1: Default::default(),
            cap_fps_phase2: Default::default(),
            last_vsync: 0,
            ticktimer: Ticktimer::new()?,

            touch_state: TouchState::init(),
            rgb_led: RgbLedState::default(),
            power_button_state: PowerButtonState::default(),
            kiosk_policies: HashMap::new(),
            auto_lock: AutoLockState::default(),
            close_app_callback: None,
            shutting_down: None,
            blur_thread: BlurThread::default(),
            backlight_animation: None,

            #[cfg(not(feature = "recovery-os"))]
            security,
            #[cfg(not(feature = "recovery-os"))]
            settings: SettingsApi::default(),
            startup_state,
        })
    }

    pub fn with_active_app<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&AppWindow) -> R,
    {
        if let Some(current_app) = self.active_app_pid().and_then(|pid| self.windows.get(&pid)) {
            return Some(f(current_app));
        }

        None
    }

    pub fn with_active_app_mut<F, R>(&mut self, f: F) -> Option<R>
    where
        F: FnOnce(&mut AppWindow) -> R,
    {
        if let Some(current_app) = self.active_app_pid().and_then(|pid| self.windows.get_mut(&pid)) {
            return Some(f(current_app));
        }

        None
    }

    fn handle_register_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        let bufs = msg.bufs.into_bufs()?.into_vma()?;

        info!(
            "Registering app `{}` with buffers: work={:016x} disp={:016x}, pid={}, cid={:?}",
            msg.name, bufs.work_buf.phys_addr, bufs.disp_buf.phys_addr, pid, msg.cid,
        );

        self.windows.insert(
            pid,
            AppWindow {
                name: msg.name,
                state: AppState::Starting,
                bufs,
                blur_state: BlurBufferState::default(),
                keyboard_state: KeyboardState::default(),
                #[cfg(not(feature = "recovery-os"))]
                camera_state: crate::camera::CameraState::default(),
                input_cid: msg.cid,
                display_control_center: true,
            },
        );

        debug!("Returning success");
        Ok(())
    }

    fn handle_register_control_center_app(
        &mut self,
        pid: PID,
        msg: RegisterApp,
    ) -> Result<(), GuiServerError> {
        let bufs = msg.bufs.into_bufs()?.into_vma()?;
        info!(
            "Registering Control Center `{}` with buffers: work={:08x} disp={:08x}, cid={:?}",
            msg.name, bufs.work_buf.phys_addr, bufs.disp_buf.phys_addr, msg.cid,
        );

        self.control_center_window = Some(ControlCenterWindow::new(msg.cid, pid, bufs));
        // Control center starts out as visible, let it know
        xous::send_message(msg.cid, xous::Message::new_scalar(InputMessage::Visible as usize, 0, 0, 0, 0))
            .map_err(|e| error!("Failed to notify control center of being visible: {e:?}"))
            .ok();
        Ok(())
    }

    fn handle_register_keyboard_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        let bufs = msg.bufs.into_bufs()?.into_vma()?;

        info!(
            "Registering keyboard `{}` with buffers: work={:08x} disp={:08x} cid={:?}",
            msg.name, bufs.work_buf.phys_addr, bufs.disp_buf.phys_addr, msg.cid,
        );

        self.keyboard_window = Some(KeyboardWindow {
            input_cid: msg.cid,
            pid,
            bufs,
            blur_state: BlurBufferState::default(),
            last_drawn_keyboard_kind: KeyboardKind::Numbers,
            last_requested_keyboard_kind: KeyboardKind::Numbers,
            notified_shown: false,
        });

        Ok(())
    }

    #[cfg(not(feature = "recovery-os"))]
    fn handle_register_camera_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        let bufs = msg.bufs.into_bufs()?.into_vma()?;

        info!(
            "Registering camera `{}` with buffers: work={:08x} disp={:08x} cid={:?}",
            msg.name, bufs.work_buf.phys_addr, bufs.disp_buf.phys_addr, msg.cid,
        );

        self.camera_window =
            Some(crate::camera::CameraWindow { input_cid: msg.cid, pid, bufs, notified_visible: false });

        Ok(())
    }

    #[cfg(feature = "recovery-os")]
    fn handle_register_camera_app(&mut self, _pid: PID, _msg: RegisterApp) -> Result<(), GuiServerError> {
        log::error!("Attempting to register camera in recovery mode");
        Err(GuiServerError::InternalError)
    }

    fn handle_register_launcher_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        self.app_registry.set_launcher_app_pid(pid);
        if self.startup_state == StartupState::WaitingForLauncherPID {
            self.waiting_for_pid = Some((pid, None));
            self.startup_state = StartupState::Started;
        }
        self.handle_register_app(pid, msg)
    }

    fn handle_register_settings_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        self.app_registry.set_settings_app_pid(pid);
        self.handle_register_app(pid, msg)
    }

    fn handle_register_onboarding_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        self.app_registry.set_onboarding_app_pid(pid);
        if self.startup_state == StartupState::WaitingForOnboardingPID {
            self.waiting_for_pid = Some((pid, None));
            self.startup_state = StartupState::Started;
        }
        self.handle_register_app(pid, msg)
    }

    fn handle_register_lock_screen_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        self.app_registry.set_lock_screen_pid(pid);
        if self.startup_state == StartupState::InitialLockScreen {
            self.waiting_for_pid = Some((pid, None));
        }
        self.handle_register_app(pid, msg)
    }

    fn handle_register_switcher_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        log::info!("Registering switcher app with PID={pid}");

        self.app_registry.set_switcher_app_pid(pid);
        self.handle_register_app(pid, msg)
    }

    fn handle_register_alerts_app(&mut self, pid: PID, msg: RegisterApp) -> Result<(), GuiServerError> {
        log::info!("Registering alerts app with PID={pid}");

        self.app_registry.set_alerts_app_pid(Some(pid));
        self.handle_register_app(pid, msg)
    }

    fn switch_to_launcher(&mut self) {
        if let Some(launcher_app_pid) = self.app_registry.launcher_app_pid() {
            match &mut self.state {
                // Special case: we are already displaying the launcher, but it's in a modal state:
                GuiState::Modal(modal_state) if modal_state.background_pid() == launcher_app_pid => {
                    modal_state.respond(Err(gui_server_api::error::NavigationError::CanceledBySystem));
                }
                _ => self.switch_to_window(launcher_app_pid),
            }
        } else {
            warn!("Tried to switch to launcher while no launcher is registered");
        }
    }

    fn switch_to_app_switcher(&mut self) {
        if let Some(switcher_app_pid) = self.app_registry.switcher_app_pid() {
            self.switch_to_window(switcher_app_pid);
        } else {
            warn!("Tried to switch to switcher while no switcher is registered");
        }
    }

    fn handle_update_buffers(&mut self, request: BlockingScalarRequest<SwapBuffers>) {
        let pid = request.response.pid();
        #[cfg(not(feature = "recovery-os"))]
        let camera_swapped = self.swap_camera_bufs(pid);
        #[cfg(feature = "recovery-os")]
        let camera_swapped = false;

        if camera_swapped
            || self.swap_control_center_bufs(pid)
            || self.swap_keyboard_bufs(pid)
            || self.swap_window_bufs(pid)
        {
            self.update_layers();
            self.handle_vsync_after_swap(request);
        }
        // The default response to msg is `None`, so if it was not handled above,
        // it will be freed here and a failure is returned.
    }

    fn swap_window_bufs(&mut self, pid: PID) -> bool {
        let Some(window) = self.windows.get_mut(&pid) else {
            return false;
        };
        let last_disp_buf = window.bufs.disp_buf.virt_addr;

        window.bufs.swap();
        window.blur_state.mark_stale();

        match &window.state {
            AppState::Starting => {
                window.state = AppState::Active { last_activated: Instant::now() };
                let name = window.name.clone();
                self.notify_switcher_app_started(pid, &name);
                self.notify_switcher_update_app_fb(pid);
            }
            AppState::Active { .. } => {}
            AppState::Closing | AppState::Terminating => return false,
        }

        if let Some((wait_pid, nav)) = &mut self.waiting_for_pid {
            if *wait_pid == pid {
                let nav = core::mem::take(nav);
                self.switch_to_window_with_nav(pid, nav);
                self.waiting_for_pid = None;
            }
        }
        match &mut self.state {
            GuiState::Modal(modal_state) if modal_state.modal_pid() == pid && modal_state.is_waiting() => {
                modal_state.expand();
                self.send_visible_event(pid);
                self.send_navigation_focused_event(pid);
            }

            GuiState::SingleWindow { pid: current_pid, next_frame_animation, .. } if *current_pid == pid => {
                if let NextFrameAnimationState::Waiting { kind } = next_frame_animation {
                    let work_fb = unsafe { xous::MemoryRange::new(last_disp_buf, FB_SIZE_BYTES).unwrap() };
                    self.animation_fb.as_slice_mut::<u32>().copy_from_slice(work_fb.as_slice());
                    #[cfg(keyos)]
                    xous::flush_cache(self.animation_fb, xous::CacheOperation::Clean).ok();

                    *next_frame_animation = NextFrameAnimationState::Animating { progress: 0, kind: *kind };
                }
            }
            _ => {}
        }
        true
    }

    fn handle_vsync_after_swap(&mut self, mut request: BlockingScalarRequest<SwapBuffers>) {
        request.response.set_response(Some(self.last_vsync));
        let pid = request.response.pid();
        if !self.display.is_lcd_on() {
            // The display is off, so we will never get a vsync, just let the swap happen
            // The app probably redrawn due to an event
            return;
        }
        match request.message.vsync {
            Vsync::Wait => self.vsync_waiters.push(request),
            Vsync::DontWait => {}
            Vsync::CapFPS => {
                // If requests come slower than vsync, the pid will never be in the cap hashset, and this
                // becomes equivalent to Vsync::DontWait:
                // VSync    |        |        |        |         |        |        |
                // cap1     ---------   ------           --------      ---          -
                // cap2
                // Req      |           |                |             |            |
                // Return   |           |                |             |            |

                // If requests come faster, it's equivalent to Vsync::Wait:
                // VSync    |        |        |        |         |        |        |
                // cap1     ---------------------------------------------------------
                // cap2         -----    -----    ----     ------   ------    -----
                // Req      |   |        |        |        |        |         |
                // Return   |        |        |        |         |        |        |

                // Transition from Fast to Slow:
                // VSync    |        |        |        |         |        |        |
                // cap1     ---------------------------   -------      ---          --
                // cap2         ------   ------
                // Req      |   |        |                |            |            |
                // Return   |        |        |           |            |            |

                // Transition from Slow to Fast:
                // VSync    |        |        |        |         |        |        |
                // cap1     ---------   ------  --------------------------------------
                // cap2                              ---   ------   ------    -----
                // Req      |           |       |    |     |        |         |
                // Return   |           |       |      |         |        |        |

                if self.cap_fps_phase1.contains(&pid) {
                    self.cap_fps_phase2.insert(pid);
                    self.vsync_waiters.push(request);
                } else {
                    self.cap_fps_phase1.insert(pid);
                }
            }
        }
    }

    fn clear_vsync_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.vsync_waiters) {
            waiter.response.respond(Some(self.last_vsync)).ok();
        }
    }

    fn on_vsync(&mut self) {
        self.cap_fps_phase1 = core::mem::take(&mut self.cap_fps_phase2);
        self.clear_vsync_waiters();
        self.last_vsync = self.ticktimer.elapsed_ms();
        self.keyboard_animation_tick();
        self.state_animation_tick();
        self.control_center_animation_tick();
        self.backlight_animation_tick();
        self.blur_vsync();
        self.update_layers();
        self.switcher_timeout_tick();
    }

    fn switch_to_window(&mut self, pid: PID) { self.switch_to_window_with_nav(pid, None); }

    fn switch_to_window_with_nav(
        &mut self,
        pid: PID,
        navigation_request: Option<ArchiveRequest<NavigateTo>>,
    ) {
        match self.windows.get(&pid).map(|w| &w.state) {
            None | Some(AppState::Starting) => {
                self.waiting_for_pid = Some((pid, navigation_request));
                return;
            }
            Some(AppState::Active { .. }) => {}
            Some(AppState::Closing) | Some(AppState::Terminating) => {
                log::error!("Trying to switch to closing app pid={pid}");
                return;
            }
        }
        let from = match &self.state {
            GuiState::BootSplash => {
                log::info!("Switching to initial window, PID={pid}");
                self.rgb_led.turn_on();
                self.change_state(GuiState::BootFade { to: pid, progress: 0 });
                self.reset_auto_lock();
                return;
            }
            GuiState::BootFade { to, .. } => *to,
            GuiState::SingleWindow { pid, .. } => *pid,
            GuiState::Switching { to, .. } => *to,
            GuiState::Modal(modal_state) => modal_state.background_pid(),
        };
        if pid == from {
            if navigation_request.is_some() {
                self.change_state(GuiState::SingleWindow {
                    pid,
                    next_frame_animation: NextFrameAnimationState::NotAnimating,
                    navigation_request,
                });
            }
        } else {
            let animation = self.switching_animation(from, pid);
            self.change_state(GuiState::Switching {
                from,
                to: pid,
                progress: 0,
                animation,
                navigation_request,
            });
        }
    }

    fn send_visible_event(&self, pid: PID) {
        if let Some(window) = self.windows.get(&pid) {
            let msg = xous::Message::new_scalar(InputMessage::Visible as usize, 0, 0, 0, 0);
            xous::send_message(window.input_cid, msg)
                .map_err(|e| error!("Failed to notify the app (PID {pid}) about being visible: {e:?}"))
                .ok();
        } else {
            error!("Can't notify visible, no app window with PID={pid} is known");
        }
    }

    pub(crate) fn send_hidden_event(&self, pid: PID) {
        if let Some(window) = self.windows.get(&pid) {
            let msg = xous::Message::new_scalar(InputMessage::Hidden as usize, 0, 0, 0, 0);
            xous::send_message(window.input_cid, msg)
                .map_err(|e| error!("Failed to notify the app (PID {pid}) about being hidden: {e:?}"))
                .ok();
        } else {
            error!("Can't notify hidden, no app window with PID={pid} is known");
        }
    }

    fn haptics_server_connection(&mut self) -> Option<HapticsApi> {
        HapticsApi::try_new_with_timeout(Duration::from_millis(HAPTICS_CONNECTION_TIMEOUT_MS))
    }

    pub(crate) fn haptics_click(&mut self) {
        if let Some(haptics) = self.haptics_server_connection() {
            haptics.click();
        }
    }

    pub(crate) fn haptics_triple_click(&mut self) {
        if let Some(haptics) = self.haptics_server_connection() {
            haptics.triple_click();
        }
    }

    pub(crate) fn shutdown(&mut self, reboot: bool) {
        self.shutting_down = Some(reboot);
        if self.display.is_lcd_on() {
            self.touch_off();
            self.turn_off_lcd();
        }
        self.close_all_apps();
    }

    pub(crate) fn finalize_shutdown(&mut self) {
        #[cfg(not(feature = "recovery-os"))]
        self.settings.flush_settings();
        #[cfg(not(feature = "recovery-os"))]
        BluetoothApi::default().disconnect().ok();

        // XXX: Leave Some time for the last few logs to actually print
        std::thread::sleep(Duration::from_millis(50));

        let reboot = self.shutting_down.take().unwrap_or_default();
        let pwr = PowerManagerApi::default();

        // Disable OTG and USB boost before shutting down, so we don't
        // continue draining the battery into a connected slave device.
        #[cfg(keyos)]
        {
            pwr.set_otg_priority(power_manager::OtgPriority::Never).ok();
            pwr.set_usb_boost(false).ok();
        }

        if reboot {
            pwr.reboot().expect("reboot failed");
        } else {
            let _ = xous::rsyscall(xous::SysCall::Shutdown(0));
        }
    }

    fn turn_off_lcd(&mut self) {
        if let Some(control_center) = &self.control_center_window {
            xous::send_message(
                control_center.input_cid,
                xous::Message::new_scalar(InputMessage::Hidden as usize, 0, 0, 0, 0),
            )
            .map_err(|e| error!("Failed to notify control center of LCD turning off: {e:?}"))
            .ok();
        }
        if let Some(active_pid) = self.active_app_pid() {
            self.send_hidden_event(active_pid);
        }

        #[cfg(not(feature = "recovery-os"))]
        self.camera_window_notify_hidden();

        self.rgb_led.turn_off();
        self.touch_off();
        self.animate_backlight_to(0, AnimationCompleteAction::LcdOff);
        self.clear_vsync_waiters();
    }

    fn turn_on_lcd(&mut self) {
        self.touch_on();
        self.animate_backlight_to(self.screen_brightness_setting(), AnimationCompleteAction::None);
        self.rgb_led.turn_on();
        self.display.turn_lcd_on();
        if let Some(control_center) = &self.control_center_window {
            // Control center is always visible as long as LCD is on
            xous::send_message(
                control_center.input_cid,
                xous::Message::new_scalar(InputMessage::Visible as usize, 0, 0, 0, 0),
            )
            .map_err(|e| error!("Failed to notify control center of LCD turning on: {e:?}"))
            .ok();
        }
        if let Some(active_pid) = self.active_app_pid() {
            self.send_visible_event(active_pid);
        }

        #[cfg(not(feature = "recovery-os"))]
        self.update_camera_window();
        self.update_layers();
    }

    /// Returns a PID of an active (focused) app window, if any.
    fn active_app_pid(&self) -> Option<PID> {
        match &self.state {
            GuiState::SingleWindow { pid, .. }
            | GuiState::Switching { to: pid, .. }
            | GuiState::BootFade { to: pid, .. } => Some(*pid),
            GuiState::Modal(modal_state) => Some(modal_state.modal_pid()),
            GuiState::BootSplash => None,
        }
    }

    fn background_pid(&self) -> Option<PID> {
        match &self.state {
            GuiState::Switching { from, .. } => Some(*from),
            GuiState::Modal(modal_state) => Some(modal_state.background_pid()),
            GuiState::BootFade { .. } | GuiState::SingleWindow { .. } | GuiState::BootSplash => None,
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    fn lock(&mut self) {
        let Some(lock_screen_pid) = self.app_registry.lock_screen_pid() else {
            error!("No lock screen app PID found");
            return;
        };
        let current_app = self.background_pid().or_else(|| self.active_app_pid());
        if current_app == self.app_registry.onboarding_app_pid() {
            debug!("Not locking during onboarding");
            return;
        }

        // If the switcher is focused during the locking, show the launcher after unlocking
        let pre_lock_app = if self.active_app_pid() == self.app_registry.switcher_app_pid() {
            self.app_registry.pre_lock_app_id().or_else(|| self.app_registry.launcher_app_pid())
        } else {
            current_app
        };

        if self.app_registry.pre_lock_app_id().is_none() && self.startup_state == StartupState::Started {
            self.app_registry.set_pre_lock_app_pid(pre_lock_app);
        }
        self.control_center_collapse();
        self.security.log_out();
        self.change_state_single_window(lock_screen_pid, None);
    }

    fn unlock(&mut self) {
        if self.startup_state != StartupState::Started {
            // This is the initial unlock. Wait for onboarding status.
            return;
        }
        let app_id = self.app_registry.pre_lock_app_id().or(self.app_registry.launcher_app_pid());
        self.app_registry.set_pre_lock_app_pid(None);
        if let Some(pid) = app_id {
            self.switch_to_window(pid);
        } else {
            error!("No launcher app PID found");
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    fn is_locked(&self) -> bool {
        self.active_app_pid().is_some() && self.active_app_pid() == self.app_registry.lock_screen_pid()
    }

    fn home_button_enabled(&self) -> bool { self.control_enabled(|policy| policy.home_button_enabled, false) }

    fn power_button_enabled(&self) -> bool {
        self.control_enabled(|policy| policy.power_button_enabled, true)
    }

    fn control_center_enabled(&self) -> bool {
        self.control_enabled(|policy| policy.control_center_enabled, true)
    }

    #[cfg(all(keyos, not(feature = "recovery-os")))]
    fn auto_lock_enabled(&self) -> bool { self.control_enabled(|policy| policy.auto_lock_enabled, true) }

    fn control_enabled(&self, enabled: fn(KioskPolicy) -> bool, active_default: bool) -> bool {
        let modal_allows = self.modal_background_pid().map_or(true, |pid| enabled(self.kiosk_policy(pid)));
        let active_allows =
            self.active_app_pid().map_or(active_default, |pid| enabled(self.kiosk_policy(pid)));

        modal_allows && active_allows
    }

    fn kiosk_policy(&self, pid: PID) -> KioskPolicy {
        self.kiosk_policies.get(&pid).copied().unwrap_or_default()
    }

    #[cfg(not(feature = "recovery-os"))]
    pub fn launch_onboarding() {
        let app_already_running =
            xous::app_id_to_pid(&gui_server_api::navigation::ONBOARDING_APP_ID).unwrap_or_default().is_some();
        if !app_already_running {
            if let Err(e) =
                AppManagerApi::default().launch_app(&gui_server_api::navigation::ONBOARDING_APP_ID)
            {
                error!("Couldn't launch onboarding: {e:?}");
            }
        }
    }

    fn state_animation_tick(&mut self) {
        const BOOT_SPLASH_TICK: usize = 6;
        const NEXT_FRAME_TICK: usize = 12;

        match &mut self.state {
            GuiState::BootSplash => (),
            GuiState::SingleWindow { next_frame_animation, .. } => {
                if let NextFrameAnimationState::Animating { progress, .. } = next_frame_animation {
                    if *progress < 100 - NEXT_FRAME_TICK {
                        *progress += NEXT_FRAME_TICK;
                    } else if *progress < 100 {
                        // Do one last frame with the animation finished so we don't drop
                        // the framebuffer too early.
                        *progress = 100;
                    } else {
                        *next_frame_animation = NextFrameAnimationState::NotAnimating;
                    }
                }
            }
            GuiState::BootFade { to, progress } => {
                if *progress < 100 - BOOT_SPLASH_TICK {
                    *progress += BOOT_SPLASH_TICK;
                } else {
                    let pid = *to;
                    self.change_state_single_window(pid, None);
                    #[cfg(keyos)]
                    if let Err(e) = xous::unmap_memory(unsafe {
                        xous::MemoryRange::new(
                            xous::keyos::BOOT_SPLASH_FB,
                            xous::keyos::BOOT_SPLASH_PAGES * xous::keyos::PAGE_SIZE,
                        )
                        .unwrap()
                    }) {
                        warn!("Could not unmap boot splash frame buffer: {e:?}")
                    }
                }
            }
            GuiState::Switching { from, to, progress, navigation_request, animation, .. } => {
                let progress_step = animation.step_size_ticks();

                match animation {
                    SwitchingAnimation::ToSwitcher(ProgressControl::Abort) => {
                        if *progress >= progress_step {
                            *progress -= progress_step;
                        } else {
                            let pid = *from;
                            let _ = core::mem::take(navigation_request);
                            self.change_state_single_window(pid, None);
                        }
                    }

                    SwitchingAnimation::ToSwitcher(ProgressControl::Manual) => {}

                    _ => {
                        if *progress < 100usize.saturating_sub(progress_step) {
                            *progress += progress_step;
                        } else {
                            let pid = *to;
                            let navigation_request = core::mem::take(navigation_request);
                            self.change_state_single_window(pid, navigation_request);
                        }
                    }
                }
            }
            GuiState::Modal(modal_state) => {
                if modal_state.animation_tick() {
                    let pid = modal_state.background_pid();
                    // Modal was collapsed
                    self.change_state_single_window(pid, None);
                }
            }
        }
    }

    fn change_state_single_window(
        &mut self,
        pid: PID,
        navigation_request: Option<ArchiveRequest<NavigateTo>>,
    ) {
        if let Some(AppState::Active { last_activated }) = self.windows.get_mut(&pid).map(|w| &mut w.state) {
            *last_activated = Instant::now();
            self.notify_switcher_app_activated(pid);
        } else {
            log::error!("Changing state to SingleWindow to a window that's not Active (pid={pid})");
        }
        self.change_state(GuiState::SingleWindow {
            pid,
            next_frame_animation: NextFrameAnimationState::NotAnimating,
            navigation_request,
        });
    }

    fn change_state(&mut self, new_state: GuiState) {
        log::debug!("Changing state to: {new_state:?}");

        let previous_active_pid = self.active_app_pid();
        let previous_visible_pids = [previous_active_pid, self.background_pid()];
        let previous_nav_request = self.get_pending_nav_request();

        self.state = new_state;
        self.update_keyboard_window();
        #[cfg(not(feature = "recovery-os"))]
        self.update_camera_window();

        let current_active_pid = self.active_app_pid();
        let current_visible_pids = [current_active_pid, self.background_pid()];

        for previous_pid in previous_visible_pids.iter().flatten().copied() {
            if self.windows.contains_key(&previous_pid) && !current_visible_pids.contains(&Some(previous_pid))
            {
                self.send_hidden_event(previous_pid);
                self.notify_switcher_update_app_fb(previous_pid);
            }
        }
        for current_pid in current_visible_pids.iter().flatten().copied() {
            if self.windows.contains_key(&current_pid) && !previous_visible_pids.contains(&Some(current_pid))
            {
                self.send_visible_event(current_pid)
            }
        }

        let current_nav_request = self.get_pending_nav_request();
        if previous_active_pid != self.active_app_pid() || previous_nav_request != current_nav_request {
            if let Some(previous_active_pid) = previous_active_pid {
                if previous_nav_request.is_some() {
                    self.send_navigation_cancelled_event(previous_active_pid);
                }
            }
            if let Some(active_pid) = current_active_pid {
                if current_nav_request.is_some() {
                    self.send_navigation_focused_event(active_pid);
                }
            }
        }
        self.update_layers();
    }
}

#[cfg(not(keyos))]
pub fn open_window() { display::window::run_window() }

#[cfg(not(keyos))]
fn get_frame(entire_device: bool, mem: &mut xous::MemoryRange) {
    use gui_server_api::consts::{DEVICE_HEIGHT, DEVICE_WIDTH, SCREEN_HEIGHT, SCREEN_WIDTH};

    if entire_device {
        let mut image_buffer = image::ImageBuffer::from_raw(DEVICE_WIDTH, DEVICE_HEIGHT, mem.as_slice_mut())
            .expect("Screen grab buffer not big enough");
        display::draw::draw_whole_device(&mut image_buffer);
    } else {
        let mut image_buffer =
            image::ImageBuffer::from_raw(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, mem.as_slice_mut())
                .expect("Screen grab buffer not big enough");
        display::draw::draw_lcd_contents(&mut image_buffer);
    };
}
