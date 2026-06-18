// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

slint::include_modules!();

/* Slint Problems/TODO:

- There is no flow layout
- GridLayout crashes when creating Row and content dynamically
- Slint functions say they are returning () instead of the real type, even when all paths are covered with returns
- Text widgets do not get drawn if they don't fit in the view.  Should just be clipped.
- What is the difference between inheriting from a component vs. just using the same component as the root? (CrossLayout didn't work)
- How to make a component in a layout take its natural size and not scale without setting a fixed size?
    (Use alignment: stretch, then apply horizontal-stretch: 1, etc. to the components that need to expand)
- Layouts don't seem to respect padding - they should fit within the padding bounds
    (Don't set a width and the layout will respect the padding)
- Does Slint scroll the focused input into view if in a scroll view?
- How can I have an image that doesn't scale?
- Binding loop detection is too aggressive.  If I have a specified width or height, then I should be able to use the correspodning value internal to the component.
- Want to be able to convert between float/int and percent (can divide go percent to float, but not the other way it seems)
- Add support for angular gradients to get circular spinner gradient effects

*/

use slint::{ComponentHandle, Image, SharedString};

// Slintbook has no fs server, so resolve images straight off the host
// filesystem by file stem; nine-slice edges and dark variants are ignored.
fn find_image(roots: [&str; 2], name: SharedString) -> Image {
    let stem = name.rsplit('/').next().unwrap_or(name.as_str());
    for root in roots {
        let path = std::path::Path::new(root).join(name.as_str());
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new(root));
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.file_stem().and_then(|s| s.to_str()) == Some(stem) {
                if let Ok(image) = Image::load_from_path(&candidate) {
                    return image;
                }
            }
        }
    }
    Image::default()
}

fn main() {
    let slintbook = SlintBook::new().unwrap();
    slint_keyos_platform::_internal_init_ui_utils!(Utils, slintbook);

    let images = ["../../resources", "../../ui/ui"];
    let icons = ["../../resources/icons", "../../ui/ui/icons"];
    slintbook.global::<Images>().on_common(move |path| find_image(images, path));
    slintbook.global::<Images>().on_nine_slice(move |path| find_image(images, path));
    slintbook.global::<Images>().on_icon(move |name, _size| find_image(icons, name));

    slintbook.run().unwrap();
}
