// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0
#![feature(proc_macro_tracked_path)]

use std::{
    collections::HashMap,
    path::PathBuf,
    str::FromStr,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use app_manifest::{ApiManifest, Manifest, MessageType};
use quote::quote;
use syn::{parse_macro_input, spanned::Spanned, Attribute};

#[proc_macro_derive(Server, attributes(name))]
pub fn derive_server_entry(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    with_manifest(|manifest| {
        derive_server(parse_macro_input!(item as syn::Item), manifest)
            .unwrap_or_else(|e| e.to_compile_error())
            .into()
    })
}

fn derive_server(item: syn::Item, manifest: &Manifest) -> Result<proc_macro2::TokenStream, syn::Error> {
    let (ident, attrs) = match &item {
        syn::Item::Enum(item_enum) => (&item_enum.ident, &item_enum.attrs),
        syn::Item::Struct(item_struct) => (&item_struct.ident, &item_struct.attrs),
        _ => return Err(syn::Error::new_spanned(item, "Item must be a struct or enum")),
    };
    let name = get_attr_value(attrs, "name").ok_or_else(|| {
        syn::Error::new_spanned(&item, concat!("Missing required `#[name = \"server-name\"]` attr."))
    })?;
    let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(name), .. }) = name else {
        return Err(syn::Error::new_spanned(item, "Name must be string literal"));
    };
    let name = name.value();

    let server = manifest
        .servers
        .get(&name)
        .ok_or_else(|| syn::Error::new_spanned(&item, concat!("Server not found in manifest")))?;
    let messages: Vec<_> = server
        .iter()
        .map(|(message_type, definition)| {
            let message_ident = syn::Ident::new(message_type, item.span());
            let handler = match definition.r#type {
                MessageType::Archive => "handle_archive_message",
                MessageType::BlockingArchive => "handle_blocking_archive_message",
                MessageType::ArchiveEvent => "handle_archive_subscription",
                MessageType::Scalar => "handle_scalar_message",
                MessageType::BlockingScalar => "handle_blocking_scalar_message",
                MessageType::ScalarEvent => "handle_scalar_subscription",
                MessageType::LendMut => "handle_lend_mut",
                MessageType::DeferredLendMut => "handle_deferred_lend_mut",
                MessageType::Move => "handle_move",
            };
            let handler = syn::Ident::new(handler, item.span());
            let cfg_attr = if let Some(cfg) = &definition.cfg {
                let Ok(cfg_tokens) = proc_macro2::TokenStream::from_str(cfg) else {
                    return Err(syn::Error::new_spanned(&item, "Invalid `cfg` value on message"));
                };
                quote!(#[cfg(#cfg_tokens)])
            } else {
                quote!()
            };
            Ok(quote!( #cfg_attr (#message_ident::ID, server::#handler::<#message_ident, _> ) ))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(quote!(
        impl server::ServerMessages for #ident {
            const NAME: &str = #name;

            fn messages() -> &'static [server::MessageDef<Self>] {
                use server::MessageId as _;
                &[
                    #(#messages),*
                ]
            }
        }
    ))
}

#[proc_macro_derive(Permissions, attributes(server_name, all_permissions))]
pub fn derive_permission_entry(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    with_manifest(|manifest| {
        derive_permission(parse_macro_input!(item as syn::Item), manifest)
            .unwrap_or_else(|e| e.to_compile_error())
            .into()
    })
}

fn derive_permission(item: syn::Item, manifest: &Manifest) -> Result<proc_macro2::TokenStream, syn::Error> {
    let syn::Item::Struct(item) = item else {
        return Err(syn::Error::new_spanned(item, "Item must be a struct"));
    };

    if !item.fields.is_empty() {
        return Err(syn::Error::new_spanned(item, "Item must be an empty struct"));
    }

    let ident = &item.ident;
    let server_name = get_attr_value(&item.attrs, "server_name").ok_or_else(|| {
        syn::Error::new_spanned(&item, concat!("Missing required `#[server_name = \"server-name\"]` attr."))
    })?;
    let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(server_name), .. }) = server_name else {
        return Err(syn::Error::new_spanned(item, "Name must be string literal"));
    };
    let server_name = server_name.value();

    let all_permissions = item
        .attrs
        .iter()
        .find(|attr| {
            let syn::Meta::Path(attr) = &attr.meta else { return false };
            let Some(ident) = attr.get_ident() else { return false };
            ident == "all_permissions"
        })
        .is_some();

    let server_permissions: Vec<_> = if all_permissions {
        let server = manifest
            .servers
            .get(&server_name)
            .ok_or_else(|| syn::Error::new_spanned(&item, concat!("Server not found in manifest")))?;
        server.keys().collect()
    } else {
        manifest
            .permissions
            .get(&server_name)
            .ok_or_else(|| syn::Error::new_spanned(&item, concat!("Server not found in 'permissions'")))?
            .iter()
            .collect()
    };
    let messages: Vec<_> = server_permissions
        .iter()
        .map(|message_type| {
            let message_ident = syn::Ident::new(message_type, item.span());
            quote!( impl server::MessageAllowed<#message_ident> for #ident {} )
        })
        .collect();

    Ok(quote!(
        impl server::CheckedPermissions for #ident {
            const NAME: &str = #server_name;
        }
        #(#messages)*
    ))
}
#[proc_macro_derive(Message, attributes(response, event, error))]
pub fn derive_message_entry(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    with_api_manifest(|manifest| {
        derive_message(parse_macro_input!(item as syn::Item), manifest)
            .unwrap_or_else(|e| e.to_compile_error())
            .into()
    })
}

fn derive_message(item: syn::Item, manifest: &ApiManifest) -> Result<proc_macro2::TokenStream, syn::Error> {
    let (ident, attrs) = match &item {
        syn::Item::Enum(item_enum) => (&item_enum.ident, &item_enum.attrs),
        syn::Item::Struct(item_struct) => (&item_struct.ident, &item_struct.attrs),
        _ => return Err(syn::Error::new_spanned(item, "Item must be a struct or enum")),
    };
    let response = get_attr_ident(attrs, "response");
    let event = get_attr_ident(attrs, "event");
    let error = get_attr_ident(attrs, "error");

    let ident_str = ident.to_string();

    if manifest.servers.values().filter(|msgs| msgs.contains_key(&ident_str)).count() > 1 {
        return Err(syn::Error::new_spanned(item, "Message found in multiple servers in manifest.toml"));
    }

    let (server, msg) = manifest
        .servers
        .iter()
        .find_map(|(server, msgs)| msgs.get(&ident_str).map(|msg| (server.clone(), msg)))
        .ok_or_else(|| syn::Error::new_spanned(&item, "Message not found in manifest.toml"))?;

    macro_rules! unused_attr {
        ($attr:ident) => {
            if $attr.is_some() {
                return Err(syn::Error::new_spanned(
                    &item,
                    concat!("This message cannot have a `", stringify!($attr), " ` attr."),
                ));
            }
        };
    }
    macro_rules! used_attr {
        ($attr:ident) => {
            $attr.ok_or_else(|| {
                syn::Error::new_spanned(&item, concat!("Missing required `", stringify!($attr), " ` attr."))
            })?
        };
    }
    let id = msg.id;
    let impl_trait = match msg.r#type {
        MessageType::Archive => {
            unused_attr!(response);
            unused_attr!(event);
            unused_attr!(error);

            quote! {
                impl server::Archive for #ident {};
            }
        }
        MessageType::BlockingArchive => {
            let response = used_attr!(response);
            unused_attr!(event);
            unused_attr!(error);

            quote! {
                impl server::BlockingArchive for #ident {
                    type Response = #response;
                };
            }
        }
        MessageType::ArchiveEvent => {
            unused_attr!(response);
            let event = used_attr!(event);
            let default_error = quote!(server::Infallible);
            let error = error.unwrap_or(&default_error);

            quote! {
                impl server::ArchiveSubscription for #ident {
                    type Event = #event;
                    type Error = #error;
                };
            }
        }
        MessageType::Scalar => {
            unused_attr!(response);
            unused_attr!(event);
            unused_attr!(error);
            let impl_scalar = impl_scalar(&item);

            quote! {
                impl server::Scalar for #ident {}
                #impl_scalar
            }
        }
        MessageType::BlockingScalar => {
            let response = used_attr!(response);
            unused_attr!(event);
            unused_attr!(error);
            let impl_scalar = impl_scalar(&item);

            quote! (
                #impl_scalar
                impl server::BlockingScalar for #ident {
                    type Response = #response;
                }
            )
        }
        MessageType::ScalarEvent => {
            unused_attr!(response);
            let event = used_attr!(event);
            let default_error = quote!(server::Infallible);
            let error = error.unwrap_or(&default_error);

            quote! {
                impl server::ScalarSubscription for #ident {
                    type Event = #event;
                    type Error = #error;
                };
            }
        }
        MessageType::LendMut | MessageType::DeferredLendMut => {
            let response = used_attr!(response);
            unused_attr!(event);
            unused_attr!(error);

            quote! {
                impl server::LendMut for #ident {
                    type Response = #response;
                };
            }
        }
        MessageType::Move => {
            unused_attr!(response);
            unused_attr!(event);
            unused_attr!(error);

            quote! {
                impl server::Move for #ident {};
            }
        }
    };

    Ok(quote!(
        impl server::MessageId for #ident {
            const ID: server::xous::MessageId = #id;
            const SERVER: &'static str = #server;
        }
        #impl_trait
    ))
}

fn impl_scalar(item: &syn::Item) -> Option<proc_macro2::TokenStream> {
    let syn::Item::Struct(item_struct) = item else {
        return None;
    };
    let ident = &item_struct.ident;
    match &item_struct.fields {
        syn::Fields::Named(_) => None,
        syn::Fields::Unnamed(fields_unnamed) => {
            if fields_unnamed.unnamed.len() == 1 {
                Some(quote!(
                    impl server::FromScalar<4> for #ident {
                        fn from_scalar(value: [u32; 4]) -> Self { Self(server::FromScalar::from_scalar(value)) }
                    }

                    impl server::AsScalar<4> for #ident {
                        fn as_scalar(&self) -> [u32; 4] { self.0.as_scalar() }
                    }
                ))
            } else {
                None
            }
        }
        syn::Fields::Unit => Some(quote! (
            impl server::FromScalar<1> for #ident {
                fn from_scalar(_: [u32; 1]) -> Self { Self {} }
            }

            impl server::AsScalar<1> for #ident {
                fn as_scalar(&self) -> [u32; 1] { [0] }
            }
        )),
    }
}

fn get_attr_ident<'a>(attrs: &'a [Attribute], name: &'static str) -> Option<&'a proc_macro2::TokenStream> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::List(attr) = &attr.meta else { return None };
        if attr.path.get_ident()? != name {
            return None;
        };
        Some(&attr.tokens)
    })
}

fn get_attr_value<'a>(attrs: &'a [Attribute], name: &'static str) -> Option<&'a syn::Expr> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(attr) = &attr.meta else { return None };
        if attr.path.get_ident()? != name {
            return None;
        };
        Some(&attr.value)
    })
}

fn load_api_manifest(dir: String) -> ApiManifest {
    ApiManifest::load_with_tracking(&PathBuf::from(&dir), |path| {
        proc_macro::tracked::path(path);
    })
}

fn load_server_manifest(dir: String) -> Manifest {
    let mut templates_dir = std::fs::canonicalize(&dir).unwrap();
    loop {
        if templates_dir.join("permission_templates.toml").exists() {
            break;
        }
        templates_dir = templates_dir
            .parent()
            .expect("Could not find permission_templates.toml in parent directories")
            .into();
    }

    Manifest::load_with_tracking(&PathBuf::from(dir), &templates_dir, |path| {
        proc_macro::tracked::path(path);
    })
}

fn with_api_manifest<T>(f: impl FnOnce(&ApiManifest) -> T) -> T {
    static MANIFESTS: LazyLock<Mutex<HashMap<String, (ApiManifest, Instant)>>> =
        LazyLock::new(|| Default::default());

    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    if MANIFESTS
        .lock()
        .unwrap()
        .get(&dir)
        .map(|(_, updated)| updated.elapsed() > Duration::from_millis(5000))
        .unwrap_or(true)
    {
        let new_manifest = load_api_manifest(dir.clone());
        MANIFESTS.lock().unwrap().insert(dir.clone(), (new_manifest, Instant::now()));
    }

    f(&MANIFESTS.lock().unwrap().get(&dir).unwrap().0)
}

fn with_manifest<T>(f: impl FnOnce(&Manifest) -> T) -> T {
    static MANIFESTS: LazyLock<Mutex<HashMap<String, (Manifest, Instant)>>> =
        LazyLock::new(|| Default::default());

    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    if MANIFESTS
        .lock()
        .unwrap()
        .get(&dir)
        .map(|(_, updated)| updated.elapsed() > Duration::from_millis(5000))
        .unwrap_or(true)
    {
        let new_manifest = load_server_manifest(dir.clone());
        MANIFESTS.lock().unwrap().insert(dir.clone(), (new_manifest, Instant::now()));
    }

    f(&MANIFESTS.lock().unwrap().get(&dir).unwrap().0)
}
