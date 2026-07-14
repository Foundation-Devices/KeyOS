// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use i_slint_compiler::{
    diagnostics::{ByteFormat, Spanned},
    langtype::{Struct, StructName, Type},
};
use miette::SourceOffset;

pub(crate) fn make_source_offset(src: impl AsRef<str>, node: &impl Spanned) -> SourceOffset {
    let source_file = node.source_file().expect("source file");
    let span = node.span();
    let (line, col) = source_file.line_column(span.offset, ByteFormat::Utf8);
    SourceOffset::from_location(src.as_ref(), line, col)
}

// The user-declared struct name, if any; builtin and anonymous structs have none.
pub(crate) fn struct_name(s: &Struct) -> Option<&str> {
    match &s.name {
        StructName::User { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

pub(crate) fn type_string(ty: &Type) -> String {
    match ty {
        Type::Float32 => "float".into(),
        Type::Int32 => "int".into(),
        Type::String => "string".into(),
        Type::Duration => "duration".into(),
        Type::Angle => "angle".into(),
        Type::PhysicalLength => "physical-length".into(),
        Type::LogicalLength => "length".into(),
        Type::Rem => "relative-font-size".into(),
        Type::Percent => "percent".into(),
        Type::Color => "color".into(),
        Type::Image => "image".into(),
        Type::Bool => "bool".into(),
        Type::Model => "model".into(),
        Type::Array(t) => format!("[{}]", type_string(t)),
        Type::Struct(s) => {
            if let Some(name) = s.name.slint_name() {
                name.to_string()
            } else {
                unreachable!()
            }
        }
        Type::PathData => "pathdata".into(),
        Type::Easing => "easing".into(),
        Type::Brush => "brush".into(),
        Type::Enumeration(enumeration) => enumeration.name.to_string(),

        Type::Invalid => unreachable!(),
        Type::Void => unreachable!(),
        Type::InferredProperty => unreachable!(),
        Type::InferredCallback => unreachable!(),
        Type::Callback { .. } => unreachable!(),
        Type::Function { .. } => unreachable!(),
        Type::ComponentFactory => unreachable!(),
        Type::UnitProduct(_) => unreachable!(),
        Type::ElementReference => unreachable!(),
        Type::LayoutCache => unreachable!(),
        Type::Keys => unreachable!(),
        Type::DataTransfer => unreachable!(),
        Type::ArrayOfU16 => unreachable!(),
        Type::StyledText => unreachable!(),
    }
}

pub(crate) fn slint_import_path(root_path: &Path, full_path: &Path) -> String {
    match full_path.strip_prefix(root_path).ok() {
        Some(path) => path.to_string_lossy().to_string(),
        None => full_path.to_string_lossy().to_string(),
    }
}
