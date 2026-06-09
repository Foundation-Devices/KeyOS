// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless layout self-check: renders the control-panel MainWindow with the
//! software renderer to a PNG. Layout is renderer-independent, so this is a good
//! proxy for arrangement/spacing even though the app itself runs on femtovg.
//!
//! Usage: `cargo run --manifest-path apps/simulator/Cargo.toml --bin shot -- [out.png] [w] [h]`

use std::rc::Rc;

use simulator::MainWindow;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::ComponentHandle;

struct ShotPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl slint::platform::Platform for ShotPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "/tmp/cp_layout.png".to_string());
    let w: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(494);
    let h: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(423);

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(ShotPlatform { window: window.clone() })).unwrap();

    let ui = MainWindow::new().unwrap();
    window.set_size(slint::PhysicalSize::new(w, h));
    ui.show().unwrap();

    let mut buffer = vec![Rgb565Pixel(0); (w * h) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, w as usize);
    });

    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for (i, px) in buffer.iter().enumerate() {
        let v = px.0;
        rgb[i * 3] = (((v >> 11) & 0x1f) * 255 / 31) as u8;
        rgb[i * 3 + 1] = (((v >> 5) & 0x3f) * 255 / 63) as u8;
        rgb[i * 3 + 2] = ((v & 0x1f) * 255 / 31) as u8;
    }
    image::save_buffer(&out, &rgb, w, h, image::ColorType::Rgb8).unwrap();
    println!("wrote {out} ({w}x{h})");
}
