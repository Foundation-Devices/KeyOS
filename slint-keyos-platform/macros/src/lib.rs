// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

extern crate proc_macro;

mod route;

use proc_macro::TokenStream;

/// Generates custom route serialization/deserialization code
/// Can be used within slint components to generate serialization of route parameters.
///
/// Simple usage:
///
/// Separate path from query parameters with '?'
///
/// Separate query parameters with '&'
///
/// ```rust,ignore
/// #[route(path = "/user/{id}?{profile}&{value}")]
/// struct SimpleRoute {
///     id: u32,
///     profile: String,
///     value: String,
/// }
/// ```
///
/// You can nest any struct or enum as a route parameter.
///
/// All fields must implement serde::Serialize and serde::Deserialize
///
/// ```rust,ignore
/// #[route(path = "/{user}/settings/{ids}?{nested}&{option}")]
/// struct TestRoute {
///     user: String,
///     ids: Vec<u32>,
///     nested: Vec<TestNested>,
///     option: EnumNested,
/// }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct TestNested {
///     a: u32,
///     b: String,
/// }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// enum EnumNested {
///     One,
///     Two,
/// }
/// ```
#[proc_macro_attribute]
pub fn route(attr: TokenStream, root_item: TokenStream) -> TokenStream { route::expand(attr, root_item) }
