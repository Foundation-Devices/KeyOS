// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gui_server_api::{
    consts::{FPS, SCREEN_HEIGHT},
    touch::{Touch, TouchKind},
    GuiApi, InputMessage, Key, NextFrameAnimationKind,
};
use i_slint_core::{item_rendering::DirtyRegion, renderer::RendererSealed};
use server::FromScalar;
#[cfg(not(feature = "recovery-os"))]
use slint::{platform::WindowAdapter, private_unstable_api::re_exports::WindowInner};
use slint::{
    platform::{software_renderer::LineBufferProvider, EventLoopProxy, PointerEventButton, WindowEvent},
    LogicalPosition, PhysicalPosition, PhysicalSize, PlatformError, SharedString,
};
use xous::{envelope::Envelope, DropDeallocate, MemoryRange};
use xous_ticktimer::{Ticktimer, TicktimerCallback};

use crate::{core::EventLoopStatus, pixel::KeyosPixel, window::KeyOsWindow, Runtime, StoredValue};

/// Width of the area on the left edge of the screen from which we detect a swipe right gesture.
const SWIPE_RIGHT_EDGE_AREA_WIDTH_PX: usize = 30; // TODO(SFT-5093): tweak setting
/// Minimum velocity (in pixels per second) required to consider a swipe right gesture valid.
const SWIPE_RIGHT_VELOCITY_THRESHOLD: f32 = 300.; // TODO(SFT-5093): tweak setting
/// Minimum swipe distance (in pixels) required to consider a swipe right gesture valid.
const SWIPE_RIGHT_DISTANCE_THRESHOLD: isize = 100; // TODO(SFT-5093): tweak setting

#[cfg(not(feature = "recovery-os"))]
const DEBUG_TOUCH_COLOR: slint::Color = slint::Color::from_argb_u8(64, 255, 0, 255);
#[cfg(not(feature = "recovery-os"))]
const DEBUG_SWIPE_COLOR: slint::Color = slint::Color::from_argb_u8(64, 0, 255, 0);

pub struct AppInput<PG: GuiAppGuiPermissions> {
    pub win: Rc<KeyOsWindow<PG>>,
    pub msg: InputMessage,
    pub envelope: Envelope,
}

impl<PG: GuiAppGuiPermissions> AppInput<PG> {
    pub fn new(win: Rc<KeyOsWindow<PG>>, msg: InputMessage, envelope: Envelope) -> Self {
        AppInput { win, msg, envelope }
    }
}

#[derive(Clone)]
pub struct AppContext<PG: GuiAppGuiPermissions, PF: GuiAppFsPermissions> {
    pub gui: Arc<GuiApi<PG>>,
    pub fs: Arc<fs::FileSystem<PF>>,

    pub router: StoredValue<crate::router::Router>,
    pub config: Rc<PlatformConfig>,

    handlers: AppHandlers<PG>,
}

impl<PG: GuiAppGuiPermissions, PF: GuiAppFsPermissions> AppContext<PG, PF> {
    pub fn new(gui: Arc<GuiApi<PG>>, fs: Arc<fs::FileSystem<PF>>) -> Self {
        Self {
            gui,
            fs,
            router: StoredValue::new(crate::router::Router::new()),
            config: Rc::new(Default::default()),
            handlers: AppHandlers::default(),
        }
    }

    pub fn set_input_handler(&self, input_handler: impl InputHandler<PG> + 'static) {
        let mut handler = self.handlers.input_handler.borrow_mut();
        let _ = handler.insert(Box::new(input_handler));
    }
}

#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub enable_swipe_back: Cell<bool>,
}

impl Default for PlatformConfig {
    fn default() -> Self { Self { enable_swipe_back: Cell::new(true) } }
}

pub trait InputHandler<PG: GuiAppGuiPermissions>: FnMut(AppInput<PG>) {}
impl<T, PG: GuiAppGuiPermissions> InputHandler<PG> for T where T: FnMut(AppInput<PG>) {}

pub trait ChildrenCrashHandler: FnMut(xous::PID, i32) {}
impl<T> ChildrenCrashHandler for T where T: FnMut(xous::PID, i32) {}

pub trait InputFocusHandler: FnMut(bool) {}
impl<T> InputFocusHandler for T where T: FnMut(bool) {}

#[derive(Default, Clone)]
struct AppHandlers<PG: GuiAppGuiPermissions> {
    input_handler: Rc<RefCell<Option<Box<dyn InputHandler<PG>>>>>,
}

pub trait GuiAppFsPermissions:
    server::CheckedPermissions
    + server::MessageAllowed<fs::messages::OpenDirMessage>
    + server::MessageAllowed<fs::messages::OpenFileMessage>
    + server::MessageAllowed<fs::messages::CloseFile>
    + server::MessageAllowed<fs::messages::CloseDir>
    + server::MessageAllowed<fs::messages::NextEntry>
    + server::MessageAllowed<fs::messages::ReadFile>
    + server::MessageAllowed<fs::messages::MapFileMessage>
{
}

impl<P> GuiAppFsPermissions for P
where
    P: server::CheckedPermissions,
    P: server::MessageAllowed<fs::messages::OpenDirMessage>,
    P: server::MessageAllowed<fs::messages::OpenFileMessage>,
    P: server::MessageAllowed<fs::messages::CloseFile>,
    P: server::MessageAllowed<fs::messages::CloseDir>,
    P: server::MessageAllowed<fs::messages::NextEntry>,
    P: server::MessageAllowed<fs::messages::ReadFile>,
    P: server::MessageAllowed<fs::messages::MapFileMessage>,
{
}

pub trait GuiAppGuiPermissions:
    server::CheckedPermissions
    + 'static
    + server::MessageAllowed<gui_server_api::msg::SubmitFrame>
    + server::MessageAllowed<gui_server_api::msg::UpdateKeyboard>
    + server::MessageAllowed<gui_server_api::msg::HideKeyboard>
    + server::MessageAllowed<gui_server_api::msg::AnimateNextFrame>
    + server::MessageAllowed<gui_server_api::msg::RequestRedraw>
{
}

impl<P> GuiAppGuiPermissions for P
where
    P: server::CheckedPermissions + 'static,
    P: server::MessageAllowed<gui_server_api::msg::SubmitFrame>,
    P: server::MessageAllowed<gui_server_api::msg::HideKeyboard>,
    P: server::MessageAllowed<gui_server_api::msg::UpdateKeyboard>,
    P: server::MessageAllowed<gui_server_api::msg::AnimateNextFrame>,
    P: server::MessageAllowed<gui_server_api::msg::RequestRedraw>,
{
}

pub struct KeyOsPlatform<const WIDTH: usize, const HEIGHT: usize, PG: GuiAppGuiPermissions> {
    start: Instant,
    state: RefCell<KeyOsEventLoopState<WIDTH, HEIGHT, PG>>,
}

impl<const WIDTH: usize, const HEIGHT: usize, PG: GuiAppGuiPermissions> KeyOsPlatform<WIDTH, HEIGHT, PG> {
    pub fn new<PF: GuiAppFsPermissions>(_app_title: &'static str, cx: AppContext<PG, PF>) -> Self {
        let window = KeyOsWindow::new(cx.gui.clone(), PhysicalSize::new(WIDTH as u32, HEIGHT as u32));

        crate::runtime::handle::global::init();
        crate::fonts::register_fonts(&cx.fs);

        let wake_callback = TicktimerCallback::new(cx.gui.sid()).unwrap();
        Self {
            start: Instant::now(),
            state: RefCell::new(KeyOsEventLoopState {
                window,
                gui: cx.gui,

                visible: false,
                wake_callback,

                router: cx.router,

                handlers: cx.handlers,
                config: cx.config.clone(),

                swipe_gesture_state: None,
                ticktimer: Ticktimer::default(),
                framebuffer: None,
            }),
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    pub fn subscribe_to_theme_changes<PS>(&self)
    where
        PS: server::CheckedPermissions,
        PS: server::MessageAllowed<settings::messages::SubscribeDebugTouch>,
    {
        crate::spawn_local({
            let window = Rc::downgrade(&self.state.borrow().window);
            async move {
                let mut sub = crate::subscribe_scalar::<PS, _>(settings::messages::SubscribeDebugTouch);
                while let Some(debug) = sub.next().await {
                    if let Some(window) = window.upgrade() {
                        let window = WindowInner::from_pub(window.window());
                        if debug.0 {
                            window.set_debug_touch(Some(DEBUG_TOUCH_COLOR));
                            window.set_debug_swipe(Some(DEBUG_SWIPE_COLOR));
                        } else {
                            window.set_debug_touch(None);
                            window.set_debug_swipe(None);
                        }
                    }
                }
            }
        })
        .detach();
    }
}

impl<const WIDTH: usize, const HEIGHT: usize, PG: GuiAppGuiPermissions> slint::platform::Platform
    for KeyOsPlatform<WIDTH, HEIGHT, PG>
{
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, PlatformError> {
        // Since on MCUs, there can be only one window, just return a clone of self.window.
        // We'll also use the same window in the event loop.
        Ok(self.state.borrow().window.clone())
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut state = self.state.borrow_mut();
        state.run();
        Ok(())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Runtime::unsafe_handle()))
    }

    fn duration_since_start(&self) -> Duration {
        let the_beginning = self.start;
        Instant::now() - the_beginning
    }

    fn debug_log(&self, arguments: std::fmt::Arguments) {
        log::debug!("{}", arguments);
    }
}

#[allow(dead_code)]
struct KeyOsEventLoopState<const WIDTH: usize, const HEIGHT: usize, PG: GuiAppGuiPermissions> {
    window: Rc<KeyOsWindow<PG>>,
    gui: Arc<GuiApi<PG>>,

    visible: bool,
    wake_callback: TicktimerCallback,

    router: StoredValue<crate::router::Router>,

    // if this proves to be a performance issue, we can use a raw pointer instead.
    handlers: AppHandlers<PG>,
    config: Rc<PlatformConfig>,

    // Tracks the time and position of the first touch for swipe detection.
    // The bool is true when the initial Press was consumed (needs to be replayed on swipe failure).
    swipe_gesture_state: Option<(Instant, Touch, bool)>,

    ticktimer: Ticktimer,
    framebuffer: Option<Framebuffer>,
}

impl<const WIDTH: usize, const HEIGHT: usize, PG: GuiAppGuiPermissions>
    KeyOsEventLoopState<WIDTH, HEIGHT, PG>
{
    pub fn run(&mut self) {
        let mut events = Vec::new();
        loop {
            if self.should_block() {
                let (event, msg) = self.gui.receive_input().unwrap();
                self.wake_callback.cancel(InputMessage::Noop as usize);
                self.process_input(event, msg, &mut events);
            }

            while let Some((event, msg)) = self.gui.try_receive_input() {
                self.process_input(event, msg, &mut events);
            }

            slint::platform::update_timers_and_animations();

            self.dispatch_events(&mut events);

            let status = Runtime::unsafe_run();
            if status == EventLoopStatus::Quit {
                break;
            }

            self.draw();
        }
        log::info!("Closing normally (received close request)");
    }

    fn should_block(&mut self) -> bool {
        const MIN_BLOCK_DURATION: Duration = Duration::from_millis(1);

        if !self.visible {
            // If we are not visible, just block until we become visible, and disregard any active timers.
            true
        } else if self.window.has_active_animations() {
            // Never block if we are animating, animate with max framerate
            false
        } else {
            let slint_timer_expiration = slint::platform::duration_until_next_timer_update();

            match slint_timer_expiration {
                // No timers active: block
                None => true,
                // Expired timers: don't block
                Some(duration) if duration < MIN_BLOCK_DURATION => false,
                Some(callback_after) => {
                    log::debug!("Requesting callback in {callback_after:?}");
                    self.wake_callback.request(
                        callback_after.as_millis() as usize,
                        InputMessage::Noop as usize,
                        0,
                    );
                    true
                }
            }
        }
    }

    fn process_input(&mut self, event: InputMessage, msg: Envelope, events: &mut Vec<WindowEvent>) {
        match event {
            InputMessage::Touch => {
                if let Some(touch) = Touch::try_from_input_message(&msg.body) {
                    let button = PointerEventButton::Left;

                    if self.config.enable_swipe_back.get() {
                        let can_go_back = self.router.with(|r| r.has_back());
                        if can_go_back {
                            match self.handle_swipe_right(&touch) {
                                SwipeResult::Consumed => return,
                                SwipeResult::Passthrough => {}
                                SwipeResult::ReplayPress(stored_touch) => {
                                    let position =
                                        PhysicalPosition::new(stored_touch.x as i32, stored_touch.y as i32)
                                            .to_logical(self.window.scale_factor());
                                    events.push(WindowEvent::PointerPressed { position, button });
                                }
                            }
                        }
                    }

                    let position = PhysicalPosition::new(touch.x as i32, touch.y as i32)
                        .to_logical(self.window.scale_factor());
                    events.push(match touch.kind {
                        TouchKind::Press => WindowEvent::PointerPressed { position, button },
                        TouchKind::Drag => WindowEvent::PointerMoved { position },
                        TouchKind::Release => WindowEvent::PointerReleased { position, button },
                    });
                    if matches!(touch.kind, TouchKind::Release) {
                        events.push(WindowEvent::PointerExited);
                    }
                }
            }

            InputMessage::KeyPress => events.push(self.handle_key_event_msg(true, &msg)),
            InputMessage::KeyRelease => events.push(self.handle_key_event_msg(false, &msg)),
            InputMessage::Scroll => {
                let scalar = msg.body.scalar_message().expect("scalar message");
                // x/y are already in logical (device-space) pixels — use them directly.
                let x = scalar.arg1 as i32;
                let y = scalar.arg2 as i32;
                // delta_x/delta_y are already in logical pixels (normalised in the emulator).
                let delta_x = f32::from_bits(scalar.arg3 as u32);
                let delta_y = f32::from_bits(scalar.arg4 as u32);
                let position = LogicalPosition::new(x as f32, y as f32);
                events.push(WindowEvent::PointerScrolled { position, delta_x, delta_y });
            }
            InputMessage::Visible => {
                log::debug!("App is now visible");
                self.visible = true;
            }
            InputMessage::Hidden => {
                log::debug!("App is hidden");
                self.visible = false;
            }
            InputMessage::CloseRequested => Runtime::unsafe_quit(),

            // do nothing but allow the event loop to run
            InputMessage::Noop => {}

            InputMessage::FrameBuffer => {
                self.framebuffer = Framebuffer::from_message(msg.take_message());
                return;
            }
            _ => (),
        }
        if let Some(handler) = self.handlers.input_handler.borrow_mut().as_mut() {
            handler(AppInput::new(self.window.clone(), event, msg));
        }
    }

    fn dispatch_events(&self, events: &mut Vec<WindowEvent>) {
        let mut event_it = events.drain(..).peekable();
        while let Some(event) = event_it.next() {
            // Only apply the last PointerMoved event if there are multiple consecutive ones. All
            // other ones would just be wasted calculation, as just the move is
            // rarely actionable, but slint does a surprisingly large amount of
            // calculations for each of these updates.
            if matches!(event, WindowEvent::PointerMoved { .. })
                && event_it.peek().map_or(false, |ne| matches!(ne, WindowEvent::PointerMoved { .. }))
            {
                continue;
            }
            self.window.dispatch_event(event);
        }
    }

    fn draw(&mut self) {
        let Some(framebuffer) = self.framebuffer.take() else { return };
        if framebuffer.is_new {
            let mut dirty = DirtyRegion::default();
            dirty.add_rect(euclid::Rect::from_size(euclid::Size2D::new(16000.0, 16000.0)));
            self.window.renderer.mark_dirty_region(dirty);
        }
        let last_swap = framebuffer.last_swap;
        let mut work_fb = framebuffer.leak();
        self.window.draw(LineProvider::<WIDTH> {
            work_fb: work_fb.as_slice_mut(),
            last_swap,
            next_timer_check: 0,
            ticktimer: &self.ticktimer,
        });
        #[cfg(keyos)]
        xous::syscall::flush_cache(work_fb, xous::CacheOperation::Clean).expect("clean cache");
        self.gui.submit_frame(work_fb).unwrap();
    }

    fn handle_key_event_msg(&self, is_press: bool, msg: &Envelope) -> WindowEvent {
        let scalar = msg.body.scalar_message().expect("scalar message");
        let key = Key::from_scalar([scalar.arg1 as u32, scalar.arg2 as u32]);

        let key: SharedString = match key {
            Key::Char(c) => (char::from_u32(c as u32).unwrap_or('?')).into(),
            Key::Backspace => slint::platform::Key::Backspace.into(),
            Key::Delete => slint::platform::Key::Delete.into(),
            Key::CursorLeft => slint::platform::Key::LeftArrow.into(),
            Key::CursorRight => slint::platform::Key::RightArrow.into(),
            Key::Enter => slint::platform::Key::Return.into(),
            Key::Tab => slint::platform::Key::Tab.into(),
        };

        if is_press {
            WindowEvent::KeyPressed { text: key }
        } else {
            WindowEvent::KeyReleased { text: key }
        }
    }

    /// Detects and handles the swipe right gesture that navigates the user back with the `Router`.
    fn handle_swipe_right(&mut self, touch: &Touch) -> SwipeResult {
        match touch.kind {
            TouchKind::Press if touch.x <= SWIPE_RIGHT_EDGE_AREA_WIDTH_PX => {
                log::debug!("Detected initial swipe right touch at ({}, {})", touch.x, touch.y);
                // Consume the press so we can replay it later if the gesture doesn't complete.
                self.swipe_gesture_state = Some((Instant::now(), touch.clone(), true));
                SwipeResult::Consumed
            }

            TouchKind::Drag => {
                if self.swipe_gesture_state.is_none() {
                    if touch.x <= SWIPE_RIGHT_EDGE_AREA_WIDTH_PX {
                        log::debug!("Detected initial swipe right drag at ({}, {})", touch.x, touch.y);
                        // The Press that started this touch was outside the edge zone and already
                        // forwarded to Slint, so no replay is needed on failure.
                        self.swipe_gesture_state = Some((Instant::now(), touch.clone(), false));
                        return SwipeResult::Consumed;
                    }
                } else {
                    return SwipeResult::Consumed;
                }

                SwipeResult::Passthrough
            }

            TouchKind::Release => {
                if let Some((first_touch_time, first_touch, press_consumed)) = self.swipe_gesture_state.take()
                {
                    let elapsed = first_touch_time.elapsed().as_secs_f32();
                    let (dx, _dy) = touch.diff(&first_touch);
                    let velocity = if elapsed != 0. { dx as f32 / elapsed } else { 0. };
                    log::debug!("Swipe right gesture velocity: {velocity} and dx: {dx}");

                    if velocity >= SWIPE_RIGHT_VELOCITY_THRESHOLD && dx >= SWIPE_RIGHT_DISTANCE_THRESHOLD {
                        log::debug!("Detected, navigating backward");
                        self.gui.animate_next_frame(NextFrameAnimationKind::SlideOutRight).ok();
                        self.router.with(|r| r.navigate_backward());
                        return SwipeResult::Consumed;
                    }

                    if press_consumed {
                        return SwipeResult::ReplayPress(first_touch);
                    }
                }

                SwipeResult::Passthrough
            }

            _ => {
                self.swipe_gesture_state = None;
                SwipeResult::Passthrough
            }
        }
    }
}

enum SwipeResult {
    /// The touch was consumed by the swipe handler; do not propagate it.
    Consumed,
    /// The touch was not consumed; propagate it normally.
    Passthrough,
    /// The gesture ended without a swipe and the initial Press was consumed. Re-emit the stored
    /// touch as a `PointerPressed` before propagating the current Release.
    ReplayPress(Touch),
}

struct Framebuffer {
    buffer: DropDeallocate,
    last_swap: usize,
    is_new: bool,
}

impl Framebuffer {
    pub fn from_message(msg: xous::Message) -> Option<Self> {
        let mem = msg.memory_message()?;
        Some(Self {
            buffer: DropDeallocate::new(mem.buf),
            last_swap: mem.offset.map(|o| o.get()).unwrap_or(0),
            is_new: mem.valid.is_some(),
        })
    }

    pub fn leak(self) -> MemoryRange { self.buffer.leak() }
}

struct LineProvider<'a, const WIDTH: usize> {
    work_fb: &'a mut [KeyosPixel],
    last_swap: usize,
    next_timer_check: usize,
    ticktimer: &'a Ticktimer,
}

impl<const WIDTH: usize> LineBufferProvider for LineProvider<'_, WIDTH> {
    type TargetPixel = KeyosPixel;

    fn process_line(
        &mut self,
        line: usize,
        range: std::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [KeyosPixel]),
    ) {
        // Don't check the timer too often, as it takes 0.1ms each time.
        const TIMER_CHECK_INTERVAL: usize = 100;
        if line >= self.next_timer_check {
            let time_of_last_line = 1000 * (line + TIMER_CHECK_INTERVAL) / (FPS * SCREEN_HEIGHT);
            let lcdc_estimated_render_tick = self.last_swap + time_of_last_line;
            let current_tick = self.ticktimer.elapsed_ms() as usize;
            // If we would overtake the LCDC line scan, wait a bit instead.
            if lcdc_estimated_render_tick > current_tick {
                let diff = lcdc_estimated_render_tick - current_tick;
                // If the difference is too large, we probably wrapped on 32 bit,
                // or there was some other issue with the calculation.
                // In this case just let it glitch visually. It will resolve in 1-2 frames.
                if diff < 100 {
                    log::trace!("Sleeping {diff} on line {line}");
                    std::thread::sleep(Duration::from_millis(diff as u64));
                }
            }
            self.next_timer_check = line + TIMER_CHECK_INTERVAL;
        }
        render_fn(&mut self.work_fb[line * WIDTH..][range]);
    }
}
