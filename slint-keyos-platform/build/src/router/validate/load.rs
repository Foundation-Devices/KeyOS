// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use i_slint_compiler::{
    diagnostics::BuildDiagnostics,
    generator::OutputFormat,
    langtype::Type,
    object_tree::{Component, ExportedName},
    typeloader::TypeLoader,
    typeregister::TypeRegister,
    CompilerConfiguration,
};
use miette::{Context, IntoDiagnostic};
use slint_keyos_platform_common::utils;

use super::error::{MultiFileErrorList, RouteError};

pub fn load_slint_file(path: &Path) -> Result<SlintDoc, RouteError> {
    let mut diag = BuildDiagnostics::default();
    let (source, doc_node) = {
        let source = std::fs::read_to_string(path)
            .into_diagnostic()
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let node = i_slint_compiler::parser::parse(source.clone(), Some(path), &mut diag);

        let source = Arc::new(source);

        diag_error(&diag)?;

        (source, node)
    };

    let (doc, diagnostics) = quick_compile(doc_node, diag);

    diag_error(&diagnostics)?;

    let doc = SlintDoc::new(source, doc);

    Ok(doc)
}

#[derive(Debug, Clone)]
pub struct SlintDoc {
    pub src: Arc<String>,
    pub export_components: Vec<(ExportedName, Rc<Component>)>,
    pub export_types: Vec<(ExportedName, Type)>,
}

impl SlintDoc {
    pub fn new(src: Arc<String>, doc: i_slint_compiler::object_tree::Document) -> Self {
        let exports = doc.exports.into_iter().partition::<Vec<_>, _>(|(_, e)| e.is_left());
        let export_components = exports.0.into_iter().map(|(n, e)| (n, e.left().unwrap())).collect();
        let export_types = exports.1.into_iter().map(|(n, e)| (n, e.right().unwrap())).collect();

        Self { src, export_components, export_types }
    }
}

fn diag_error(diagnostics: &BuildDiagnostics) -> Result<(), RouteError> {
    let mut loaded_files = HashMap::<PathBuf, Arc<String>>::new();
    let mut load_file = |path: &Path| -> Result<Arc<String>, RouteError> {
        match loaded_files.entry(path.to_path_buf()) {
            std::collections::hash_map::Entry::Occupied(occupied_entry) => Ok(occupied_entry.get().clone()),
            std::collections::hash_map::Entry::Vacant(vacant_entry) => {
                let src = std::fs::read_to_string(path)
                    .into_diagnostic()
                    .with_context(|| format!("Failed to read file: {}", path.display()))?;
                let src = Arc::new(src);
                vacant_entry.insert(src.clone());
                Ok(src)
            }
        }
    };
    if diagnostics.has_errors() {
        let errors = diagnostics
            .iter()
            .filter(|d| d.level() == i_slint_compiler::diagnostics::DiagnosticLevel::Error)
            .cloned();

        let errors = errors
            .map(|e| {
                let path = e.source_file().expect("has source");
                let (line, col) = e.line_column();
                let src = load_file(path)?;
                let offset = miette::SourceOffset::from_location(&*src, line, col);
                let span = miette::SourceSpan::new(offset, 0);

                Ok(super::error::SourceError::single(
                    e.message(),
                    miette::NamedSource::new(path.display().to_string(), src.clone()),
                    miette::LabeledSpan::new_with_span(None, span),
                    None,
                ))
            })
            .collect::<Result<Vec<_>, RouteError>>()?;

        Err(MultiFileErrorList::new("Slint compilation failed", None::<String>, errors).into())
    } else {
        Ok(())
    }
}

/// a quick compile that only does minimal work to validate pages and props for router
/// based off [`i_slint_compiler::compile_syntax_node`]
fn quick_compile(
    syntax_node: i_slint_compiler::parser::SyntaxNode,
    diag: BuildDiagnostics,
) -> (i_slint_compiler::object_tree::Document, BuildDiagnostics) {
    COMPILER_CTX.with(|cx| {
        let mut cx = cx.borrow_mut();
        spin_on::spin_on(cx.quick_compile(syntax_node, diag))
    })
}

thread_local! {
    static COMPILER_CTX: RefCell<CompilerContext> = RefCell::new(CompilerContext::new({
        let mut config = CompilerConfiguration::new(OutputFormat::Rust);
        config.library_paths.extend(utils::library_paths());
        config
    }));
}

#[allow(unused)]
struct CompilerContext {
    config: CompilerConfiguration,
    loader: TypeLoader,
}

impl CompilerContext {
    fn new(config: CompilerConfiguration) -> Self {
        let loader = {
            let mut diag = BuildDiagnostics::default();
            let loader = TypeLoader::new(config.clone(), &mut diag);
            if diag.has_errors() {
                panic!("failed to create loader");
            }
            loader
        };

        Self { config, loader }
    }

    async fn quick_compile(
        &mut self,
        doc_node: i_slint_compiler::parser::SyntaxNode,
        mut diag: BuildDiagnostics,
    ) -> (i_slint_compiler::object_tree::Document, BuildDiagnostics) {
        let loader = &mut self.loader;

        let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));

        let doc: i_slint_compiler::parser::syntax_nodes::Document = doc_node.into();

        let (foreign_imports, reexports) =
            loader.load_dependencies_recursively(&doc, &mut diag, &registry).await;

        let mut doc = i_slint_compiler::object_tree::Document::from_node(
            doc,
            foreign_imports,
            reexports,
            &mut diag,
            &registry,
            false,
        );

        i_slint_compiler::passes::run_minimal_typecheck_passes(&mut doc, loader, &mut diag);

        (doc, diag)
    }
}
