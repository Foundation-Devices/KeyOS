// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::Cell,
    rc::{Rc, Weak},
    sync::Arc,
};

use gui_server_api::{
    consts::{DEFAULT_KEYBOARD_HEIGHT, KEYBOARD_TOP_BAR_MARGIN},
    GuiApi, KeyboardKind,
};
use i_slint_core::window::{InputMethodRequest, WindowAdapter, WindowAdapterInternal, WindowInner};
use slint::{
    platform::{
        software_renderer::{LineBufferProvider, RepaintBufferType, SoftwareRenderer},
        Renderer,
    },
    PhysicalPosition, PhysicalSize, Window,
};

use crate::GuiAppGuiPermissions;

/// Height of the part of the keyboard that actually covers us. Its top bar is transparent, and
/// gui-server routes touches there to the app rather than to the keyboard.
const KEYBOARD_OPAQUE_HEIGHT: usize = DEFAULT_KEYBOARD_HEIGHT - KEYBOARD_TOP_BAR_MARGIN;

/// This is a minimal adapter for a Window that doesn't have any other feature than rendering
/// using the software renderer.
pub struct KeyOsWindow<PG: GuiAppGuiPermissions> {
    window: Window,
    gui: Arc<GuiApi<PG>>,
    pub(crate) renderer: SoftwareRenderer,
    requested_redraw: Cell<bool>,
    size: PhysicalSize,
    keyboard_shown: Cell<bool>,
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
            keyboard_shown: Cell::new(false),
        })
    }

    pub fn draw(&self, line_provider: impl LineBufferProvider) {
        self.renderer.render_by_line(line_provider);
        self.requested_redraw.set(false);
    }

    /// Show the keyboard with the given configuration, or hide it with `None`.
    ///
    /// Also tells Slint which part of the window the keyboard covers, so that it can scroll a
    /// focused input out from under it.
    fn set_keyboard(&self, update: Option<gui_server_api::msg::UpdateKeyboard>) {
        let shown = update.is_some();

        // Slint re-scrolls the focus item every time the covered area is set, and we get an update
        // per keystroke, or else typing would yank back a user who scrolled away.
        if self.keyboard_shown.replace(shown) != shown {
            let (origin, size) = if shown {
                let scale_factor = self.window.scale_factor();
                // The keyboard and the windows it covers are both bottom-aligned.
                let height = (KEYBOARD_OPAQUE_HEIGHT as u32).min(self.size.height);
                (
                    PhysicalPosition::new(0, (self.size.height - height) as i32).to_logical(scale_factor),
                    PhysicalSize::new(self.size.width, height).to_logical(scale_factor),
                )
            } else {
                Default::default()
            };

            self.window.set_virtual_keyboard(origin, size, i_slint_core::InternalToken);
        }

        match update {
            Some(update) => self.gui.update_keyboard(update).ok(),
            None => self.gui.hide_keyboard().ok(),
        };
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
                self.set_keyboard(Some(gui_server_api::msg::UpdateKeyboard {
                    kind,
                    request_caps,
                    accept_button_text: imp.accept_button_text.to_string(),
                    accept_button_enabled: imp.accept_button_enabled,
                    delete_button_enabled,
                }));
            }
            InputMethodRequest::Disable => self.set_keyboard(None),
            _ => {}
        }
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
            self.set_keyboard(None);
        }
    }
}
impl<PG: GuiAppGuiPermissions> core::ops::Deref for KeyOsWindow<PG> {
    type Target = Window;

    fn deref(&self) -> &Self::Target { &self.window }
}
