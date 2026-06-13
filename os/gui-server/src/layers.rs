// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use gui_server_api::consts::{CONTROL_CENTER_HEIGHT_EXPANDED_PX, SCREEN_HEIGHT, SCREEN_WIDTH};
use xous::MemoryRange;

use crate::{control_center::ControlCenterWindowState, display::MAX_LAYERS, AppWindow, Gui};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    src: SourceType,
    src_width: usize,
    #[allow(dead_code)]
    src_height: usize,
    crop_x: usize,
    crop_y: usize,
    crop_width: usize,
    crop_height: usize,
    dst_x: usize,
    dst_y: usize,
    dst_width: usize,
    dst_height: usize,
    pixel_format: LayerPixelFormat,
    alpha: u8,
    low_priority: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceType {
    /// DMA range plus physical address on hardware
    Dma {
        #[cfg(keyos)]
        phys: usize,
        range: MemoryRange,
    },
    Color {
        r: u8,
        g: u8,
        b: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerPixelFormat {
    Argb8888,
    #[cfg(keyos)]
    #[allow(dead_code)]
    Rgb565,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct LayerStack {
    pub layers: [Option<Layer>; MAX_LAYERS],
}

impl Layer {
    pub fn new(src: MemoryRange, src_width: usize, src_height: usize) -> Self {
        Self::new_with_pixel_format(src, src_width, src_height, LayerPixelFormat::Argb8888)
    }

    pub fn new_with_pixel_format(
        src: MemoryRange,
        src_width: usize,
        src_height: usize,
        pixel_format: LayerPixelFormat,
    ) -> Self {
        let Some(required_len) = source_len_for_dimensions(src_width, src_height, pixel_format) else {
            log::error!("Layer source dimensions overflow: {src_width}x{src_height} {pixel_format:?}");
            return Self::new_single_color(0, 0, 0, src_width, src_height);
        };
        if src.len() < required_len {
            log::error!(
                "Layer source range too short: actual={}, expected={required_len}, dimensions={src_width}x{src_height}, pixel_format={pixel_format:?}",
                src.len()
            );
            return Self::new_single_color(0, 0, 0, src_width, src_height);
        }

        #[cfg(keyos)]
        let phys = xous::virt_to_phys(src.as_ptr() as usize).unwrap();
        Self {
            src: SourceType::Dma {
                #[cfg(keyos)]
                phys,
                range: src,
            },
            src_width,
            src_height,
            crop_x: 0,
            crop_y: 0,
            crop_width: src_width,
            crop_height: src_height,
            dst_x: 0,
            dst_y: 0,
            dst_width: src_width,
            dst_height: src_height,
            pixel_format,
            alpha: 255,
            low_priority: false,
        }
    }

    pub fn new_window(src: &AppWindow, src_width: usize, src_height: usize) -> Self {
        if let Some(buffer) = src.buffers.most_recent_buffer() {
            Self::new(src.blur_state.blurred_buf().unwrap_or(buffer), src_width, src_height)
        } else {
            Self::new_single_color(0, 0, 0, src_width, src_height)
        }
    }

    pub fn new_single_color(r: u8, g: u8, b: u8, width: usize, height: usize) -> Self {
        Self {
            src: SourceType::Color { r, g, b },
            src_width: width,
            src_height: height,
            crop_x: 0,
            crop_y: 0,
            crop_width: width,
            crop_height: height,
            dst_x: 0,
            dst_y: 0,
            dst_width: width,
            dst_height: height,
            pixel_format: LayerPixelFormat::Argb8888,
            alpha: 255,
            low_priority: false,
        }
    }

    pub fn with_position(self, x: usize, y: usize) -> Self { Self { dst_x: x, dst_y: y, ..self } }

    pub fn with_crop(self, x: usize, y: usize, width: usize, height: usize) -> Self {
        let x = x.min(self.src_width);
        let y = y.min(self.src_height);
        let width = width.min(self.src_width - x);
        let height = height.min(self.src_height - y);

        Self {
            crop_x: x,
            crop_y: y,
            crop_width: width,
            crop_height: height,
            dst_width: width,
            dst_height: height,
            ..self
        }
    }

    pub fn with_dst_size(self, width: usize, height: usize) -> Self {
        Self { dst_width: width, dst_height: height, ..self }
    }

    pub fn with_alpha(self, alpha: u8) -> Self { Self { alpha, ..self } }

    pub fn with_low_priority(self) -> Self { Self { low_priority: true, ..self } }

    pub fn is_scaled(&self) -> bool {
        self.crop_width != self.dst_width || self.crop_height != self.dst_height
    }

    pub fn src(&self) -> SourceType { self.src }

    pub fn src_dimensions(&self) -> (usize, usize) { (self.src_width, self.src_height) }

    pub fn crop_pos(&self) -> (usize, usize) { (self.crop_x, self.crop_y) }

    pub fn crop_dimensions(&self) -> (usize, usize) { (self.crop_width, self.crop_height) }

    pub fn pixel_format(&self) -> LayerPixelFormat { self.pixel_format }

    pub fn dst_pos(&self) -> (usize, usize) { (self.dst_x, self.dst_y) }

    pub fn dst_dimensions(&self) -> (usize, usize) { (self.dst_width, self.dst_height) }

    pub fn alpha(&self) -> u8 { self.alpha }
}

impl LayerPixelFormat {
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            LayerPixelFormat::Argb8888 => 4,
            #[cfg(keyos)]
            LayerPixelFormat::Rgb565 => 2,
        }
    }
}

fn source_len_for_dimensions(width: usize, height: usize, pixel_format: LayerPixelFormat) -> Option<usize> {
    width.checked_mul(height)?.checked_mul(pixel_format.bytes_per_pixel())
}

impl LayerStack {
    pub fn push(&mut self, layer: Layer) {
        for (i, o) in &mut self.layers.iter_mut().enumerate() {
            match o {
                Some(o) => {
                    if o.is_scaled() && layer.is_scaled() {
                        log::error!("Cannot have two scaling layers, skipping {layer:?}");
                        return;
                    }
                }
                None => {
                    if i != 1 && i != 2 && layer.is_scaled() {
                        log::error!("Layer {i} can't have scaling. Skipping {layer:?}");
                    } else {
                        *o = Some(layer);
                        return;
                    }
                }
            }
        }
        if let Some(low_prio_layer) = self.layers.iter().position(|lo| lo.map_or(true, |l| l.low_priority)) {
            // Remove the low priority layer and put the new one on top
            // This could replace the earlier added low prio layer with a new low prio layer,
            // but that's by design.
            self.layers.copy_within(low_prio_layer + 1.., low_prio_layer);
            self.layers[self.layers.len() - 1] = Some(layer);
        } else if !layer.low_priority {
            log::error!("Too many layers; not adding {layer:?}");
        }
    }

    pub fn high_priority_layer_count(&self) -> usize {
        self.layers.iter().filter(|lo| lo.map_or(false, |l| !l.low_priority)).count()
    }
}

impl Gui {
    #[cfg(keyos)]
    pub(crate) fn boot_splash_layer() -> Layer {
        let boot_splash = unsafe {
            MemoryRange::new(xous::keyos::BOOT_SPLASH_FB, SCREEN_WIDTH * SCREEN_HEIGHT * 4).unwrap()
        };
        Layer::new(boot_splash, SCREEN_WIDTH, SCREEN_HEIGHT)
    }

    #[cfg(not(keyos))]
    pub(crate) fn boot_splash_layer() -> Layer {
        Layer::new_single_color(50, 0, 0, SCREEN_WIDTH, SCREEN_HEIGHT)
    }

    pub fn update_layers(&mut self) {
        let mut layers = LayerStack::default();
        let control_center_collapsed = self.is_control_center_collapsed();
        match &self.state {
            crate::GuiState::Splash => {
                layers.push(Self::boot_splash_layer());
            }
            crate::GuiState::SplashFade { to, progress } => {
                let Some(window) = self.windows.get(to) else {
                    log::error!("PID {to} does not have a window");
                    return;
                };
                layers.push(Self::boot_splash_layer());
                layers.push(
                    Layer::new_window(window, SCREEN_WIDTH, SCREEN_HEIGHT)
                        .with_alpha((*progress * 255 / 100) as u8),
                );
            }
            crate::GuiState::SingleWindow { pid, next_frame_animation, .. } => {
                let Some(window) = self.windows.get(pid) else {
                    log::error!("PID {pid} does not have a window");
                    return;
                };
                match next_frame_animation {
                    crate::NextFrameAnimationState::NotAnimating
                    | crate::NextFrameAnimationState::Waiting { .. } => {
                        self.add_camera_layer(window, &mut layers, 0);
                        layers.push(Layer::new_window(window, SCREEN_WIDTH, SCREEN_HEIGHT));
                    }
                    crate::NextFrameAnimationState::Animating { progress, kind } => {
                        Self::next_frame_animation_layers(
                            &mut layers,
                            Layer::new(self.animation_fb, SCREEN_WIDTH, SCREEN_HEIGHT),
                            Layer::new_window(window, SCREEN_WIDTH, SCREEN_HEIGHT),
                            *progress,
                            *kind,
                        );
                    }
                }
                self.add_keyboard_layer(window, &mut layers);
            }
            crate::GuiState::Switching { from, to, progress, animation, .. } => {
                let Some(from_window) = self.windows.get(from) else {
                    log::error!("From PID {from} does not have a window");
                    return;
                };
                let Some(to_window) = self.windows.get(to) else {
                    log::error!("To PID {to} does not have a window");
                    return;
                };
                animation.add_layers(
                    &mut layers,
                    Layer::new_window(from_window, SCREEN_WIDTH, SCREEN_HEIGHT),
                    Layer::new_window(to_window, SCREEN_WIDTH, SCREEN_HEIGHT),
                    *progress,
                );
            }
            crate::GuiState::Modal(modal_state) => {
                let Some(background) = self.windows.get(&modal_state.background_pid()) else {
                    log::error!("Modal bg PID {} does not have a window", modal_state.background_pid());
                    return;
                };
                if let Some(modal) = self.windows.get(&modal_state.modal_pid()) {
                    if modal_state.y() > 0 {
                        layers.push(Layer::new_window(background, SCREEN_WIDTH, SCREEN_HEIGHT));
                    }

                    self.add_camera_layer(modal, &mut layers, modal_state.y());

                    // If we have space for it, darken the background of the modal when it's not fullscreen
                    if !modal_state.is_fullscreen() && control_center_collapsed {
                        layers.push(
                            Layer::new_single_color(0, 0, 0, SCREEN_WIDTH, SCREEN_HEIGHT)
                                .with_alpha(modal_state.dark_overlay_alpha())
                                .with_low_priority(),
                        );
                    }

                    layers.push(
                        Layer::new_window(modal, SCREEN_WIDTH, SCREEN_HEIGHT)
                            .with_position(0, modal_state.y()),
                    );

                    // Only add a keyboard if we still have one layer for it and then the control center.
                    if layers.high_priority_layer_count() < MAX_LAYERS - 1 {
                        self.add_keyboard_layer(modal, &mut layers);
                    }
                } else {
                    log::trace!(
                        "Modal PID {} does not have a window, we are probably waiting for it",
                        modal_state.modal_pid()
                    );
                    self.add_camera_layer(background, &mut layers, 0);
                    layers.push(Layer::new_window(background, SCREEN_WIDTH, SCREEN_HEIGHT));
                    self.add_keyboard_layer(background, &mut layers);
                }
            }
        };

        if self.is_control_center_visible() {
            if let Some(control_center_window) = &self.control_center_window {
                // If we have space for it, darken the background
                if !control_center_collapsed {
                    layers.push(
                        Layer::new_single_color(0, 0, 0, SCREEN_WIDTH, SCREEN_HEIGHT)
                            .with_alpha(control_center_window.dark_overlay_alpha())
                            .with_low_priority(),
                    );
                }
                let crop_top = if control_center_window.state == ControlCenterWindowState::Collapsed {
                    0
                } else {
                    CONTROL_CENTER_HEIGHT_EXPANDED_PX - control_center_window.curr_height
                };
                layers.push(
                    Layer::new(
                        control_center_window.buffers.most_recent_buffer().unwrap(),
                        SCREEN_WIDTH,
                        CONTROL_CENTER_HEIGHT_EXPANDED_PX,
                    )
                    .with_crop(
                        0,
                        crop_top,
                        SCREEN_WIDTH,
                        control_center_window.curr_height,
                    ),
                );
            }
        }

        self.layers = layers;
        self.display.setup_layers(layers);
    }

    fn add_camera_layer(&self, window: &AppWindow, layers: &mut LayerStack, offset: usize) {
        #[cfg(feature = "recovery-os")]
        let _ = (window, layers, offset);

        #[cfg(not(feature = "recovery-os"))]
        if window.is_camera_visible() {
            if let Some(camera_front_buffer) = &self.camera_window.latest_frame {
                use camera::{CAMERA_HEIGHT, CAMERA_MARGIN};
                #[cfg(keyos)]
                const CAMERA_PIXEL_FORMAT: LayerPixelFormat = LayerPixelFormat::Rgb565;
                #[cfg(not(keyos))]
                const CAMERA_PIXEL_FORMAT: LayerPixelFormat = LayerPixelFormat::Argb8888;

                let crop_top = CAMERA_MARGIN - window.camera_state.y_pos as usize;
                layers.push(
                    Layer::new_with_pixel_format(
                        camera_front_buffer.padded_range(),
                        SCREEN_WIDTH,
                        CAMERA_HEIGHT + CAMERA_MARGIN * 2,
                        CAMERA_PIXEL_FORMAT,
                    )
                    .with_crop(0, crop_top, SCREEN_WIDTH, SCREEN_HEIGHT)
                    .with_position(0, offset),
                );
            }
        }
    }

    fn add_keyboard_layer(&self, window: &AppWindow, layers: &mut LayerStack) {
        if let Some(keyboard_height) = window.keyboard_state.height() {
            if let Some(keyboard_window) = &self.keyboard_window
                && let Some(buffer) = keyboard_window.buffers.most_recent_buffer()
            {
                layers.push(
                    Layer::new(
                        keyboard_window.blur_state.blurred_buf().unwrap_or(buffer),
                        SCREEN_WIDTH,
                        keyboard_height,
                    )
                    .with_position(0, SCREEN_HEIGHT - keyboard_height),
                );
            }
        }
    }
}
