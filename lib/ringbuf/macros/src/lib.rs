// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, DeriveInput,
};

/// Derives an implementation of the [`ringbuf::Event`] trait for the annotated
/// `enum` type.
///
/// Note that this macro can currently only be used on `enum` types.
#[proc_macro_derive(Event)]
pub fn derive_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_count_event_impl(input) {
        Ok(tokens) => tokens.to_token_stream().into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Generate the event counts static for a ringbuffer.
///
/// This must be a proc-macro in order to concatenate the ringbuf's name with
/// `_COUNTS`. This macro is invoked by the `counted_ringbuf!` macro in the
/// `ringbuf` crate; you are not generally expected to invoke this directly.
#[doc(hidden)]
#[proc_macro]
pub fn declare_counts(input: TokenStream) -> TokenStream {
    let DeclareCounts { ident, ty } =
        parse_macro_input!(input as DeclareCounts);
    let counts_ident = counts_ident(&ident);
    quote! {
        #[used]
        static #counts_ident: ringbuf::EventCounts<#ty, { <#ty as ringbuf::Event>::VARIANTS }>
             = ringbuf::EventCounts::new(<#ty as ringbuf::Event>::NAMES);
    }.into()
}

/// Increment the event count in a ringbuffer for a particular event.
///
/// This must be a proc-macro in order to concatenate the ringbuf's name with
/// `_COUNTS`. This macro is invoked by the `event!` macro in the `ringbuf`
/// crate; you are not generally expected to invoke this directly.
#[doc(hidden)]
#[proc_macro]
pub fn incr_count(input: TokenStream) -> TokenStream {
    let IncrCount { mut buf, expr } = parse_macro_input!(input as IncrCount);
    let counts_ident = counts_ident(
        &buf.segments.last().expect("path may not be empty").ident,
    );
    buf.segments
        .last_mut()
        .expect("path may not be empty")
        .ident = counts_ident;

    quote! {
        #buf.increment(#expr);
    }
    .into()
}

struct DeclareCounts {
    ident: Ident,
    ty: syn::Type,
}

struct IncrCount {
    buf: syn::Path,
    expr: syn::Expr,
}

impl Parse for DeclareCounts {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let ident = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let ty = input.parse()?;
        Ok(DeclareCounts { ident, ty })
    }
}

impl Parse for IncrCount {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let buf = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let expr = input.parse()?;
        Ok(IncrCount { buf, expr })
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
    let mut variant_patterns = Vec::with_capacity(len);
    let mut variant_names = Vec::with_capacity(len);
    for (i, variant) in variants.iter().enumerate() {
        let ident = &variant.ident;
        variant_patterns.push(match variant.fields {
            syn::Fields::Unit => quote! { #name::#ident => #i },
            syn::Fields::Named(_) => quote! { #name::#ident { .. } => #i },
            syn::Fields::Unnamed(_) => quote! { #name::#ident(..) => #i },
        });
        variant_names.push(quote! { stringify!(#ident) });
    }
    let imp = quote! {
        impl ringbuf::Event for #name {
            const VARIANTS: usize = #len;
            const NAMES: &'static [&'static str] = &[ #(#variant_names),* ];
            fn index(&self) -> usize {
                match self {
                    #(#variant_patterns),*
                }
            }

        }
    };
    Ok(imp)
}

fn counts_ident(ident: &Ident) -> Ident {
    Ident::new(&format!("{}_COUNTS", ident), Span::call_site())
}
