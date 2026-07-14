// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::Cell,
    rc::{Rc, Weak},
    sync::Arc,
};

use gui_server_api::{GuiApi, KeyboardKind};
use i_slint_core::window::{InputMethodRequest, WindowAdapter, WindowAdapterInternal, WindowInner};
use slint::{
    platform::{
        software_renderer::{LineBufferProvider, RepaintBufferType, SoftwareRenderer},
        Renderer,
    },
    PhysicalSize, Window,
};

use crate::GuiAppGuiPermissions;
/// This is a minimal adapter for a Window that doesn't have any other feature than rendering
/// using the software renderer.
pub struct KeyOsWindow<PG: GuiAppGuiPermissions> {
    window: Window,
    gui: Arc<GuiApi<PG>>,
    pub(crate) renderer: SoftwareRenderer,
    requested_redraw: Cell<bool>,
    size: PhysicalSize,
}

impl<PG: GuiAppGuiPermissions> KeyOsWindow<PG> {
    pub fn new(gui: Arc<GuiApi<PG>>, size: PhysicalSize) -> Rc<Self> {
        Rc::new_cyclic(|w: &Weak<Self>| Self {
            window: Window::new(w.clone()),
            gui,
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::SwappedBuffers),
            // We get a frame at the start, which is an implicit redraw request at init.
            requested_redraw: Cell::new(true),
            size,
        })
    }

    pub fn draw(&self, line_provider: impl LineBufferProvider) {
        self.renderer.render_by_line(line_provider);
        self.requested_redraw.set(false);
    }
}

impl<PG: GuiAppGuiPermissions> WindowAdapter for KeyOsWindow<PG> {
    fn window(&self) -> &Window { &self.window }

    fn renderer(&self) -> &dyn Renderer { &self.renderer }

    fn size(&self) -> PhysicalSize { self.size }

    fn set_size(&self, size: slint::WindowSize) {
        log::warn!("Trying to call unimplemented function: set_size({size:?})");
    }

    fn request_redraw(&self) {
        if !self.requested_redraw.get() {
            self.gui.request_redraw().ok();
        }
        self.requested_redraw.set(true);
    }

    fn internal(&self, _: i_slint_core::InternalToken) -> Option<&dyn WindowAdapterInternal> { Some(self) }
}

impl<PG: GuiAppGuiPermissions> WindowAdapterInternal for KeyOsWindow<PG> {
    fn input_method_request(&self, imr: InputMethodRequest) {
        log::trace!("Got {imr:?}");
        match imr {
            InputMethodRequest::Enable(imp) | InputMethodRequest::Update(imp) => {
                let kind = match imp.input_type {
                    i_slint_core::items::InputType::Number => KeyboardKind::Numbers,
                    i_slint_core::items::InputType::Decimal => KeyboardKind::Decimal,
                    _ => KeyboardKind::Alphanumeric,
                };
                let pre_cursor_text = &imp.text[..imp.cursor_position];
                let request_caps = match imp.caps_mode {
                    i_slint_core::items::CapsMode::None => false,
                    i_slint_core::items::CapsMode::Sentences => pre_cursor_text
                        .trim_end()
                        .chars()
                        .last()
                        .map(|c| c == '.' || c == '!' || c == '?')
                        .unwrap_or(true),
                    i_slint_core::items::CapsMode::Words => {
                        pre_cursor_text.chars().last().map(|c| c.is_whitespace()).unwrap_or(true)
                    }
                    i_slint_core::items::CapsMode::All => true,
                };
                let delete_button_enabled = imp.delete_button_enabled && !imp.text.is_empty();
                self.gui
                    .update_keyboard(gui_server_api::msg::UpdateKeyboard {
                        kind,
                        request_caps,
                        accept_button_text: imp.accept_button_text.to_string(),
                        accept_button_enabled: imp.accept_button_enabled,
                        delete_button_enabled,
                    })
                    .ok()
            }
            InputMethodRequest::Disable => self.gui.hide_keyboard().ok(),
            _ => None,
        };
    }

    fn unregister_item_tree(
        &self,
        _component: i_slint_core::item_tree::ItemTreeRef,
        _items: &mut dyn Iterator<Item = std::pin::Pin<i_slint_core::items::ItemRef<'_>>>,
    ) {
        // This method is called in the Drop function of the ItemTree, when the refcount on the Rc reaches
        // zero. Since there is no Rc anymore, focus events cannot be called on the item, and IMR events also
        // won't be sent anymore.
        // We can detect if the removed element was the focused one by trying to upgrade the weak ref in
        // window, and if it's no longer valid (see above), we have to pretend we got an IMR disable event and
        // just hide the keyboard.
        let window = WindowInner::from_pub(self.window());
        let focus_item = window.focus_item.borrow().clone();
        if focus_item != Default::default() && focus_item.upgrade().is_none() {
            // Also clear the focus item to prevent repeated calls.
            window.focus_item.take();
            self.gui.hide_keyboard().ok();
        }
    }
}
impl<PG: GuiAppGuiPermissions> core::ops::Deref for KeyOsWindow<PG> {
    type Target = Window;

    fn deref(&self) -> &Self::Target { &self.window }
}
