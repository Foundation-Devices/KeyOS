// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub(crate) mod common;
pub(crate) mod error;
pub(crate) mod file_finder;
pub(crate) mod load;
mod model;

use std::sync::Arc;
use std::{collections::BTreeMap, path::Path};

use i_slint_compiler::langtype::Type;
use miette::{LabeledSpan, NamedSource};
pub use model::*;
use slint_keyos_platform_common::analyze_path::AnalyzedPath;
use {
    common::{slint_import_path, struct_name},
    error::{MultiFileErrorList, RouteErrorList, SourceError},
    file_finder::find_props_iter,
    load::load_slint_file,
};

use crate::router::validate::{error::RouteError, file_finder::find_page_iter};

pub fn build_stage_one(output: &mut RouterOutput) -> Result<(), RouteError> {
    build_router_props(output)?;
    build_router_pages(output)?;
    Ok(())
}

pub fn build_stage_two(output: &mut RouterOutput) -> Result<(), RouteError> {
    add_slint_data_to_pages(output);
    Ok(())
}

// Validates all props files
fn build_router_props(output: &mut RouterOutput) -> Result<(), RouteError> {
    for props_path in find_props_iter(&output.root_path) {
        match props_path {
            Ok(props_path) => match validate_props(&output.root_path, &props_path) {
                Ok(props) => output.valid_props.push(props),
                Err(error) => output.errors.extend(error),
            },
            Err(e) => return Err(e.into()),
        }
    }

    output.valid_props.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(())
}

// Validates all pages, without slint compilation
fn build_router_pages(output: &mut RouterOutput) -> Result<(), RouteError> {
    for page_path in find_page_iter(&output.root_path) {
        match page_path {
            Ok(page_path) => match build_page(&output.root_path, &page_path, &output.valid_props) {
                Ok(page) => {
                    output.valid_pages.push(page);
                }
                Err(error) => output.errors.push(error),
            },
            Err(e) => return Err(e.into()),
        }
    }

    if let Err(e) = ensure_valid_default(&output.valid_pages) {
        output.errors.push(e);
    }

    for page in &output.valid_pages {
        if let Err(e) = ensure_properties_unique_keys(page) {
            output.errors.extend(e);
        }
    }

    // Default must be first for codegen
    output.valid_pages.sort_by(|a, b| {
        // if either is default that one goes first, otherwise compare by name
        if a.is_default() {
            std::cmp::Ordering::Less
        } else if b.is_default() {
            std::cmp::Ordering::Greater
        } else {
            if let Some((a_last, b_last)) = a.props.last().zip(b.props.last()) {
                a_last.path.cmp(&b_last.path)
            } else {
                a.names.pascal.cmp(&b.names.pascal)
            }
        }
    });

    Ok(())
}

// Handle all Slint-related validation
fn add_slint_data_to_pages(output: &mut RouterOutput) {
    for page in &mut output.valid_pages {
        match validate_page_slint(&page.path, &output.valid_props, &page.page_name) {
            Ok(data) => {
                page.add_slint_data(data);
            }
            Err(e) => {
                output.errors.push(e);
            }
        };
    }
}

fn validate_props(root: &Path, path: &Path) -> Result<RouteProps, RouteErrorList> {
    let document = load_slint_file(path)?;

    let (export_name, props_struct) = document
        .export_types
        .into_iter()
        .find_map(|(name, export)| match export {
            Type::Struct(s) if name.name.ends_with("Props") => Some((name, s)),
            _ => None,
        })
        .ok_or_else(|| {
            SourceError::missing_export(
                "Props export struct not found",
                path,
                document.src.clone(),
                None::<String>,
                Some("Ensure you are exporting a struct with a name ending in 'Props'"),
            )
        })?;

    // @rust-attr annotations live on the parent StructDeclaration node, not the
    // Struct type, so walk up from the object-type node to read them. An empty
    // list here surfaces later as a MissingRouteAttribute from validation.
    let rust_attributes = props_struct
        .node()
        .and_then(|node| {
            node.parent().and_then(i_slint_compiler::parser::syntax_nodes::StructDeclaration::new)
        })
        .map(|decl| decl.AtRustAttr().map(|attr| attr.text().to_string()).collect::<Vec<String>>())
        .unwrap_or_default();

    let fields = props_struct
        .fields
        .iter()
        .map(|(key, value)| (StringCases::new(key.to_string()), value))
        .collect::<Vec<_>>();

    let field_keys = fields.iter().map(|(key, _)| key.snake.clone()).collect::<Vec<_>>();

    let route_attr = match validate_route_attribute(rust_attributes, field_keys) {
        Ok(route) => Ok(route),
        Err(RouteValidationError::MissingRouteAttribute) => Err(SourceError::props(
            &export_name,
            path,
            document.src.clone(),
            "Missing `route` attribute on props struct",
            Some("Missing route attribute"),
            Some("Add `@rust-attr(route(path = \"...\"))` to the struct"),
        )),
        Err(RouteValidationError::MissingPath) => Err(SourceError::props(
            &export_name,
            path,
            document.src.clone(),
            "Missing `path` attribute on props struct",
            Some("Missing path attribute"),
            Some("Add `@rust-attr(route(path = \"...\"))` to the struct"),
        )),
        Err(RouteValidationError::Invalid(e)) => Err(SourceError::props(
            &export_name,
            path,
            document.src.clone(),
            format!("Invalid path: {}", e),
            Some("Invalid Route Path"),
            None::<String>,
        )),
    }?;

    // Ensure that all props struct/enum fields have route annotation.
    // TODO: should we add this validation? right now slint doesn't support serializing lists/models
    // props_struct.fields
    //       .iter()
    //       .filter_map(|(_key, field)| match &field {
    //           Type::Struct(s) => {
    //               match s.rust_attributes {
    //                   Some(_) => None,
    //                   None => Some(SourceError::from_node(
    //                       &s.node.clone().expect("struct node"),
    //                       document.src.clone(),
    //                       path,
    //                       "Missing `serde` attributes on field struct",
    //                       Some("Missing serde derive attributes"),
    //                       Some("Add `@rust-attr(derive(serde::Serialize, serde::Deserialize))` to the
    // struct definition"),                   )),
    //               }
    //           }
    //           Type::Enumeration(e) => {
    //               let node = e.node.as_ref().expect("enum node");
    //               if node.AtRustAttr().is_some() {
    //                   None
    //               } else {
    //                   Some(SourceError::from_node(
    //                       node,
    //                       document.src.clone(),
    //                       path,
    //                       "Missing `serde` attributes on field enum",
    //                       Some("Missing serde derive attributes"),
    //                       Some("Add `@rust-attr(derive(serde::Serialize, serde::Deserialize))` to the enum
    // definition"),                   ))
    //               }
    //           }
    //           _ => None,
    //       })
    //       .collect::<RouteErrorList>()
    //       .into_result()?;

    Ok(RouteProps {
        default: route_attr.default,
        analyzed_path: route_attr.path,
        names: StringCases::new(export_name.name.clone()),
        export_name,
        fields: fields
            .into_iter()
            .map(|(key, value)| PropsField::new(root, key.base, value.clone()))
            .collect(),
        src: document.src,
        path: path.to_path_buf(),
        slint_import_path: slint_import_path(root, path),
    })
}

fn build_page(root: &Path, page_path: &Path, valid_props: &[RouteProps]) -> Result<RouterPage, RouteError> {
    // Get parent directory
    let parent = page_path.parent().ok_or_else(|| {
        RouteError::Unexpected(Arc::new(miette::miette!(
            code = "router::path",
            help = "Ensure the page file is inside a directory and not at the root",
            "Cannot get parent directory of path: {}",
            page_path.display()
        )))
    })?;

    let props_path = parent.join("props.slint");

    // Find corresponding props
    let page_props = valid_props.iter().find(|p| p.path == props_path).ok_or_else(|| {
        RouteError::Unexpected(Arc::new(miette::miette!(
            code = "router::props",
            help = "Ensure there is a props.slint file in the same directory",
            "Props file not found at: {}",
            props_path.display()
        )))
    })?;

    let base_page_name = format!("{}", page_props.export_name.name.trim_end_matches("Props"));

    let all_props = {
        let mut props = valid_props
            .iter()
            .filter(|p| {
                p.path
                    .parent()
                    .and_then(|props_parent| {
                        page_path.parent().map(|page_parent| {
                            page_parent.ancestors().any(|ancestor| ancestor == props_parent)
                        })
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();

        props.sort_by_key(|p| p.path.components().count());
        props
    };

    Ok(RouterPage::new(
        base_page_name,
        all_props,
        page_path.to_path_buf(),
        slint_import_path(root, page_path),
    ))
}

fn validate_page_slint(
    path: &Path,
    props: &[RouteProps],
    page_name: &str,
) -> Result<PageSlintData, RouteError> {
    // Load and parse the Slint file
    let document = match load_slint_file(path) {
        Ok(doc) => doc,
        Err(e) => {
            return Err(e);
        }
    };

    // Find the page component export
    let (exported_name, page) =
        document.export_components.into_iter().find(|(name, _)| name.name == page_name).ok_or_else(|| {
            RouteError::Single(SourceError::missing_export(
                "Page component not found",
                path,
                document.src.clone(),
                Some("No Page Export Found!"),
                Some(format!("Ensure you are exporting a component '{page_name}'")),
            ))
        })?;

    // Validate props references
    let (property_decls, errors) = page
        .root_element
        .borrow()
        .property_declarations
        .iter()
        .filter(|(_, p)| p.expose_in_public_api)
        .filter_map(|(key, decl)| {
            if let Type::Struct(s) = &decl.property_type {
                Some((key.to_string(), decl, s))
            } else {
                None
            }
        })
        .fold((Vec::new(), Vec::new()), |(mut successes, mut failures), (key, decl, s)| {
            if let Some(name) = struct_name(s).map(|name| name.to_string()) {
                if !props.iter().any(|p| p.export_name.name == name) {
                    failures.push(SourceError::from_node(
                        decl.type_node().as_ref().unwrap_or(&exported_name.name_ident),
                        document.src.clone(),
                        path.to_path_buf(),
                        format!("Props struct '{}' not found", name),
                        None::<String>,
                        None::<String>,
                    ));
                } else {
                    successes.push(PropertyDecl { name, key, decl: decl.clone() });
                }
            } else {
                failures.push(SourceError::page(
                    page.clone(),
                    document.src.clone(),
                    path.to_path_buf(),
                    "Cannot use anonymous structs as props",
                    None::<String>,
                    None::<String>,
                ));
            }
            (successes, failures)
        });

    if errors.is_empty() {
        Ok(PageSlintData { export_name: exported_name, src: document.src, component: page, property_decls })
    } else {
        let available_props = props
            .iter()
            .map(|p| format!("- {} ({})", p.export_name.name, p.path.display()))
            .collect::<Vec<_>>()
            .join("\n");

        let help_message = format!(
            indoc::indoc! {"
                    Ensure all props references exist and are valid, or make them private

                    Available public props:
                    {}
                "},
            available_props
        );

        Err(RouteError::from(MultiFileErrorList::new(
            "Invalid props references found",
            Some(help_message),
            errors,
        )))
    }
}

// Add this function after validate_page
fn ensure_valid_default(pages: &[RouterPage]) -> Result<(), RouteError> {
    let (default, non_default): (Vec<_>, Vec<_>) = pages.iter().partition(|page| page.is_default());

    // Ensure a default exists
    if default.is_empty() {
        let errors = non_default
            .iter()
            .flat_map(|page| {
                page.props.iter().map(|props| {
                    let src = NamedSource::new(props.path.to_string_lossy(), props.src.clone());
                    let span = LabeledSpan::new_with_span(None, props.make_span());
                    SourceError::single("Default Page Option", src, span, None)
                })
            })
            .collect::<Vec<_>>();

        return Err(MultiFileErrorList::new(
                "No Default Page found",
                Some("Mark a page as default using the route attribute:\n'@rust-attr(route(default, path = \"...\"))'"),
                errors,
            ).into());
    }

    // Ensure only one default
    if default.len() > 1 {
        let errors = default
            .iter()
            .flat_map(|page| {
                page.props.iter().rev().find(|p| p.default).map(|props| {
                    let src = NamedSource::new(props.path.to_string_lossy(), props.src.clone());
                    let span = LabeledSpan::new_with_span(None, props.make_span());
                    SourceError::single("found default page", src, span, None)
                })
            })
            .collect::<Vec<_>>();

        return Err(MultiFileErrorList::new(
                format!("found {} default pages, but only one is allowed", default.len()),
                Some("remove the 'default' flag from all but one route attribute:\n`@rust-attr(route(default, path = \"...\"))`"),
                errors,
            ).into());
    }

    Ok(())
}

fn ensure_properties_unique_keys(page: &RouterPage) -> Result<(), RouteErrorList> {
    let mut keys = std::collections::BTreeMap::new();
    let all_fields = page.props.iter().flat_map(|prop| prop.fields.iter().map(move |field| (prop, field)));

    let mut errors: BTreeMap<&str, MultiFileErrorList> = BTreeMap::new();

    for (prop, field) in all_fields {
        let key = field.key.base.as_str();
        if let Some((prev_prop, _)) = keys.insert(key, (prop, field)) {
            let message = format!("Field: {key}");
            let errors = errors.entry(field.key.base.as_str()).or_insert_with(|| {
                let prev_error = SourceError::from_node(
                    &prev_prop.export_name.name_ident,
                    prev_prop.src.clone(),
                    &prev_prop.path,
                    message.clone(),
                    Some("First occurrence"),
                    None::<String>,
                );

                MultiFileErrorList::new(
                    format!("Duplicate field key: {key}"),
                    Some("Ensure unique field keys in page properties"),
                    vec![prev_error],
                )
            });

            let error = SourceError::from_node(
                &prop.export_name.name_ident,
                prop.src.clone(),
                &prop.path,
                message,
                Some("Duplicate key"),
                None::<String>,
            );

            errors.push(error);
        }
    }

    errors.into_values().collect::<RouteErrorList>().into_result()
}

#[derive(Debug, Clone, PartialEq)]
struct AnalyzedRoute {
    default: bool,
    path: AnalyzedPath,
}

#[derive(Debug, PartialEq)]
enum RouteValidationError {
    MissingRouteAttribute,
    MissingPath,
    Invalid(slint_keyos_platform_common::analyze_path::PathError),
}

fn validate_route_attribute(
    rust_attributes: Vec<String>,
    fields: Vec<String>,
) -> Result<AnalyzedRoute, RouteValidationError> {
    let macro_args = rust_attributes
        .iter()
        .find(|attr| attr.trim_start().starts_with("route("))
        .map(|attr| attr.trim())
        .map(|attr| attr.trim_start_matches("route(").trim_end_matches(')'))
        .ok_or(RouteValidationError::MissingRouteAttribute)?;

    let path = macro_args
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .find(|s| s.starts_with("path"))
        .map(|s| s.split('=').nth(1).unwrap_or("").trim().trim_matches('"'))
        .ok_or(RouteValidationError::MissingPath)?;

    let path = slint_keyos_platform_common::analyze_path::validate(path, fields)
        .map_err(RouteValidationError::Invalid)?;

    let default = macro_args.split(',').map(|s| s.trim()).any(|s| s == "default");

    Ok(AnalyzedRoute { default, path })
}

#[cfg(test)]
mod tests {
    use slint_keyos_platform_common::analyze_path::{AnalyzedPath, Segment};

    use super::*;

    #[test]
    fn default_valid() {
        let rust_attributes = vec!["route(default, path = \"/settings/{setting_id}\")".to_string()];
        let fields = vec!["setting_id".to_string()];
        let result = validate_route_attribute(rust_attributes, fields);
        let analyzed_route = result.unwrap();
        assert_eq!(
            analyzed_route,
            AnalyzedRoute {
                default: true,
                path: AnalyzedPath {
                    path: vec![
                        Segment::Static("settings".to_string()),
                        Segment::Capture("setting_id".to_string())
                    ],
                    query: vec![],
                }
            }
        );
    }

    #[test]
    fn non_default() {
        let rust_attributes = vec!["route(path = \"/settings\")".to_string()];
        let fields = vec![];
        let result = validate_route_attribute(rust_attributes, fields);
        let analyzed_route = result.unwrap();
        assert_eq!(
            analyzed_route,
            AnalyzedRoute {
                default: false,
                path: AnalyzedPath { path: vec![Segment::Static("settings".to_string()),], query: vec![] }
            }
        );
    }

    #[test]
    fn missing_path() {
        let rust_attributes = vec!["route(default)".to_string()];
        let fields = vec!["setting_id".to_string()];
        let result = validate_route_attribute(rust_attributes, fields);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), RouteValidationError::MissingPath);
    }
}
