// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;

use super::GenContext;
use crate::{
    generated_file::GeneratedFile,
    source::{uwrite, uwriteln, Source},
};

pub static ROUTER_INIT_RS: &str = "router_init.rs";

pub fn generate_and_write(ctx: GenContext) -> Result<(), std::io::Error> {
    let file = generate(ctx);
    file.write(&ctx.config.out_dir)
}

fn generate(ctx: GenContext) -> GeneratedFile {
    let mut src = Source::default();
    let data = ctx.data;

    uwriteln!(
        src,
        "
            macro_rules! create_router {{
                ($app:expr, $ctx:expr) => {{
                    {{
                        let app = &$app;
                        let router = $ctx.router.clone();
                        let gui_api = $ctx.gui.clone();

                        router.borrow_mut().register_on_navigation_start({{
                            let app = slint_keyos_platform::slint::ComponentHandle::clone_strong(app);
                            move |history| {{
                                let has_backward = history.has_backward();
                                let has_forward = history.has_forward();
                                app.global::<Navigate>().set_has_backward(has_backward);
                                app.global::<Navigate>().set_has_forward(has_forward);
                            }}
                        }});

                        // Register routes and callbacks.
            "
    );

    // Register routes
    uwriteln!(src, "{{");
    uwriteln!(src, "let mut router = router.borrow_mut();");
    for p in data.pages {
        uwriteln!(src, "router.register_route::<{}>({{", p.rust_tuple());
        uwrite!(
            src,
            "
                let app = slint_keyos_platform::slint::ComponentHandle::clone_strong(app);
                move |router| {{
                "
        );
        uwrite!(src, "let (");
        for prop in p.props.iter() {
            uwrite!(src, "{},", prop.names.snake);
        }
        uwriteln!(src, ") = router.get_active::<{}>().expect(\"State to be active\");", p.rust_tuple());
        uwriteln!(
            src,
            "
                let route_state = app.global::<RouteState>();
                "
        );
        for prop in p.props.iter() {
            uwriteln!(src, "route_state.set_{}({});", prop.names.snake, prop.names.snake);
        }
        uwriteln!(src, "route_state.set_active(RouteOption::{});", p.names.pascal);
        uwriteln!(src, "}}");
        uwriteln!(src, "}});");
    }

    // Initialize first state
    if let Some(p) = data.pages.first() {
        uwriteln!(src, "router.push_route(<{}>::default());", p.rust_tuple());
    }
    uwriteln!(src, "}}");

    // Navigation callbacks
    uwriteln!(src, "let navigation = app.global::<Navigate>();");
    for p in data.pages {
        uwrite! {src,
            "
                navigation.on_{snake_name}({{
                ",
            snake_name = p.names.snake
        }

        uwriteln!(src, "let gui_api = gui_api.clone();");

        p.rust_closure_input(&mut src);
        uwriteln!(src, "{{");

        for prop in &p.props {
            uwriteln!(src, "{}", prop.rust_struct_construction());
        }
        uwrite!(src, "let props = (");
        for prop in p.props.iter() {
            uwrite!(src, "{},", prop.names.snake);
        }
        uwriteln!(src, ");");

        uwrite! {src,
            "
                let success = if options.replace {{
                    router.borrow_mut().replace_route::<{tuple}>(props)
                }} else {{
                    router.borrow_mut().push_route::<{tuple}>(props)
                }};
                if success {{
                    match options.animate {{
                        Animate::Forward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap(),
                        Animate::Backward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap(),
                        _ => (),
                    }};
                }}
                ",
            tuple = p.rust_tuple()
        }
        uwriteln!(src, "}}");
        uwriteln!(src, "}});");
    }

    // encode callbacks
    for (ii, p) in data.pages.iter().filter(|p| !p.is_static_route()).enumerate() {
        if ii == 0 {
            uwriteln!(src, "let encode = app.global::<RoutePath>();");
        }

        uwriteln!(src, "encode.on_{}({{", p.names.snake);

        uwrite!(src, "move |");
        p.deconstruct_params(&mut src);
        uwriteln!(src, "| {{");

        for prop in &p.props {
            uwriteln!(src, "{}", prop.rust_struct_construction());
        }
        uwrite!(src, "let props = (");
        for prop in p.props.iter() {
            uwrite!(src, "{},", prop.names.snake);
        }
        uwriteln!(src, ");");

        uwriteln!(
            src,
            "slint_keyos_platform::route::RouteCodec::ser_route(&props).expect(\"serialize route\").into()"
        );

        uwriteln!(src, "}}");
        uwriteln!(src, "}});");
    }

    uwrite!(
        src,
        "
            navigation.on_backward({{
                let gui_api = gui_api.clone();
                move || {{
                    let mut router = router.borrow_mut();
                    if router.navigate_backward() {{
                        gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap();
                    }}
                }}
            }});
            navigation.on_navigate({{
                let gui_api = gui_api.clone();
                move |path, options| {{
                    let mut router = router.borrow_mut();
                    let success = if options.replace {{
                        router.replace_raw_route(path.into()).expect(\"Failed to replace route\")
                    }} else {{
                        router.push_raw_route(path.into()).expect(\"Failed to push route\")
                    }};
                    if success {{
                        match options.animate {{
                            Animate::Forward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap(),
                            Animate::Backward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap(),
                            _ => (),
                        }};
                    }}
                }}
            }});
            navigation.on_forward({{
                let gui_api = gui_api.clone();
                move || {{
                    let mut router = router.borrow_mut();
                    if router.navigate_forward() {{
                        gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap();
                    }}
                }}
            }});
            navigation.on_backward_animate({{
                let gui_api = gui_api.clone();
                move |animate| {{
                    let mut router = router.borrow_mut();
                    if router.navigate_backward() {{
                        match animate {{
                            Animate::Forward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap(),
                            Animate::Backward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap(),
                            _ => (),
                        }};
                    }}
                }}
            }});
            navigation.on_forward_animate({{
                let gui_api = gui_api.clone();
                move |animate| {{
                    let mut router = router.borrow_mut();
                    if router.navigate_forward() {{
                        match animate {{
                            Animate::Forward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap(),
                            Animate::Backward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap(),
                            _ => (),
                        }};
                    }}
                }}
            }});
            navigation.on_return_home({{
                let gui_api = gui_api.clone();
                move || {{
                    let mut router = router.borrow_mut();
                    let navigated = {{
                        let mut any = false;
                        while router.navigate_backward() {{ any = true; }}
                        any
                    }};
                    if navigated {{
                        gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap();
                    }}
                }}
            }});
            navigation.on_return_home_animate({{
                let gui_api = gui_api.clone();
                move |animate| {{
                    let mut router = router.borrow_mut();
                    let navigated = {{
                        let mut any = false;
                        while router.navigate_backward() {{ any = true; }}
                        any
                    }};
                    if navigated {{
                        match animate {{
                            Animate::Forward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideOutRight).unwrap(),
                            Animate::Backward => gui_api.animate_next_frame(slint_keyos_platform::gui_server_api::NextFrameAnimationKind::SlideInRight).unwrap(),
                            _ => (),
                        }};
                    }}
                }}
            }});
            navigation.on_debug({{
                move || {{
                    let router = router.borrow();
                    router.with_history(|history| println!(\"{{history:#?}}\"));
                }}
            }});
        "
    );

    uwriteln!(src, "router");
    uwriteln!(src, "}}");
    uwriteln!(src, "}}");
    uwriteln!(src, "}}");

    GeneratedFile { path: ROUTER_INIT_RS.into(), content: src.into() }
}

pub fn gen_empty_router(out_dir: impl AsRef<std::path::Path>) -> Result<(), std::io::Error> {
    let mut src = Source::default();
    uwriteln!(
        src,
        "
        #[allow(unused_macros)]
        macro_rules! create_router {{
            ($app:expr) => {{
                {{
                    let _ = &$app;
                }}
            }};

            ($app:expr, $router:expr) => {{
                {{
                    let _ = $app;
                    let _ = $router;
                }}
            }};
        }}
        "
    );

    let file = GeneratedFile { path: ROUTER_INIT_RS.into(), content: src.into() };
    file.write(out_dir.as_ref())
}
