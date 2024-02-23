// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::{parse_macro_input, DeriveInput, parse::{Parse, ParseStream}};
 
/// Derives an implementation of the `ringbuf::Count` trait for the annotated
/// `enum` type.
///
/// Note that this macro can currently only be used on `enum` types.
///
/// # Variant Attributes
///
/// The following attributes may be added on one or more of the variants of the
/// `enum` type deriving `Count`:
///
/// - `#[count(skip)]`: Skip counting this variant. Enums used as ringbuf
///   entries often have a `None` or `Empty` variant which is used to initialize
///   the ring buffer but not recorded as an entry at runtime. The
///   `#[count(skip)]` attribute avoids generating a counter for such variants,
///   reducing the memory used by the counter struct a little bit.
///
/// - `#[count(children)]`: Count variants of a nested enum. Typically, when a
///   variant of a type deriving `Count` has fields, all instances of that
///   variant increment the same counter, regardless of the value of the fields.
///   When a variant has a single field of a type which also implements the
///   `Count` trait, however, the `#[count(children)]` attribute can be used to
///   generate an instance of the field type's counter struct, and implement
///   those counters instead.
#[proc_macro_derive(Count, attributes(count))]
pub fn derive_count(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_count_impl(input) {
        Ok(tokens) => tokens.to_token_stream().into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn gen_count_impl(input: DeriveInput) -> Result<impl ToTokens, syn::Error> {
    let name = &input.ident;
    let vis = &input.vis;
    let data_enum = match input.data {
        syn::Data::Enum(ref data_enum) => data_enum,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "`ringbuf::Count` can only be derived for enums",
            ));
        }
    };
    let variants = &data_enum.variants;
    let len = variants.len();
    let mut field_defs = Vec::with_capacity(len);
    let mut field_inits = Vec::with_capacity(len);
    let mut variant_patterns = Vec::with_capacity(len);
    let mut any_skipped = false;
    'variants: for variant in variants {
        let ident = &variant.ident;
        let mut count_children = None;
        for attr in &variant.attrs {
            if !attr.path().is_ident("count") {
                continue;
            }
            match attr.parse_args_with(VariantAttr::parse)? {
                VariantAttr::Skip => {
                    any_skipped = true;
                    continue 'variants;
                },
                VariantAttr::Children => {
                    count_children = Some(attr);
                },
            }
        }

        if let Some(count_children) = count_children {
            match variant.fields {
                syn::Fields::Unit => return Err(syn::Error::new_spanned(
                    count_children,
                    "the `count_children` attribute may not be used on unit variants",
                )),
                syn::Fields::Named(_) => return Err(syn::Error::new_spanned(
                    count_children,
                    "the `count_children` attribute does not currently support variants with named fields",
                )),

                syn::Fields::Unnamed(_) if variant.fields.len() > 1 => return Err(syn::Error::new_spanned(
                    count_children,
                    "the `count_children` attribute does not currently support variants with multiple fields",
                )),
                syn::Fields::Unnamed(ref u) => {
                    let field = u.unnamed.first().unwrap();
                    let ty = &field.ty;
                    variant_patterns.push(quote! { #name::#ident(c) => c.count(&counters.#ident) });
                    field_defs.push(quote! {
                        #[doc = concat!(
                            " The total number of times a [`",
                            stringify!(#name), "::", stringify!(#ident),
                            "`] entry"
                        )]
                        #[doc = " has been recorded by this ringbuf."]
                        pub #ident: <#ty as ringbuf::Count>::Counters
                    });
                    field_inits.push(quote! {
                        #ident: <#ty as ringbuf::Count>::NEW_COUNTERS
                    });
                }
            }
        } else {
            let incr = quote! {
                counters.#ident.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            };
            variant_patterns.push(match variant.fields {
                syn::Fields::Unit => quote! { #name::#ident => { #incr } },
                syn::Fields::Named(_) => {
                    quote! { #name::#ident { .. } => { #incr } }
                }
                syn::Fields::Unnamed(_) => {
                    quote! { #name::#ident(..) => { #incr } }
                }
            });
            field_defs.push(quote! {
                #[doc = concat!(
                    " The total number of times a [`",
                    stringify!(#name), "::", stringify!(#ident),
                    "`] entry"
                )]
                #[doc = " has been recorded by this ringbuf."]
                pub #ident: core::sync::atomic::AtomicU32
            });
            field_inits.push(quote! { #ident: core::sync::atomic::AtomicU32::new(0) });
        }
    }

    // If we skipped any variants, generate a catchall case.
    if any_skipped {
        variant_patterns.push(quote! { _ => {} });
    }

    let counts_ty = counts_ty(name);
    let code = quote! {
        #[doc = concat!(" Ringbuf entry total counts for [`", stringify!(#name), "`].")]
        #[allow(nonstandard_style)]
        #vis struct #counts_ty {
            #(#field_defs),*
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
                #(#field_inits),*
            };

            fn count(&self, counters: &Self::Counters) {
                #[cfg(all(target_arch = "arm", armv6m))]
                use ringbuf::rmv6m_atomic_hack::AtomicU32Ext;

                match self {
                    #(#variant_patterns),*
                };
            }
        }
    };
    Ok(code)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum VariantAttr {
    Skip,
    Children,
}

impl Parse for VariantAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident = input.fork().parse::<syn::Ident>()?;
        if ident == "skip" {
            // consume the token
            let _: syn::Ident = input.parse()?;
            Ok(VariantAttr::Skip)
        } else if ident == "children" {
            let _: syn::Ident = input.parse()?;
            Ok(VariantAttr::Children)
        } else {
            Err(syn::Error::new(
                ident.span(),
                "unrecognized count option, expected `skip` or `children`",
            ))
        }
    }
}


fn counts_ty(ident: &Ident) -> Ident {
    Ident::new(&format!("{ident}Counts"), Span::call_site())
}
