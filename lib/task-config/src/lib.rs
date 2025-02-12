// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Field, Result, Token};

struct Config {
    items: Punctuated<Field, Token![,]>,
}
impl Parse for Config {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            items: input
                .parse_terminated(Field::parse_named, syn::Token![,])?,
        })
    }
}

/// Recursively turns a `syn::Type` and `toml::Value` into a `TokenStream`.
///
/// The type and value must be compatible, e.g. if the type is an array, then
/// the value should also be an array.  This supports arrays, tuples, slices,
/// references, and primitive values.
fn config_to_token(
    ty: &syn::Type,
    v: &toml::Value,
) -> proc_macro2::TokenStream {
    match ty {
        syn::Type::Tuple(a) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "Expected TOML array for tuple type {}; got {}",
                        ty.to_token_stream(),
                        v
                    )
                })
                .iter()
                .zip(a.elems.iter())
                .map(|(v, t)| config_to_token(t, v))
                .collect();
            quote! { ( #(#v),* ) }
        }
        syn::Type::Array(a) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "Expected TOML array for array type {}; got {}",
                        ty.to_token_stream(),
                        v
                    )
                })
                .iter()
                .map(|v| config_to_token(&a.elem, v))
                .collect();
            quote! { [ #(#v),* ] }
        }
        syn::Type::Slice(s) => {
            let v: Vec<proc_macro2::TokenStream> = v
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "Expected TOML array for slice type {}; got {}",
                        ty.to_token_stream(),
                        v
                    )
                })
                .iter()
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
                .unwrap_or_else(|_| panic!("Could not parse {}", v))
        }
        _ => {
            panic!("Got unhandled type {}", ty.to_token_stream())
        }
    }
}

/// The `task_config!` macro defines a `struct TASK_CONFIG` which is pulled
/// from the Hubris task config.
///
/// For example, the following definition could live in a task's `main.rs`:
/// ```ignore
/// task_config::task_config! {
///     count: usize,
///     leds: &'static [(drv_stm32xx_sys_api::PinSet, bool)],
/// }
/// ```
///
/// Then, it look for a `config` block in the `user_leds` task:
/// ```toml
/// [tasks.user_leds.config]
/// count = 4
/// leds = [
///     ["drv_stm32xx_sys_api::Port::C.pin(6)", true],
///     ["drv_stm32xx_sys_api::Port::I.pin(8)", false],
///     ["drv_stm32xx_sys_api::Port::I.pin(9)", false],
///     ["drv_stm32xx_sys_api::Port::I.pin(10)", false],
///     ["drv_stm32xx_sys_api::Port::I.pin(11)", false],
/// ]
/// ```
///
/// This would generate the following Rust code:
/// ```ignore
/// struct TaskConfig {
///     count: usize,
///     leds: &'static [(drv_stm32xx_sys_api::PinSet, bool)],
/// }
/// const TASK_CONFIG: TaskConfig {
///     count: 4,
///     leds: &[
///         (drv_stm32xx_sys_api::Port::C.pin(6), true),
///         (drv_stm32xx_sys_api::Port::I.pin(8), false),
///         (drv_stm32xx_sys_api::Port::I.pin(9), false),
///         (drv_stm32xx_sys_api::Port::I.pin(10), false),
///         (drv_stm32xx_sys_api::Port::I.pin(11), false),
///     ]
/// }
/// ```
///
/// At the moment, this only supports tasks which are instantiated _once_ and
/// configured through the task configuration block (e.g. the SPI driver
/// cannot be configured using this macro).
#[proc_macro]
pub fn task_config(tokens: TokenStream) -> TokenStream {
    let config = build_util::task_config::<toml::Value>().unwrap();

    let input = parse_macro_input!(tokens as Config);
    let fields = input.items.iter();

    let values = input
        .items
        .iter()
        .map(|f| {
            let ident = f.ident.as_ref().expect("Missing ident");
            let v = config.get(ident.to_string()).unwrap_or_else(|| {
                panic!("Missing config parameter in TOML file: {}", ident)
            });
            let vs = config_to_token(&f.ty, v);
            quote! { #ident: #vs }
        })
        .collect::<Vec<_>>();

    let app_toml_path = std::env::var("HUBRIS_APP_TOML")
        .expect("Could not find 'HUBRIS_APP_TOML' environment variable");

    // Once `proc_macro::tracked_env::var` is stable, we won't need to use
    // this hack, but until then, we include the app TOML file to force
    // rebuilds if it changes (and trust it's optimized out by the compiler)
    quote! {
        const APP_TOML_TO_ENSURE_REBUILD: &[u8] = include_bytes!(#app_toml_path);
        struct Config {
            #(#fields),*
        }
        const TASK_CONFIG: Config = Config {
            #(#values),*
        };
    }
    .into()
}

/// Equivalent of `task_config!` for cases where a task has optional
/// configuration that can be replaced by defaults if required. This will
/// generate a `TASK_CONFIG` that is of type `Option<TaskConfig>`.
#[proc_macro]
pub fn optional_task_config(tokens: TokenStream) -> TokenStream {
    let input = parse_macro_input!(tokens as Config);
    let fields = input.items.iter();
    let app_toml_path = std::env::var("HUBRIS_APP_TOML")
        .expect("Could not find 'HUBRIS_APP_TOML' environment variable");

    let cfg_val = if let Ok(config) = build_util::task_config::<toml::Value>() {
        let values = input
            .items
            .iter()
            .map(|f| {
                let ident = f.ident.as_ref().expect("Missing ident");
                let v = config.get(ident.to_string()).unwrap_or_else(|| {
                    panic!("Missing config parameter in TOML file: {}", ident)
                });
                let vs = config_to_token(&f.ty, v);
                quote! { #ident: #vs }
            })
            .collect::<Vec<_>>();

        // Once `proc_macro::tracked_env::var` is stable, we won't need to use
        // this hack, but until then, we include the app TOML file to force
        // rebuilds if it changes (and trust it's optimized out by the compiler)
        quote! {
            Some(Config {
                #(#values),*
            })
        }
    } else {
        quote! { None }
    };
    // Once `proc_macro::tracked_env::var` is stable, we won't need to use
    // this hack, but until then, we include the app TOML file to force
    // rebuilds if it changes (and trust it's optimized out by the compiler)
    quote! {
        const APP_TOML_TO_ENSURE_REBUILD: &[u8] = include_bytes!(#app_toml_path);
        struct Config {
            #(#fields),*
        }
        const TASK_CONFIG: Option<Config> = #cfg_val;
    }
    .into()
}
