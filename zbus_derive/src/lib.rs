//! This crate provides derive macros helpers for zbus.
//!
//! At the moment, it supports basic [`Proxy`] implementation only.
//!
//! # Examples
//!
//! ```
//!# use std::error::Error;
//! use zbus_derive::dbus_proxy;
//! use zbus::{Connection, Result};
//! use zvariant::Value;
//!
//! #[dbus_proxy(
//!     interface = "org.test.SomeIface",
//!     default_service = "org.test.SomeService",
//!     default_path = "/org/test/SomeObject"
//! )]
//! trait SomeIface {
//!     fn do_this(&self, with: &str, some: u32, arg: &Value) -> Result<bool>;
//!     #[dbus_proxy(property)]
//!     fn a_property(&self) -> Result<String>;
//!     #[dbus_proxy(property)]
//!     fn set_a_property(&self, a_property: &str) -> Result<()>;
//! };
//!
//! let c = Connection::new_session()?;
//! let i = SomeIfaceProxy::new(&c)?;
//! let _ = i.do_this("foo", 32, &Value::new(true));
//! let _ = i.set_a_property("val");
//!
//!# Ok::<_, Box<dyn Error + Send + Sync>>(())
//! ```
//! [`Proxy`]: https://docs.rs/zbus/2.0.0/zbus/struct.Proxy.html
//!
extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro_crate::crate_name;
use quote::quote;
use syn::{
    self, AttributeArgs, FnArg, Ident, ItemTrait, NestedMeta, Pat, PatIdent, PatType,
    TraitItemMethod,
};

mod utils;
use utils::*;

#[proc_macro_attribute]
pub fn dbus_proxy(attr: TokenStream, mut item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as AttributeArgs);
    let gen_impl = true;
    let mut iface_name = None;
    let mut default_path = None;
    let mut default_service = None;
    let mut has_introspect_method = false;

    let zbus = Ident::new(
        &match crate_name("zbus") {
            Ok(x) => x,
            Err(_) => "zbus".into(),
        },
        Span::call_site(),
    );

    for arg in args {
        match arg {
            NestedMeta::Meta(syn::Meta::NameValue(nv)) => {
                if nv.path.is_ident("interface") {
                    if let syn::Lit::Str(lit) = nv.lit {
                        iface_name = Some(lit.value());
                    } else {
                        panic!("Invalid interface argument")
                    }
                } else if nv.path.is_ident("default_path") {
                    if let syn::Lit::Str(lit) = nv.lit {
                        default_path = Some(lit.value());
                    } else {
                        panic!("Invalid path argument")
                    }
                } else if nv.path.is_ident("default_service") {
                    if let syn::Lit::Str(lit) = nv.lit {
                        default_service = Some(lit.value());
                    } else {
                        panic!("Invalid service argument")
                    }
                } else {
                    panic!("Unsupported argument");
                }
            }
            _ => panic!("Unknown attribute"),
        }
    }

    if gen_impl {
        let input = syn::parse_macro_input!(item as ItemTrait);
        let doc = get_doc_attrs(&input.attrs);
        let proxy_name = Ident::new(&format!("{}Proxy", input.ident), Span::call_site());
        let ident = input.ident.to_string();
        let name = iface_name.unwrap_or(format!("org.freedesktop.{}", ident));
        let default_path = default_path.unwrap_or(format!("/org/freedesktop/{}", ident));
        let default_service = default_service.unwrap_or_else(|| name.clone());
        let mut methods = proc_macro2::TokenStream::new();

        for i in input.items.iter() {
            if let syn::TraitItem::Method(m) = i {
                let method_name = m.sig.ident.to_string();
                if method_name == "introspect" {
                    has_introspect_method = true;
                }

                let attrs = parse_item_attributes(&m.attrs).unwrap();
                let is_property = attrs.iter().any(|x| x.is_property());
                let has_inputs = m.sig.inputs.len() > 1;
                let name = attrs
                    .iter()
                    .find_map(|x| match x {
                        ItemAttribute::Name(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        pascal_case(if is_property && has_inputs {
                            assert!(method_name.starts_with("set_"));
                            &method_name[4..]
                        } else {
                            &method_name
                        })
                    });
                let m = if is_property {
                    gen_proxy_property(&name, &m)
                } else {
                    gen_proxy_method_call(&name, &m)
                };
                methods.extend(m);
            }
        }

        if !has_introspect_method {
            methods.extend(quote! {
                pub fn introspect(&self) -> #zbus::Result<String> {
                    self.0.introspect()
                }
            });
        };

        let proxy_impl = quote! {
            #(#doc)*
            pub struct #proxy_name<'c>(#zbus::Proxy<'c>);

            impl<'c> #proxy_name<'c> {
                /// Creates a new proxy with the default service & path.
                pub fn new(conn: &'c #zbus::Connection) -> #zbus::Result<Self> {
                    Ok(Self(#zbus::Proxy::new(
                        conn,
                        #default_service,
                        #default_path,
                        #name,
                    )?))
                }

                /// Creates a new proxy for the given `destination` and `path`.
                pub fn new_for(conn: &'c #zbus::Connection, destination: &'c str, path: &'c str) -> #zbus::Result<Self> {
                    Ok(Self(#zbus::Proxy::new(
                        conn,
                        destination,
                        path,
                        #name,
                    )?))
                }

                #methods
            }
        };

        item = proc_macro::TokenStream::from(proxy_impl)
    }
    item
}

fn gen_proxy_method_call(method_name: &str, m: &TraitItemMethod) -> proc_macro2::TokenStream {
    let doc = get_doc_attrs(&m.attrs);
    let args = m
        .sig
        .inputs
        .iter()
        .filter_map(|arg| arg_ident(arg))
        .collect::<Vec<_>>();
    let sig = &m.sig;
    quote! {
        #(#doc)*
        pub #sig {
            let reply = self.0.call(#method_name, &(#(#args),*))?;
            Ok(reply)
        }
    }
}

fn gen_proxy_property(property_name: &str, m: &TraitItemMethod) -> proc_macro2::TokenStream {
    let doc = get_doc_attrs(&m.attrs);
    let sig = &m.sig;
    if sig.inputs.len() > 1 {
        let value = arg_ident(sig.inputs.last().unwrap()).unwrap();
        quote! {
            #(#doc)*
            pub #sig {
                self.0.try_set(#property_name, #value)
            }
        }
    } else {
        quote! {
            #(#doc)*
            pub #sig {
                self.0.try_get(#property_name)
            }
        }
    }
}

fn arg_ident(arg: &FnArg) -> Option<&Ident> {
    match arg {
        FnArg::Typed(PatType { pat, .. }) => {
            if let Pat::Ident(PatIdent { ident, .. }) = &**pat {
                return Some(ident);
            }
            None
        }
        _ => None,
    }
}