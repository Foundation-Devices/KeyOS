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
    std::{num::NonZeroU32, sync::Arc},
    winit::{
        application::ApplicationHandler,
        dpi::{LogicalPosition, PhysicalPosition, PhysicalSize},
        event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
        event_loop::EventLoop,
        keyboard::{Key as WinitKey, NamedKey},
        window::{Window, WindowButtons},
    },
};

#[derive(Debug, Default, Clone)]
struct AllSimulatorPermissions;
impl server::CheckedPermissions for AllSimulatorPermissions {
    const NAME: &str = "os/gui-server";
}
impl<T> server::MessageAllowed<T> for AllSimulatorPermissions {}

struct EmulatorApp {
    simulator_api: gui_server_api::simulator::SimulatorApi<AllSimulatorPermissions>,
    capture_api: gui_server_api::GuiApiLight<AllSimulatorPermissions>,
    window: Option<Arc<Window>>,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
    last_cursor_pos: LogicalPosition<f64>,
    last_window_pos: Option<PhysicalPosition<i32>>,
    last_pressed: bool,
    resizer: fast_image_resize::Resizer,
    surface: Option<softbuffer::Surface>,
}

impl EmulatorApp {
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
        surface
            .resize(
                NonZeroU32::new(self.window_size.width).unwrap(),
                NonZeroU32::new(self.window_size.height).unwrap(),
            )
            .unwrap();
    }
}

impl ApplicationHandler for EmulatorApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        println!("Resuming");
        self.update_scale_factor();
        let attributes = Window::default_attributes()
            .with_transparent(true)
            .with_enabled_buttons(WindowButtons::MINIMIZE)
            .with_resizable(false)
            .with_inner_size(self.window_size)
            .with_title("Passport Prime");
        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.window = Some(window.clone());
        if let Some(pos) = read_window_pos() {
            window.set_outer_position(pos);
        }
        self.last_window_pos = window.outer_position().ok();
        VIRTUAL_VSYNC_EVENTS.lock().unwrap().push(Box::new({
            let window = window.clone();
            move || window.request_redraw()
        }));

        let context = unsafe { softbuffer::Context::new(&*window) }.unwrap();
        let mut surface = unsafe { softbuffer::Surface::new(&context, &*window) }.unwrap();
        surface
            .resize(
                NonZeroU32::new(self.window_size.width).unwrap(),
                NonZeroU32::new(self.window_size.height).unwrap(),
            )
            .unwrap();
        self.surface = Some(surface);
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
            let curr_window_pos = window.outer_position().expect("window pos");
            if pos != curr_window_pos {
                self.last_window_pos = Some(curr_window_pos);
                save_window_pos(pos);
            }
        }

        match event {
            WindowEvent::RedrawRequested => {
                //let measure = std::time::Instant::now();
                let mut image_buffer = ImageBuffer::new(DEVICE_WIDTH, DEVICE_HEIGHT);
                draw_whole_device(&mut image_buffer);

                let src = fast_image_resize::images::Image::from_vec_u8(
                    DEVICE_WIDTH,
                    DEVICE_HEIGHT,
                    image_buffer.into_vec(),
                    fast_image_resize::PixelType::U8x4,
                )
                .unwrap();
                let Some(surface) = &mut self.surface else { return };
                let mut surface_buffer = surface.buffer_mut().unwrap();
                let mut dst = fast_image_resize::images::Image::from_slice_u8(
                    self.window_size.width,
                    self.window_size.height,
                    bytemuck::cast_slice_mut(&mut surface_buffer),
                    fast_image_resize::PixelType::U8x4,
                )
                .unwrap();

                let resize_opts =
                    fast_image_resize::ResizeOptions::new().resize_alg(fast_image_resize::ResizeAlg::Nearest);
                self.resizer.resize(&src, &mut dst, Some(&resize_opts)).ok();
                image_swizzle::bgra_to_rgba_inplace(bytemuck::cast_slice_mut(&mut surface_buffer));
                //println!("Frame time: {:?}", measure.elapsed());
                surface_buffer.present().unwrap();
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
                    self.capture_api.inject_touch(touch).expect("send touch");
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
                    window.drag_window().unwrap();
                }

                // Send virtual power button press and release events to the button server
                if self.last_pressed && is_in_pwr_button {
                    self.simulator_api.simulate_power_button(true).expect("simulate power button press");
                } else if is_in_pwr_button {
                    self.simulator_api.simulate_power_button(false).expect("simulate power button press");
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

                    self.capture_api.inject_touch(touch).expect("send touch");
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
                    self.simulator_api.simulate_scroll(x, y, delta_x, delta_y).expect("simulate scroll");
                }
            }

            WindowEvent::CloseRequested => {
                gui_server_api::GuiApiLight::<AllSimulatorPermissions>::default().shutdown().ok();
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
                    self.simulator_api.simulate_key(key, is_pressed).expect("simulate key");
                }
            }

            _ => (),
        }
    }
}

pub(crate) fn run_window() {
    let event_loop = EventLoop::new().unwrap();

    let mut app = EmulatorApp {
        simulator_api: Default::default(),
        capture_api: Default::default(),
        window: None,
        window_size: PhysicalSize::new(DEVICE_WIDTH, DEVICE_HEIGHT),
        scale_factor: 1.0,
        last_cursor_pos: Default::default(),
        last_window_pos: Default::default(),
        last_pressed: false,
        resizer: fast_image_resize::Resizer::new(),
        surface: None,
    };
    event_loop.run_app(&mut app).unwrap();
}

fn is_within_area(pos: LogicalPosition<f64>, x: usize, y: usize, w: usize, h: usize) -> bool {
    let outside_x = pos.x < x as f64 || pos.x > x as f64 + w as f64;
    let outside_y = pos.y < y as f64 || pos.y > y as f64 + h as f64;

    !(outside_x || outside_y)
}

fn save_window_pos(pos: PhysicalPosition<i32>) {
    let str = format!("{}\n{}", pos.x, pos.y);
    std::fs::write(".last_pos", str).expect("save .last_pos");
}

fn read_window_pos() -> Option<PhysicalPosition<i32>> {
    std::fs::read_to_string(".last_pos").ok().and_then(|s| {
        let lines = s.lines().collect::<Vec<_>>();
        if lines.len() < 2 {
            return None;
        }
        let x = lines[0].parse::<i32>().ok()?;
        let y = lines[1].parse::<i32>().ok()?;
        Some(PhysicalPosition::new(x, y))
    })
}
