// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {slint_keyos_platform::app, std::rc::Rc};

app!("System Actions");

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let settings = Rc::new(SettingsApi::default());
    ui.global::<Callbacks>().set_touch_offset(settings.get_touch_offset().0);

    ui.global::<Callbacks>().on_reboot({
        let gui = cx.gui.clone();
        move || {
            log::info!("Rebooting");
            gui.reboot().ok();
        }
    });

    ui.global::<Callbacks>().on_shut_down({
        let gui = cx.gui.clone();
        move || {
            log::info!("Shutting down");
            gui.shutdown().ok();
        }
    });

    ui.global::<Callbacks>().on_crash_test(move || {
        crash_test();
    });

    ui.global::<Callbacks>().set_debug_touch(settings.get_debug_touch().0);

    ui.global::<Callbacks>().on_set_debug_touch({
        let ui = ui.as_weak();
        let settings = settings.clone();
        move |debug| {
            settings.set_debug_touch(debug);
            let ui = ui.unwrap();
            ui.global::<Callbacks>().set_debug_touch(debug);
        }
    });

    ui.global::<Callbacks>().on_set_touch_offset({
        let ui = ui.as_weak();
        let settings = settings.clone();
        move |value| {
            settings.set_touch_offset(value);
            let ui = ui.unwrap();
            ui.global::<Callbacks>().set_touch_offset(value);
        }
    });

    ui.global::<Callbacks>().on_is_sim({
        move || {
            return !cfg!(keyos);
        }
    });

    ui.run().expect("UI running");
}

fn crash_test() {
    let mut byte = [0u8; 1];
    getrandom::getrandom(&mut byte).expect("getrandom");
    match byte[0] % 3 {
        0 => {
            log::info!("Crashing the app with a panic message");
            panic!("This application has crashed with a test panic message");
        }
        1 => {
            log::info!("Crashing the app with a stack overflow");
            stack_overflow(0);
        }
        _ => {
            log::info!("Crashing the app by hammering random user-space pointers");
            wild_pointer_hammer();
        }
    }
}

#[inline(never)]
#[allow(unconditional_recursion)]
fn stack_overflow(depth: u64) -> u64 {
    let frame = [depth; 64];
    let next = stack_overflow(depth + 1);
    core::hint::black_box(frame[0]).wrapping_add(next)
}

fn wild_pointer_hammer() -> ! {
    let start = keyos::ASLR_START;
    let end = keyos::USER_AREA_END;
    loop {
        let mut bytes = [0u8; 4];
        getrandom::getrandom(&mut bytes).expect("getrandom");
        let addr = start + (u32::from_le_bytes(bytes) as usize) % (end - start);
        let ptr = addr as *mut u8;
        unsafe {
            let v = ptr.read_volatile();
            ptr.write_volatile(v);
        }
    }
}
