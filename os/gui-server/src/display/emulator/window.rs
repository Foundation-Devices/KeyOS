// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    super::VIRTUAL_VSYNC_EVENTS,
    crate::display::{
        draw::draw_whole_device,
        emulator::{
            consts::{
                TOUCH_AREA_H, TOUCH_AREA_W, TOUCH_AREA_X, TOUCH_AREA_Y, VIRT_HOME_BUTTON_HEIGHT,
                VIRT_HOME_BUTTON_WIDTH, VIRT_HOME_BUTTON_X, VIRT_HOME_BUTTON_Y, VIRT_PWR_BUTTON_HEIGHT,
                VIRT_PWR_BUTTON_WIDTH, VIRT_PWR_BUTTON_X, VIRT_PWR_BUTTON_Y,
            },
            virtbuttons::translate_virt_button_coords,
        },
        PlatformDisplay,
    },
    gui_server_api::{
        consts::{DEVICE_HEIGHT, DEVICE_WIDTH},
        touch::{Touch, TouchKind},
    },
    image::ImageBuffer,
    std::{
        num::NonZeroU32,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    },
    winit::{
        application::ApplicationHandler,
        dpi::{LogicalPosition, PhysicalPosition, PhysicalSize},
        event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
        event_loop::EventLoop,
        keyboard::{Key as WinitKey, NamedKey},
        window::{Window, WindowButtons, WindowLevel},
    },
};

#[derive(Debug, Default, Clone)]
struct AllSimulatorPermissions;
impl server::CheckedPermissions for AllSimulatorPermissions {
    const NAME: &str = "os/gui-server";
}
impl<T> server::MessageAllowed<T> for AllSimulatorPermissions {}

type HostedSimulatorApi = gui_server_api::simulator::SimulatorApi<AllSimulatorPermissions>;

struct EmulatorApp {
    simulator_api: HostedSimulatorApi,
    capture_api: gui_server_api::GuiApiLight<AllSimulatorPermissions>,
    window: Option<Arc<Window>>,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
    last_cursor_pos: LogicalPosition<f64>,
    last_window_pos: Option<PhysicalPosition<i32>>,
    last_pressed: bool,
    resizer: fast_image_resize::Resizer,
    surface: Option<softbuffer::Surface>,
    raised: bool,
    // Set when focus arrives; read by the grace thread.
    focus_seen: Arc<AtomicBool>,
}

impl EmulatorApp {
    fn send_simulator_event(
        &self,
        action: &str,
        f: impl FnOnce(&HostedSimulatorApi) -> Result<(), gui_server_api::GuiServerError>,
    ) {
        let result = f(&self.simulator_api);
        if let Err(err) = result {
            log::warn!("Failed to {action}: {err:?}");
        }
    }

    fn send_capture_event(
        &self,
        action: &str,
        f: impl FnOnce(
            &gui_server_api::GuiApiLight<AllSimulatorPermissions>,
        ) -> Result<(), gui_server_api::GuiServerError>,
    ) {
        let result = f(&self.capture_api);
        if let Err(err) = result {
            log::warn!("Failed to {action}: {err:?}");
        }
    }

    fn update_scale_factor(&mut self) {
        let new_scale_factor = PlatformDisplay::scale_factor();
        if self.scale_factor == new_scale_factor {
            return;
        };
        self.window_size = PhysicalSize::new(
            (DEVICE_WIDTH as f64 * new_scale_factor) as u32,
            (DEVICE_HEIGHT as f64 * new_scale_factor) as u32,
        );
        self.scale_factor = new_scale_factor;
        let Some(surface) = &mut self.surface else { return };
        let (Some(width), Some(height)) =
            (NonZeroU32::new(self.window_size.width), NonZeroU32::new(self.window_size.height))
        else {
            log::warn!("Skipping hosted simulator resize for zero-sized window: {:?}", self.window_size);
            return;
        };

        if let Err(err) = surface.resize(width, height) {
            log::warn!("Failed to resize hosted simulator surface: {err:?}");
        }
    }
}

impl ApplicationHandler for EmulatorApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        println!("Resuming");
        self.update_scale_factor();
        let mut attributes = Window::default_attributes()
            .with_transparent(true)
            .with_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE)
            .with_resizable(false)
            .with_inner_size(self.window_size)
            .with_title("Passport Simulator");
        // macOS ignores a post-creation set_outer_position() here, so restore at creation.
        if let Some(pos) = read_window_pos() {
            attributes = attributes.with_position(pos);
        }
        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.window = Some(window.clone());
        // Track the inner (content) position, not the outer frame: winit applies
        // the creation-time position to the content rect, so saving/restoring the
        // outer position would shift the window up by the title bar height each
        // time. Reading the content position keeps save and restore symmetric.
        self.last_window_pos = window.inner_position().ok();

        let context = unsafe { softbuffer::Context::new(&*window) }.unwrap();
        let mut surface = unsafe { softbuffer::Surface::new(&context, &*window) }.unwrap();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(self.window_size.width), NonZeroU32::new(self.window_size.height))
        else {
            log::warn!(
                "Skipping hosted simulator surface init for zero-sized window: {:?}",
                self.window_size
            );
            return;
        };
        if let Err(err) = surface.resize(width, height) {
            log::warn!("Failed to initialize hosted simulator surface size: {err:?}");
            return;
        }
        self.surface = Some(surface);
        // A new surface has never been painted; repaint it even when static.
        PlatformDisplay::mark_display_dirty();

        // Register the hook only once a surface exists, or else a redraw with no
        // surface clears the dirty gate and leaves the screen stale.
        VIRTUAL_VSYNC_EVENTS.lock().unwrap().push(Box::new({
            let window = window.clone();
            move || {
                if crate::display::PlatformDisplay::take_display_dirty() {
                    window.request_redraw()
                }
            }
        }));
    }

    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        self.update_scale_factor();
        let Some(window) = &self.window else { return };
        if window.inner_size() != self.window_size {
            let _ = window.request_inner_size(self.window_size);
        }
        // Wayland doesn't support retrieving window location nor setting it.
        if let Some(pos) = self.last_window_pos {
            // Save new window position if it's changed
            let Ok(curr_window_pos) = window.inner_position() else {
                log::warn!("Failed to read hosted simulator window position");
                return;
            };
            if pos != curr_window_pos {
                self.last_window_pos = Some(curr_window_pos);
                save_window_pos(curr_window_pos);
            }
        }

        match event {
            WindowEvent::RedrawRequested => {
                if !self.raised {
                    self.raised = true;
                    // First frame: winit focus is a no-op until the window is on
                    // screen, and macOS throttles programmatic activation, so pin it
                    // above others. Skip it when focus already arrived, or else the
                    // window floats over the user's other windows for the grace period.
                    if !self.focus_seen.load(Ordering::Relaxed) {
                        window.focus_window();
                        window.set_window_level(WindowLevel::AlwaysOnTop);
                        std::thread::spawn({
                            let window = window.clone();
                            let focus_seen = Arc::clone(&self.focus_seen);
                            move || {
                                std::thread::sleep(Duration::from_secs(2));
                                window.set_window_level(WindowLevel::Normal);
                                // Re-assert front order only if focus never arrived,
                                // or else it steals focus from whatever the user
                                // switched to.
                                if !focus_seen.load(Ordering::Relaxed) {
                                    window.focus_window();
                                }
                            }
                        });
                    }
                }
                //let measure = std::time::Instant::now();
                let mut image_buffer = ImageBuffer::new(DEVICE_WIDTH, DEVICE_HEIGHT);
                draw_whole_device(&mut image_buffer);

                let Ok(src) = fast_image_resize::images::Image::from_vec_u8(
                    DEVICE_WIDTH,
                    DEVICE_HEIGHT,
                    image_buffer.into_vec(),
                    fast_image_resize::PixelType::U8x4,
                ) else {
                    log::warn!("Failed to create hosted simulator source frame buffer");
                    PlatformDisplay::mark_display_dirty();
                    return;
                };
                let Some(surface) = &mut self.surface else { return };
                let Ok(mut surface_buffer) = surface.buffer_mut() else {
                    log::warn!("Failed to acquire hosted simulator surface buffer");
                    PlatformDisplay::mark_display_dirty();
                    return;
                };
                let Ok(mut dst) = fast_image_resize::images::Image::from_slice_u8(
                    self.window_size.width,
                    self.window_size.height,
                    bytemuck::cast_slice_mut(&mut surface_buffer),
                    fast_image_resize::PixelType::U8x4,
                ) else {
                    log::warn!("Failed to create hosted simulator destination frame buffer");
                    PlatformDisplay::mark_display_dirty();
                    return;
                };

                let resize_opts =
                    fast_image_resize::ResizeOptions::new().resize_alg(fast_image_resize::ResizeAlg::Nearest);
                self.resizer.resize(&src, &mut dst, Some(&resize_opts)).ok();
                image_swizzle::bgra_to_rgba_inplace(bytemuck::cast_slice_mut(&mut surface_buffer));
                //println!("Frame time: {:?}", measure.elapsed());
                if let Err(err) = surface_buffer.present() {
                    log::warn!("Failed to present hosted simulator frame: {err:?}");
                    // Re-arm, or else a static screen never retries the paint.
                    PlatformDisplay::mark_display_dirty();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor_pos = position.to_logical(self.scale_factor);

                let is_in_touch_screen = is_within_area(
                    self.last_cursor_pos,
                    TOUCH_AREA_X,
                    TOUCH_AREA_Y,
                    TOUCH_AREA_W,
                    TOUCH_AREA_H,
                );
                let is_in_home_button = is_within_area(
                    self.last_cursor_pos,
                    VIRT_HOME_BUTTON_X,
                    VIRT_HOME_BUTTON_Y,
                    VIRT_HOME_BUTTON_WIDTH,
                    VIRT_HOME_BUTTON_HEIGHT,
                );

                // If dragged inside the screen area while pressed, send the event to the touch
                // server
                if self.last_pressed && (is_in_touch_screen || is_in_home_button) {
                    let mut x = (self.last_cursor_pos.x as usize - TOUCH_AREA_X) as u16;
                    let mut y = (self.last_cursor_pos.y as usize - TOUCH_AREA_Y) as u16;

                    if is_in_home_button {
                        // Additionally translate the touch coordinates when pressed within the
                        // virtual home button area to the physical coordinates
                        (x, y) = translate_virt_button_coords(x, y);
                    }

                    let touch = Touch { kind: TouchKind::Drag, id: 0, x: x as usize, y: y as usize };
                    self.send_capture_event("send hosted simulator drag touch", |capture_api| {
                        capture_api.inject_touch(touch)
                    });
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                let is_within_touch_area = is_within_area(
                    self.last_cursor_pos,
                    TOUCH_AREA_X,
                    TOUCH_AREA_Y,
                    TOUCH_AREA_W,
                    TOUCH_AREA_H,
                );
                let is_in_home_button = is_within_area(
                    self.last_cursor_pos,
                    VIRT_HOME_BUTTON_X,
                    VIRT_HOME_BUTTON_Y,
                    VIRT_HOME_BUTTON_WIDTH,
                    VIRT_HOME_BUTTON_HEIGHT,
                );
                self.last_pressed = matches!(state, ElementState::Pressed);

                let is_in_pwr_button = is_within_area(
                    self.last_cursor_pos,
                    VIRT_PWR_BUTTON_X,
                    VIRT_PWR_BUTTON_Y,
                    VIRT_PWR_BUTTON_WIDTH,
                    VIRT_PWR_BUTTON_HEIGHT,
                );

                let is_dragging_window =
                    self.last_pressed && !is_in_pwr_button && !is_within_touch_area && !is_in_home_button;
                if is_dragging_window {
                    start_window_drag(window);
                }

                // Send virtual power button press and release events to the button server
                if self.last_pressed && is_in_pwr_button {
                    self.send_simulator_event("simulate hosted power button press", |simulator_api| {
                        simulator_api.simulate_power_button(true)
                    });
                } else if is_in_pwr_button {
                    self.send_simulator_event("simulate hosted power button release", |simulator_api| {
                        simulator_api.simulate_power_button(false)
                    });
                }

                let is_in_home_button = is_within_area(
                    self.last_cursor_pos,
                    VIRT_HOME_BUTTON_X,
                    VIRT_HOME_BUTTON_Y,
                    VIRT_HOME_BUTTON_WIDTH,
                    VIRT_HOME_BUTTON_HEIGHT,
                );

                // If pressed/released inside the screen send the event to the touch server
                if is_within_touch_area || is_in_home_button {
                    let mut x = (self.last_cursor_pos.x as usize - TOUCH_AREA_X) as u16;
                    let mut y = (self.last_cursor_pos.y as usize - TOUCH_AREA_Y) as u16;

                    if is_in_home_button {
                        // Additionally translate the touch coordinates when pressed within the
                        // virtual home button area to the physical coordinates
                        (x, y) = translate_virt_button_coords(x, y);
                    }

                    let kind = if self.last_pressed { TouchKind::Press } else { TouchKind::Release };
                    let touch = Touch { kind, id: 0, x: x as usize, y: y as usize };

                    self.send_capture_event("send hosted simulator touch", |capture_api| {
                        capture_api.inject_touch(touch)
                    });
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if is_within_area(
                    self.last_cursor_pos,
                    TOUCH_AREA_X,
                    TOUCH_AREA_Y,
                    TOUCH_AREA_W,
                    TOUCH_AREA_H,
                ) {
                    // Both deltas are normalised to logical (device-space) units so they match
                    // the x/y position coordinates (which come from last_cursor_pos, already
                    // converted to logical via to_logical(scale_factor)).
                    let (delta_x, delta_y) = match delta {
                        // LineDelta is already scale-independent (line counts); multiply by a
                        // fixed pixel-per-line constant to get logical pixels.
                        MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
                        // PixelDelta is in physical pixels — divide by scale_factor to get
                        // logical pixels matching the rest of the coordinate system.
                        MouseScrollDelta::PixelDelta(pos) => {
                            let sf = self.scale_factor as f32;
                            (pos.x as f32 / sf, pos.y as f32 / sf)
                        }
                    };
                    let x = (self.last_cursor_pos.x as u32).saturating_sub(TOUCH_AREA_X as u32);
                    let y = (self.last_cursor_pos.y as u32).saturating_sub(TOUCH_AREA_Y as u32);
                    self.send_simulator_event("simulate hosted scroll", |simulator_api| {
                        simulator_api.simulate_scroll(x, y, delta_x, delta_y)
                    });
                }
            }

            WindowEvent::CloseRequested => {
                // Shutting gui-server down terminates every hosted process, so closing
                // the device screen also closes the control panel.
                self.capture_api.shutdown().ok();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state == ElementState::Pressed;
                let key = match &event.logical_key {
                    WinitKey::Named(NamedKey::Backspace) => Some(gui_server_api::Key::Backspace),
                    WinitKey::Named(NamedKey::Delete) => Some(gui_server_api::Key::Delete),
                    WinitKey::Named(NamedKey::ArrowLeft) => Some(gui_server_api::Key::CursorLeft),
                    WinitKey::Named(NamedKey::ArrowRight) => Some(gui_server_api::Key::CursorRight),
                    WinitKey::Named(NamedKey::Enter) => Some(gui_server_api::Key::Enter),
                    WinitKey::Named(NamedKey::Tab) => Some(gui_server_api::Key::Tab),
                    WinitKey::Named(NamedKey::Space) => Some(gui_server_api::Key::Char(' ' as usize)),
                    WinitKey::Character(s) => s.chars().next().map(|c| gui_server_api::Key::Char(c as usize)),
                    _ => None,
                };
                if let Some(key) = key {
                    self.send_simulator_event("simulate hosted key input", |simulator_api| {
                        simulator_api.simulate_key(key, is_pressed)
                    });
                }
            }

            WindowEvent::Focused(true) => {
                // Focus arrived: record it and drop the pin (a no-op if unpinned).
                self.focus_seen.store(true, Ordering::Relaxed);
                window.set_window_level(WindowLevel::Normal);
            }

            _ => (),
        }
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        // Cmd-Q can exit the loop without a per-window CloseRequested; shut gui-server
        // down here too so the control panel goes with it.
        self.capture_api.shutdown().ok();
    }
}

pub(crate) fn run_window() {
    let event_loop = EventLoop::new().unwrap();

    // gui-server runs on a sibling thread (see main) and registers its name
    // independently, so block until it is up rather than timing out: the window is
    // useless without it.
    let simulator_api = HostedSimulatorApi::connect();
    let capture_api = gui_server_api::GuiApiLight::connect();

    let mut app = EmulatorApp {
        simulator_api,
        capture_api,
        window: None,
        window_size: PhysicalSize::new(DEVICE_WIDTH, DEVICE_HEIGHT),
        scale_factor: 1.0,
        last_cursor_pos: Default::default(),
        last_window_pos: Default::default(),
        last_pressed: false,
        resizer: fast_image_resize::Resizer::new(),
        surface: None,
        raised: false,
        focus_seen: Arc::new(AtomicBool::new(false)),
    };
    event_loop.run_app(&mut app).unwrap();
    // Backstop if the loop exits without `exiting` firing: shut gui-server down so
    // the control panel goes with it.
    app.capture_api.shutdown().ok();
}

fn is_within_area(pos: LogicalPosition<f64>, x: usize, y: usize, w: usize, h: usize) -> bool {
    let outside_x = pos.x < x as f64 || pos.x > x as f64 + w as f64;
    let outside_y = pos.y < y as f64 || pos.y > y as f64 + h as f64;

    !(outside_x || outside_y)
}

fn start_window_drag(window: &Window) {
    if let Err(err) = window.drag_window() {
        log::warn!("Failed to drag hosted simulator window: {err:?}");
    }
}

fn save_window_pos(pos: PhysicalPosition<i32>) {
    let str = format!("{}\n{}", pos.x, pos.y);
    if let Err(err) = std::fs::write(".last_pos", str) {
        log::warn!("Failed to save hosted simulator window position: {err}");
    }
}

fn read_window_pos() -> Option<PhysicalPosition<i32>> {
    let contents = std::fs::read_to_string(".last_pos").ok()?;
    let mut lines = contents.lines();
    let x = lines.next()?.parse::<i32>().ok()?;
    let y = lines.next()?.parse::<i32>().ok()?;
    Some(PhysicalPosition::new(x, y))
}
