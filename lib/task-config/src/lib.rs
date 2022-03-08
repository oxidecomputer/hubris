// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use serde::{de::DeserializeOwned, Deserialize};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Field, Ident, Result, Token};

struct Config {
    name: Ident,
    _comma: Token![,],
    items: Punctuated<Field, Token![,]>,
}
impl Parse for Config {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(Self {
            name: input.parse()?,
            _comma: input.parse()?,
            items: input.parse_terminated(Field::parse_named)?,
        })
    }
}

fn toml_from_env<T: DeserializeOwned>(var: &str) -> Option<T> {
    std::env::var(var)
        .ok()
        .and_then(|config| toml::from_slice(config.as_bytes()).ok())
}

/// We assume that there's a local task config which indexes into the global
/// task config.
#[derive(Deserialize)]
struct TaskConfig {
    global_config: String,
}

fn config_to_token(
    ty: &syn::Type,
    v: &toml::Value,
) -> proc_macro2::TokenStream {
    match ty {
        syn::Type::Tuple(a) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .expect(&format!(
                    "Expected TOML array for tuple type {}; got {}",
                    ty.to_token_stream().to_string(),
                    v
                ))
                .into_iter()
                .zip(a.elems.iter())
                .map(|(v, t)| config_to_token(t, v))
                .collect();
            quote! { ( #(#v),* ) }
        }
        syn::Type::Array(a) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .expect(&format!(
                    "Expected TOML array for array type {}; got {}",
                    ty.to_token_stream().to_string(),
                    v
                ))
                .into_iter()
                .map(|v| config_to_token(&a.elem, v))
                .collect();
            quote! { [ #(#v),* ] }
        }
        syn::Type::Slice(s) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .expect(&format!(
                    "Expected TOML array for slice type {}; got {}",
                    ty.to_token_stream().to_string(),
                    v
                ))
                .into_iter()
                .map(|v| config_to_token(&s.elem, v))
                .collect();
            quote! { [ #(#v),* ] }
        }
        syn::Type::Reference(r) => {
            let mut out: proc_macro2::TokenStream = "&".parse().unwrap();
            out.extend(config_to_token(&r.elem, v));
            out
        }
        syn::Type::Path(_) => {
            // We assume that strings should be inserted verbatim into the
            // code; if you want an explicit string, then do something like
            // '"hello, world"' in the app.toml file
            let v = if v.is_str() {
                v.as_str().unwrap().to_string()
            } else {
                v.to_string()
            };
            v.parse()
                .expect(&format!("Could not parse {}", v.to_string()))
        }
        _ => {
            panic!(
                "Got unhandled type {}\n{:?}",
                ty.to_token_stream().to_string(),
                ty
            );
        }
    }
}

#[proc_macro]
pub fn task_config(tokens: TokenStream) -> TokenStream {
    // TODO: include_bytes! on app.toml to trigger a recompilation when it
    // changes.  (Alternatively, `proc_macro::tracked_env::var` works on
    // nightly to explicitly track environmental variables)
    //
    // Right now, it doesn't matter because we do a clean rebuild whenever
    // app.toml ever changes, but that will change if issue
    // https://github.com/oxidecomputer/hubris/issues/240 is closed

    let task_config = toml_from_env::<TaskConfig>("HUBRIS_TASK_CONFIG");
    let global_config = toml_from_env::<toml::Value>("HUBRIS_APP_CONFIG")
        .expect("Could not find HUBRIS_TASK_CONFIG");
    let config = if let Some(t) = task_config {
        &global_config.get(&t.global_config).expect(&format!(
            "Could not find local config with key {}",
            t.global_config
        ))
    } else {
        &global_config
    };

    let input = parse_macro_input!(tokens as Config);
    let config = config.get(input.name.to_string()).expect(&format!(
        "Could not find config.{} in app TOML file",
        input.name.to_string()
    ));

    let values = input
        .items
        .iter()
        .map(|f| {
            let ident = f.ident.as_ref().expect("Missing ident");
            let v = config.get(ident.to_string()).expect(&format!(
                "Missing parameter in app TOML file: {}",
                ident.to_string()
            ));
            let mut out = quote! { #ident: };
            out.extend(config_to_token(&f.ty, v).into_iter());
            out
        })
        .collect::<Vec<_>>();

    let fields = input.items.iter();
    let out = quote! {
        struct Config {
            #(#fields),*
        }
        const TASK_CONFIG: Config = Config {
            #(#values),*
        };
    }
    .into();

    out
}
