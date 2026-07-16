// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use indoc::{formatdoc, indoc};

use self::validate::RouterOutput;
use super::generate;
use crate::generated_file::{GenerateConfig, GeneratedFile};
use crate::router::validate;
use crate::router::validate::error::RouteError;

#[test]
fn single_dynamic() {
    let input_files = vec![
        generated_file(
            "props.slint",
            indoc! {
                r#"
                @rust-attr(route(default, path = "/person/{name}?{age}"))
                export struct PersonProps {
                    name: string,
                    age: int,
                }
                "#
            },
        ),
        generated_file(
            "page.slint",
            indoc! {
                    r#"
                import { Navigate } from "./gen/navigate.slint";
                import { PersonProps } from "./props.slint";

                export component PersonPage {
                    in property <PersonProps> props;

                    init => {
                        Navigate.debug();
                    }

                    Rectangle {}
                }

                "#
            },
        ),
    ];

    let expected_output = [
        generated_file(
            "internal.slint",
            indoc! {
                r#"
                import { PersonProps } from "../props.slint";

                export enum RouteOption {
                    Person,
                }
                export global RouteState {
                    in property <RouteOption> active;
                    in-out property <PersonProps> person-props;
                }
                "#
            },
        ),
        generated_file(
            "navigate.slint",
            indoc! {
                r#"

                export enum Animate {
                    Forward,
                    Backward,
                    None,
                }

                struct NavigateOptions {
                    replace: bool,
                    animate: Animate,
                }

                export struct PersonParams {
                    age: int, 
                    name: string, 
                }

                export global Navigate {
                    in property <bool> has-backward;
                    in property <bool> has-forward;

                    callback backward();
                    callback forward();

                    callback backward-animate(Animate);
                    callback forward-animate(Animate);

                    callback return-home();
                    callback return-home-animate(Animate);

                    callback debug();

                    callback navigate(string, NavigateOptions);

                    callback person(PersonParams, NavigateOptions);
                }

                export global RoutePath {
                    pure callback person(PersonParams) -> string;
                }
                "#
            },
        ),
        generated_file(
            "exports.slint",
            indoc! {
                r#"
                import { Navigate, RoutePath } from "navigate.slint";
                import { RouteOption, RouteState } from "internal.slint";

                export { Navigate, RoutePath, RouteOption, RouteState }
                "#
            },
        ),
        generated_file(
            "router.slint",
            indoc! {
                r#"
                import { PersonPage } from "../page.slint";
                import { RouteState, RouteOption } from "internal.slint";

                export component Router inherits Rectangle {
                    if (RouteState.active == RouteOption.Person): PersonPage {
                        props <=> RouteState.person-props;
                    }
                }
                "#
            },
        ),
    ];

    let result = no_errors_or_warnings(input_files, expected_output);
    let page = result.valid_pages.iter().next().expect("single page");

    let slint_callback = {
        let mut s = String::new();
        page.slint_callback_decl(&mut s);
        s
    };
    assert_eq!(slint_callback, "callback person(PersonParams, NavigateOptions);");

    let rust_closure = {
        let mut s = String::new();
        page.rust_closure_input(&mut s);
        s
    };
    assert_eq!(rust_closure, "move |PersonParams{age,name,}, options|");
}

#[test]
fn single_static() {
    let input_files = vec![
        generated_file(
            "props.slint",
            indoc! {
                r#"
                @rust-attr(route(default, path = "/about"))
                export struct AboutProps {}
                "#
            },
        ),
        generated_file(
            "page.slint",
            indoc! {
                r#"
                import { AboutProps } from "./props.slint";

                export component AboutPage {
                    in property <AboutProps> props;

                    Rectangle {
                        Text {
                            text: "About Page";
                        }
                    }
                }
                "#
            },
        ),
    ];

    let expected_output = [
        generated_file(
            "internal.slint",
            indoc! {
                r#"
                import { AboutProps } from "../props.slint";

                export enum RouteOption {
                    About,
                }
                export global RouteState {
                    in property <RouteOption> active;
                    in-out property <AboutProps> about-props;
                }
                "#
            },
        ),
        generated_file(
            "navigate.slint",
            indoc! {
                r#"

                export enum Animate {
                    Forward,
                    Backward,
                    None,
                }

                struct NavigateOptions {
                    replace: bool,
                    animate: Animate,
                }

                export global Navigate {
                    in property <bool> has-backward;
                    in property <bool> has-forward;

                    callback backward();
                    callback forward();

                    callback backward-animate(Animate);
                    callback forward-animate(Animate);

                    callback return-home();
                    callback return-home-animate(Animate);

                    callback debug();

                    callback navigate(string, NavigateOptions);

                    callback about(NavigateOptions);
                }

                export global RoutePath {
                    out property <string> about : "/about";
                }
                "#
            },
        ),
        generated_file(
            "exports.slint",
            indoc! {
                r#"
                import { Navigate, RoutePath } from "navigate.slint";
                import { RouteOption, RouteState } from "internal.slint";

                export { Navigate, RoutePath, RouteOption, RouteState }
                "#
            },
        ),
        generated_file(
            "router.slint",
            indoc! {
                r#"
                import { AboutPage } from "../page.slint";
                import { RouteState, RouteOption } from "internal.slint";

                export component Router inherits Rectangle {
                    if (RouteState.active == RouteOption.About): AboutPage {
                        props <=> RouteState.about-props;
                    }
                }
                "#
            },
        ),
    ];

    let result = no_errors_or_warnings(input_files, expected_output);
    let page = result.valid_pages.iter().next().expect("single page");

    let slint_callback = {
        let mut s = String::new();
        page.slint_callback_decl(&mut s);
        s
    };
    assert_eq!(slint_callback, "callback about(NavigateOptions);");

    let rust_closure = {
        let mut s = String::new();
        page.rust_closure_input(&mut s);
        s
    };
    assert_eq!(rust_closure, "move |options|");
}

#[test]
fn two_with_navigate() {
    let input_files = vec![
        generated_file(
            "test-one/props.slint",
            indoc! {
                r#"
                @rust-attr(route(default, path = "/test-one/{show-debug}"))
                export struct TestOneProps {
                    show-debug: bool
                }
                "#
            },
        ),
        generated_file(
            "test-one/page.slint",
            indoc! {
                    r#"
                import { Button } from "std-widgets.slint";

                import { Navigate } from "../gen/navigate.slint";
                import { TestOneProps } from "./props.slint";

                export component TestOnePage {
                    in property <TestOneProps> props;

                    Button {
                        clicked => {
                            Navigate.test-two({ text: "Hello" }, {});
                        }
                    }
                }

                "#
            },
        ),
        generated_file(
            "test-two/props.slint",
            indoc! {
                r#"
                @rust-attr(route(path = "/test-two/{text}"))
                export struct TestTwoProps {
                    text: string
                }
                "#
            },
        ),
        generated_file(
            "test-two/page.slint",
            indoc! {
                    r#"
                import { Button } from "std-widgets.slint";

                import { Navigate } from "../gen/navigate.slint";
                import { TestTwoProps } from "./props.slint";

                export component TestTwoPage {
                    in property <TestTwoProps> props;

                    Button {
                        clicked => {
                            Navigate.test-one({ show-debug: true }, {});
                        }
                    }
                }

                "#
            },
        ),
    ];

    let expected_output = [
        generated_file(
            "internal.slint",
            indoc! {
                r#"
                import { TestOneProps } from "../test-one/props.slint";
                import { TestTwoProps } from "../test-two/props.slint";

                export enum RouteOption {
                    TestOne,
                    TestTwo,
                }
                export global RouteState {
                    in property <RouteOption> active;
                    in-out property <TestOneProps> test-one-props;
                    in-out property <TestTwoProps> test-two-props;
                }
                "#
            },
        ),
        generated_file(
            "navigate.slint",
            indoc! {
                r#"

                export enum Animate {
                    Forward,
                    Backward,
                    None,
                }

                struct NavigateOptions {
                    replace: bool,
                    animate: Animate,
                }

                export struct TestOneParams {
                    show-debug: bool,
                }

                export struct TestTwoParams {
                    text: string,
                }

                export global Navigate {
                    in property <bool> has-backward;
                    in property <bool> has-forward;

                    callback backward();
                    callback forward();

                    callback backward-animate(Animate);
                    callback forward-animate(Animate);

                    callback return-home();
                    callback return-home-animate(Animate);

                    callback debug();

                    callback navigate(string, NavigateOptions);

                    callback test-one(TestOneParams, NavigateOptions);
                    callback test-two(TestTwoParams, NavigateOptions);
                }

                export global RoutePath {
                    pure callback test-one(TestOneParams) -> string;
                    pure callback test-two(TestTwoParams) -> string;
                }
                "#
            },
        ),
        generated_file(
            "exports.slint",
            indoc! {
                r#"
                import { Navigate, RoutePath } from "navigate.slint";
                import { RouteOption, RouteState } from "internal.slint";

                export { Navigate, RoutePath, RouteOption, RouteState }
                "#
            },
        ),
        generated_file(
            "router.slint",
            indoc! {
                r#"
                import { TestOnePage } from "../test-one/page.slint";
                import { TestTwoPage } from "../test-two/page.slint";

                import { RouteState, RouteOption } from "internal.slint";

                export component Router inherits Rectangle {
                    if (RouteState.active == RouteOption.TestOne): TestOnePage {
                        props <=> RouteState.test-one-props;
                    }
                    if (RouteState.active == RouteOption.TestTwo): TestTwoPage {
                        props <=> RouteState.test-two-props;
                    }
                }
                "#
            },
        ),
    ];

    let _ = no_errors_or_warnings(input_files, expected_output);
}

#[test]
fn nested_params() {
    let input_files = vec![
        generated_file(
            "parent/props.slint",
            indoc! {
                r#"
                @rust-attr(route(default, path = "/parent/{parent-name}"))
                export struct ParentProps {
                    parent-name: string
                }
                "#
            },
        ),
        generated_file(
            "parent/page.slint",
            indoc! {
                r#"
                import { ParentProps } from "./props.slint";

                export component ParentPage {
                    in property <ParentProps> props;
                    Rectangle {
                        Text {
                            text: props.parent-name;
                        }
                    }
                }
                "#
            },
        ),
        generated_file(
            "parent/child/props.slint",
            indoc! {
                r#"
                @rust-attr(route(path = "/child/{child-name}"))
                export struct ChildProps {
                    child-name: string
                }
                "#
            },
        ),
        generated_file(
            "parent/child/page.slint",
            indoc! {
                r#"
                import { ParentProps } from "../props.slint";
                import { ChildProps } from "./props.slint";

                export component ChildPage {
                    in property <ParentProps> parent-props;
                    in property <ChildProps> child-props;
                    Rectangle {
                        Text {
                            text: parent-props.parent-name;
                        }
                        Text {
                            text: child-props.child-name;
                        }
                    }
                }
                "#
            },
        ),
    ];

    let expected_output = [
        generated_file(
            "internal.slint",
            indoc! {
                r#"
                import { ParentProps } from "../parent/props.slint";
                import { ChildProps } from "../parent/child/props.slint";

                export enum RouteOption {
                    Parent,
                    Child,
                }
                export global RouteState {
                    in property <RouteOption> active;
                    in-out property <ParentProps> parent-props;
                    in-out property <ChildProps> child-props;
                }
                "#
            },
        ),
        generated_file(
            "navigate.slint",
            indoc! {
                r#"
                export enum Animate {
                    Forward,
                    Backward,
                    None,
                }

                struct NavigateOptions {
                    replace: bool,
                    animate: Animate,
                }

                export struct ParentParams {
                    parent-name: string,
                }

                export struct ChildParams {
                    parent-name: string,
                    child-name: string,
                }

                export global Navigate {
                    in property <bool> has-backward;
                    in property <bool> has-forward;

                    callback backward();
                    callback forward();

                    callback backward-animate(Animate);
                    callback forward-animate(Animate);

                    callback return-home();
                    callback return-home-animate(Animate);

                    callback debug();

                    callback navigate(string, NavigateOptions);

                    callback parent(ParentParams, NavigateOptions);
                    callback child(ChildParams, NavigateOptions);
                }

                export global RoutePath {
                    pure callback parent(ParentParams) -> string;
                    pure callback child(ChildParams) -> string;
                }
                "#
            },
        ),
        generated_file(
            "exports.slint",
            indoc! {
                r#"
                import { Navigate, RoutePath } from "navigate.slint";
                import { RouteOption, RouteState } from "internal.slint";

                export { Navigate, RoutePath, RouteOption, RouteState }
                "#
            },
        ),
        generated_file(
            "router.slint",
            indoc! {
                r#"
                import { ParentPage } from "../parent/page.slint";
                import { ChildPage } from "../parent/child/page.slint";

                import { RouteState, RouteOption } from "internal.slint";

                export component Router inherits Rectangle {
                    if (RouteState.active == RouteOption.Parent): ParentPage {
                        props <=> RouteState.parent-props;
                    }
                    if (RouteState.active == RouteOption.Child): ChildPage {
                        child-props <=> RouteState.child-props;
                        parent-props <=> RouteState.parent-props;
                    }
                }
                "#
            },
        ),
    ];

    let _ = no_errors_or_warnings(input_files, expected_output);
}

#[test]
fn complex_types() {
    let input_files = vec![
        generated_file(
            "props.slint",
            indoc! {
                r#"
                @rust-attr(derive(serde::Serialize, serde::Deserialize))
                struct PowerStats {
                    strength: int,
                    agility: int,
                    magic: int,
                }

                @rust-attr(derive(serde::Serialize, serde::Deserialize))
                export enum Element {
                    Fire,
                    Ice,
                    Lightning,
                    Earth,
                }

                @rust-attr(route(default, path = "/character/{element}/{stats}"))
                export struct CharacterProps {
                    element: Element,
                    stats: PowerStats,
                }
                "#
            },
        ),
        generated_file(
            "page.slint",
            indoc! {
                r#"
                import { Element, CharacterProps } from "./props.slint";

                export component CharacterPage {
                    in property <CharacterProps> props;

                    Rectangle {
                        Text {
                            text: "Character Sheet";
                        }
                        VerticalLayout {
                            Text { text: "Power Level: " +
                                (props.stats.strength + props.stats.agility + props.stats.magic); }
                            if (props.element == Element.Fire): Text { text: "Element: Fire"; }
                            if (props.element == Element.Ice): Text { text: "Element: Ice"; }
                        }
                    }
                }
                "#
            },
        ),
    ];

    let expected_output = [
        generated_file(
            "internal.slint",
            indoc! {
                r#"
                import { CharacterProps } from "../props.slint";

                export enum RouteOption {
                    Character,
                }
                export global RouteState {
                    in property <RouteOption> active;
                    in-out property <CharacterProps> character-props;
                }
                "#
            },
        ),
        generated_file(
            "navigate.slint",
            indoc! {
                r#"
                import { Element, PowerStats } from "../props.slint";

                export enum Animate {
                    Forward,
                    Backward,
                    None,
                }

                struct NavigateOptions {
                    replace: bool,
                    animate: Animate,
                }

                export struct CharacterParams {
                    element: Element,
                    stats: PowerStats,
                }

                export global Navigate {
                    in property <bool> has-backward;
                    in property <bool> has-forward;

                    callback backward();
                    callback forward();

                    callback backward-animate(Animate);
                    callback forward-animate(Animate);

                    callback return-home();
                    callback return-home-animate(Animate);

                    callback debug();

                    callback navigate(string, NavigateOptions);

                    callback character(CharacterParams, NavigateOptions);
                }

                export global RoutePath {
                    pure callback character(CharacterParams) -> string;
                }
                "#
            },
        ),
        generated_file(
            "exports.slint",
            indoc! {
                r#"
                import { Navigate, RoutePath } from "navigate.slint";
                import { RouteOption, RouteState } from "internal.slint";

                export { Navigate, RoutePath, RouteOption, RouteState }
                "#
            },
        ),
        generated_file(
            "router.slint",
            indoc! {
                r#"
                import { CharacterPage } from "../page.slint";
                import { RouteState, RouteOption } from "internal.slint";

                export component Router inherits Rectangle {
                    if (RouteState.active == RouteOption.Character): CharacterPage {
                        props <=> RouteState.character-props;
                    }
                }
                "#
            },
        ),
    ];

    let _ = no_errors_or_warnings(input_files, expected_output);
}

fn generated_file(path: impl Into<std::path::PathBuf>, content: impl Into<String>) -> GeneratedFile {
    GeneratedFile { path: path.into(), content: content.into() }
}

#[track_caller]
fn no_errors_or_warnings(
    input: impl Into<Vec<GeneratedFile>>,
    expected: impl Into<[GeneratedFile; 4]>,
) -> RouterOutput {
    #[track_caller]
    fn test_output(result: &RouterOutput) {
        if !result.errors.errors.is_empty() {
            let report: miette::Report = result.errors.clone().into();
            panic!("Found validation errors: {:?}", report);
        }
    }

    router_test(SlintTestApp {
        slint_out: "gen",
        input_files: input.into(),
        expected_output: expected.into(),
        test_output,
    })
}

struct SlintTestApp {
    slint_out: &'static str,
    input_files: Vec<GeneratedFile>,
    expected_output: [GeneratedFile; 4],
    test_output: fn(&RouterOutput),
}

#[track_caller]
fn router_test(test_app: SlintTestApp) -> RouterOutput {
    let temp_dir = temp_dir::TempDir::new().unwrap();

    for file in test_app.input_files {
        file.write(temp_dir.path()).unwrap();
    }

    let slint_out = test_app.slint_out;
    let (result, generated) = generate_test_files(temp_dir.path(), slint_out).expect("no fatal errors");

    for file in generated.iter() {
        println!("{}", file.content);
    }
    (test_app.test_output)(&result);

    for file in generated.iter() {
        let expected = test_app.expected_output.iter().find(|f| f.path == file.path).unwrap_or_else(|| {
            panic!("Generated file '{}' not found in expected output files", file.path.display())
        });

        let mut expected_content = get_lines(&expected.content);
        let mut actual_content = get_lines(&file.content);

        while let (Some(expected_line), Some(actual_line)) = (expected_content.next(), actual_content.next())
        {
            assert_eq!(
                expected_line,
                actual_line,
                "file {} does not match expected output\n\n{}",
                file.path.display(),
                file.content
            );
        }

        assert!(expected_content.next().is_none());
        if actual_content.next().is_some() {
            panic!("file {} does not match expected output\n\n{}", file.path.display(), file.content);
        }
    }

    let app_mounted_router = generated_file(
        "app.slint",
        formatdoc! {
            r#"
            import {{ Router }} from "{slint_out}/router.slint";

            export component App inherits Window {{
                Router {{}}
            }}
            "#
        },
    );

    app_mounted_router.write(temp_dir.path()).unwrap();

    let slint_loader = validate::load::load_slint_file(&temp_dir.path().join("app.slint"));

    if let Err(e) = slint_loader {
        let report: miette::Report = e.into();
        panic!("Error loading app.slint: {:?}", report);
    }

    result
}

fn generate_test_files(
    path: &Path,
    slint_out: &'static str,
) -> Result<(RouterOutput, Vec<GeneratedFile>), RouteError> {
    let slint_out = path.join(slint_out);
    let mut output = RouterOutput::new(path)?;
    validate::build_stage_one(&mut output)?;

    let gen_config = GenerateConfig { root_slint: slint_out, out_dir: path.into() };

    let ctx = (&output, &gen_config).into();
    let stage_one = generate::slint::generate_navigate(ctx);
    {
        for file in stage_one.iter() {
            file.write(&gen_config.root_slint).unwrap();
        }
    }

    validate::build_stage_two(&mut output)?;

    let ctx = (&output, &gen_config).into();
    let stage_two = generate::slint::generate_router(ctx);
    {
        for file in stage_two.iter() {
            file.write(&gen_config.root_slint).unwrap();
        }
    }

    Ok((output, [&stage_one[..], &stage_two[..]].concat()))
}

fn get_lines(s: &str) -> impl Iterator<Item = &str> {
    s.lines().map(|line| line.trim()).filter(|line| {
        !line.is_empty()
            && !line.starts_with("//")
            && !line.starts_with("/*")
            && !line.starts_with("*")
            && !line.starts_with("/**")
    })
}
