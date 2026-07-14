// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use i_slint_compiler::{
    diagnostics::Spanned,
    generator::{to_kebab_case, to_pascal_case},
    langtype::Type,
    object_tree::{Component, ExportedName},
};
use miette::SourceSpan;
use slint_keyos_platform_common::analyze_path::{AnalyzedPath, Segment};

use super::common::{make_source_offset, slint_import_path, struct_name};
use super::error::{RouteError, RouteErrorList};

#[derive(Debug, Default)]
pub struct RouterOutput {
    // canonicalized root path of router .slint files
    pub root_path: PathBuf,
    // Successfully validated props structs
    pub valid_props: Vec<RouteProps>,
    // Successfully validated pages
    pub valid_pages: Vec<RouterPage>,
    // All validation errors
    pub errors: RouteErrorList,
}

impl RouterOutput {
    pub fn new(root_path: &Path) -> Result<Self, RouteError> {
        let root_path = miette::Context::wrap_err_with(
            miette::IntoDiagnostic::into_diagnostic(root_path.canonicalize()),
            || format!("Failed to canonicalize root path: {}", root_path.display()),
        )?;

        Ok(Self {
            root_path,
            valid_props: Vec::new(),
            valid_pages: Vec::new(),
            errors: RouteErrorList::default(),
        })
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct RouterPage {
    // excludes Page suffix
    pub names: StringCases,

    // includes the Page suffix
    pub page_name: String,

    // Sort from least to most nested.
    pub props: Vec<RouteProps>,
    pub path: PathBuf,
    pub slint_import_path: String,

    // if none, failed slint compilation
    pub slint_data: Option<PageSlintData>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct PageSlintData {
    pub export_name: ExportedName,
    pub src: Arc<String>,
    pub component: Rc<Component>,
    pub property_decls: Vec<PropertyDecl>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct PropertyDecl {
    pub key: String,
    pub decl: i_slint_compiler::object_tree::PropertyDeclaration,
    pub name: String,
}

impl RouterPage {
    pub fn new(name: String, props: Vec<RouteProps>, path: PathBuf, slint_import_path: String) -> Self {
        let names = StringCases::new(name.as_str());
        let page_name = format!("{}Page", names.base);

        Self { names, page_name, props, path, slint_import_path, slint_data: None }
    }

    pub fn add_slint_data(&mut self, slint_data: PageSlintData) { self.slint_data = Some(slint_data); }

    pub fn is_static_route(&self) -> bool {
        self.props.iter().all(|p| {
            p.analyzed_path.path.iter().all(|segment| match segment {
                Segment::Static(_) => true,
                Segment::Capture(_) => false,
            }) && self.props.iter().all(|p| p.analyzed_path.query.is_empty())
        })
    }

    pub fn is_default(&self) -> bool { self.props.last().map_or(false, |p| p.default) }
}

#[derive(Debug, Clone)]
pub struct RouteProps {
    // #[route] macro content
    pub default: bool,
    pub analyzed_path: AnalyzedPath,

    pub export_name: ExportedName,
    pub names: StringCases,

    pub fields: Vec<PropsField>,

    pub src: Arc<String>,
    // Path to file where struct is exported.
    pub path: PathBuf,
    // String to use in slint codegen.
    pub slint_import_path: String,
}

impl RouteProps {
    pub fn make_span(&self) -> SourceSpan {
        let node = &self.export_name.name_ident;
        let offset = make_source_offset(&*self.src, node);
        SourceSpan::new(offset, 0)
    }

    pub fn rust_struct_construction(&self) -> String {
        let fields = self.fields.iter().map(|field| field.key.snake.as_str()).collect::<Vec<_>>().join(", ");

        format!(
            "let {var} = {name} {{ {fields} }};",
            var = self.names.snake,
            name = self.names.base,
            fields = fields
        )
    }
}

#[derive(Debug, Clone)]
pub struct PropsField {
    pub key: StringCases,
    pub ty: Type,

    // Only for struct and enum types.
    pub name: Option<String>,
    // String to use in slint codegen.
    pub slint_import_path: Option<String>,
}

impl PropsField {
    pub fn new(root: &Path, key: String, ty: Type) -> Self {
        let (name, node) = match &ty {
            Type::Struct(s) => {
                let node = s.node().expect("struct node");
                (struct_name(s).map(|n| n.to_string()), Some(node.to_source_location()))
            }
            Type::Enumeration(enumeration) => {
                let enum_node = enumeration.node.as_ref().expect("enum node");
                (Some(enumeration.name.to_string()), Some(enum_node.to_source_location()))
            }
            _ => (None, None),
        };

        let path = node.as_ref().map(|node| node.source_file().expect("source file").path());

        let slint_import_path = path.map(|path| slint_import_path(root, path));

        Self { key: StringCases::new(key), ty, name, slint_import_path }
    }
}

#[derive(Debug, Clone)]
pub struct StringCases {
    pub base: String,

    pub pascal: String,
    pub snake: String,
    pub kebab: String,
}

impl StringCases {
    pub fn new(base: impl Into<String>) -> Self {
        let base: String = base.into();

        let pascal = to_pascal_case(&base);
        let kebab = to_kebab_case(&base);
        let snake = kebab.replace('-', "_");

        Self { base, pascal, snake, kebab }
    }
}
