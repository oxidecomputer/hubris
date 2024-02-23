// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::{parse_macro_input, DeriveInput};

/// Derives an implementation of the `ringbuf::Count` trait for the annotated
/// `enum` type.
///
/// Note that this macro can currently only be used on `enum` types.
#[proc_macro_derive(Count)]
pub fn derive_count(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_count_event_impl(input) {
        Ok(tokens) => tokens.to_token_stream().into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn gen_count_event_impl(
    input: DeriveInput,
) -> Result<impl ToTokens, syn::Error> {
    let name = &input.ident;
    let data_enum = match input.data {
        syn::Data::Enum(ref data_enum) => data_enum,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "Event can only be derived for enums",
            ));
        }
    };
    let variants = &data_enum.variants;
    let len = variants.len();
    let mut variant_names = Vec::with_capacity(len);
    let mut variant_patterns = Vec::with_capacity(len);
    for variant in variants {
        let ident = &variant.ident;
        variant_patterns.push(match variant.fields {
            syn::Fields::Unit => quote! { #name::#ident => &counters.#ident },
            syn::Fields::Named(_) => {
                quote! { #name::#ident { .. } => &counters.#ident }
            }
            syn::Fields::Unnamed(_) => {
                quote! { #name::#ident(..) => &counters.#ident }
            }
        });
        variant_names.push(ident.clone());
    }
    let counts_ty = counts_ty(name);
    let code = quote! {
        #[doc = concat!(" Ringbuf event counts for [`", stringify!(#name), "`].")]
        #[allow(nonstandard_style)]
        pub struct #counts_ty {
            #(
                #[doc = concat!(
                    " The total number of times a [`",
                    stringify!(#name), "::", stringify!(#variant_names),
                    "`] event"
                )]
                #[doc = " has been recorded by this ringbuf."]
                pub #variant_names: core::sync::atomic::AtomicU32
            ),*
        }

        #[automatically_derived]
        impl ringbuf::Count for #name {
            type Counters = #counts_ty;

            // This is intended for use in a static initializer, so the fact that every
            // time the constant is used it will be a different instance is not a
            // problem --- in fact, it's the desired behavior.
            //
            // `declare_interior_mutable_const` is really Not My Favorite Clippy
            // Lint...
            #[allow(clippy::declare_interior_mutable_const)]
            const NEW_COUNTERS: #counts_ty = #counts_ty {
                #(#variant_names: core::sync::atomic::AtomicU32::new(0)),*
            };

            fn count(&self, counters: &Self::Counters) {
                #[cfg(all(target_arch = "arm", armv6m))]
                use ringbuf::rmv6m_atomic_hack::AtomicU32Ext;

                let counter = match self {
                    #(#variant_patterns),*
                };
                counter.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
    };
    Ok(code)
}

fn counts_ty(ident: &Ident) -> Ident {
    Ident::new(&format!("{ident}Counts"), Span::call_site())
}
