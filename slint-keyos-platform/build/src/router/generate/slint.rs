// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;
use std::path::PathBuf;

use itertools::Itertools;

use super::GenContext;
use crate::{
    generated_file::GeneratedFile,
    router::validate::{common::type_string, RouteProps, RouterPage},
    source::{uwrite, uwriteln, Source},
};

static NAVIGATION_FILE: &str = "navigate.slint";
static INTERNAL_FILE: &str = "internal.slint";
static ROUTER_COMPONENT: &str = "router.slint";

const INTERNAL_WARNING: &str = "// INTERNAL ROUTER COMPONENT. DO NOT USE.";

pub fn generate_and_write_navigate(ctx: GenContext) -> Result<(), std::io::Error> {
    let files = generate_navigate(ctx);
    ctx.write_generated(files)
}

pub fn generate_and_write_router(ctx: GenContext) -> Result<(), std::io::Error> {
    let files = generate_router(ctx);
    ctx.write_generated(files)
}

pub fn generate_navigate(ctx: GenContext) -> [GeneratedFile; 2] {
    [ctx.create_internal(), ctx.create_navigate()]
}

pub fn generate_router(ctx: GenContext) -> [GeneratedFile; 1] { [ctx.create_router_component()] }

impl<'a> GenContext<'a> {
    fn write_generated<const N: usize>(&self, files: [GeneratedFile; N]) -> Result<(), std::io::Error> {
        for f in files {
            f.write(&self.config.root_slint)?;
        }

        Ok(())
    }

    fn create_navigate(&self) -> GeneratedFile {
        let mut src = Source::default();

        self.prop_field_imports(&mut src);

        uwriteln! {src,
            "
            export enum Animate {{
                Forward,
                Backward,
                None,
            }}
            "
        }

        uwrite!(
            src,
            "struct NavigateOptions {{
                // Replace the current route in the history stack
                replace: bool,
                
                // Animation to use when navigating
                animate: Animate,
            }}"
        );

        src.newline();
        src.newline();

        for p in self.data.pages.iter() {
            if !p.is_static_route() {
                uwrite!(src, "export struct ");
                p.slint_navigation_struct(&mut src);
                uwriteln!(src, " {{");

                for field in p.props.iter().flat_map(|p| p.fields.iter()) {
                    let key = field.key.base.as_str();
                    let ty = type_string(&field.ty);
                    uwriteln!(src, "{key}: {ty}, ");
                }

                uwriteln!(src, "}}");
                src.newline();
            }
        }

        uwrite! {
            src,
            "
            export global Navigate {{
               // Navigation state
               in property <bool> has-backward;
               in property <bool> has-forward;
    
               // Generic navigation callbacks
               callback backward();
               callback forward();

               callback backward-animate(Animate);
               callback forward-animate(Animate);

               callback return-home();
               callback return-home-animate(Animate);

               // Log the current navigation state
               callback debug();

               // Raw string navigation
               callback navigate(string, NavigateOptions);

               //
               // Type safe navigation callbacks
               //
            "
        }

        src.newline();

        for (i, p) in self.data.pages.iter().enumerate() {
            src.push_str("// ");
            p.example_route(&mut src);
            src.newline();

            p.slint_callback_decl(&mut src);
            src.newline();

            // Conform to the formatter: don't add newline on last iteration
            if i < self.data.pages.len() - 1 {
                src.newline();
            }
        }

        uwriteln!(src, "}}");
        src.newline();

        uwriteln!(src, "export global RoutePath {{");
        for p in self.data.pages {
            p.slint_path_property(&mut src);
            src.newline();
        }
        uwriteln!(src, "}}");
        src.newline();

        GeneratedFile { path: NAVIGATION_FILE.into(), content: src.into() }
    }

    fn create_internal(&self) -> GeneratedFile {
        let mut src = Source::default();

        for import in prop_imports(self.data.pages.iter()) {
            uwriteln!(src, "{}", import);
        }

        uwriteln!(src, "");
        uwrite! {src,
            "
            {INTERNAL_WARNING}
            export enum RouteOption {{
            "
        }
        for p in self.data.pages {
            uwriteln!(src, "{},", p.names.pascal);
        }
        uwriteln!(src, "}}");

        uwrite! {src,
            "
            {INTERNAL_WARNING}
            export global RouteState {{
                in property <RouteOption> active;
            "
        }

        for p in unique_props(self.data.pages.iter()) {
            uwriteln!(src, "in-out property <{}> {};", p.export_name.name, p.names.kebab);
        }

        uwriteln!(src, "}}");

        GeneratedFile { path: INTERNAL_FILE.into(), content: src.into() }
    }

    fn create_router_component(&self) -> GeneratedFile {
        let mut src = Source::default();

        let valid_pages = self.data.complete_pages();

        for (p, slint_data) in &valid_pages {
            let import = make_import(&slint_data.export_name.name, &p.slint_import_path);
            uwriteln!(src, "{}", import);
        }

        uwriteln!(src, "import {{ RouteState, RouteOption }} from \"{INTERNAL_FILE}\";");
        uwriteln!(src, "");

        uwrite!(
            src,
            "
            // Mount this component in your app to enable routing
            export component Router inherits Rectangle {{
            "
        );

        for (p, slint_data) in &valid_pages {
            uwriteln!(
                src,
                "if (RouteState.active == RouteOption.{enum_name}): {page_name} {{",
                enum_name = p.names.pascal,
                page_name = slint_data.export_name.name.as_str()
            );

            slint_data
                .property_decls
                .iter()
                .map(|d| {
                    let prop = p.props.iter().find(|p| p.export_name.name == d.name).unwrap();
                    (prop, d)
                })
                .for_each(|(prop, d)| uwriteln!(src, "{} <=> RouteState.{};", d.key, prop.names.kebab));

            uwriteln!(src, "}}");
        }

        uwriteln!(src, "}}");

        GeneratedFile { path: ROUTER_COMPONENT.into(), content: src.into() }
    }

    fn prop_field_imports(&self, src: &mut Source) {
        // Create a HashMap to group names by their import path
        let mut imports: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

        self.data
            .pages
            .iter()
            .flat_map(|p| &p.props)
            .flat_map(|props| props.fields.iter())
            .filter_map(|field| {
                field
                    .slint_import_path
                    .as_ref()
                    .map(|path| (path.to_string(), field.name.as_ref().unwrap().to_string()))
            })
            .for_each(|(path, name)| {
                imports.entry(path).or_default().push(name);
            });

        // Write out grouped imports
        for (path, names) in imports.iter_mut() {
            names.sort(); // Sort names for consistent output
            names.dedup(); // Remove any duplicates
            let full_path = PathBuf::from("../").join(path);
            uwriteln!(src, "import {{ {} }} from \"{}\";", names.join(", "), full_path.display());
        }

        if !imports.is_empty() {
            uwriteln!(src, "");
        }
    }
}

fn unique_props<'a>(
    pages: impl Iterator<Item = &'a RouterPage> + 'a,
) -> impl Iterator<Item = &'a RouteProps> + 'a {
    pages.flat_map(|p| &p.props).unique_by(|p| &p.export_name.name)
}

fn prop_imports<'a>(pages: impl Iterator<Item = &'a RouterPage> + 'a) -> impl Iterator<Item = String> + 'a {
    unique_props(pages).map(|p| make_import(&p.export_name.name, &p.slint_import_path))
}

fn make_import(name: &str, path: &str) -> String {
    format!("import {{ {} }} from \"{}\";", name, PathBuf::from("../").join(path).display())
}
