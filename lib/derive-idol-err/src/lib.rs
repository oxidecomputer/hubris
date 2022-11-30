// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Adds three `impl` blocks for the given error `enum` type:
/// - `From<E> for u16` (Idol encoding)
/// - `From<E> for u32` (Hiffy encoding)
/// - `TryFrom<u32> for E` (Idol decoding)
///
/// The given type must also derive `FromPrimitive`, which is used in the
/// `TryFrom<u32>` implementation.  Sadly, this cannot be automatically added
/// to the type by this macro.
///
/// The `enum` must not include 0, because 0 is decoded as "okay" by IPC
/// infrastructure.
#[proc_macro_derive(IdolError)]
pub fn derive(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, data, .. } = parse_macro_input!(input);

    let data = match data {
        syn::Data::Enum(data) => data,
        syn::Data::Struct(_) | syn::Data::Union(_) => {
            return compile_error(
                ident.span(),
                "IdolError can only be derived on enums",
            )
            .into();
        }
    };

    let mut variant_errors = vec![];
    let mut discriminant = None;
    for v in &data.variants {
        if v.fields != syn::Fields::Unit {
            variant_errors.push(compile_error(
                v.ident.span(),
                "idol errors must be C-style enums",
            ));
        }
        if let Some((_, d)) = &v.discriminant {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(lit),
                ..
            }) = d
            {
                let value: i64 = match lit.base10_parse() {
                    Ok(value) => value,
                    Err(e) => {
                        variant_errors.push(e.into_compile_error());
                        continue;
                    }
                };
                check_discriminant(&mut variant_errors, lit.span(), value);
                discriminant = Some(value);
            } else {
                variant_errors.push(compile_error(
                    v.ident.span(),
                    "idol errors must use simple positive integer \
                     discriminants",
                ));
                discriminant = None;
            }
        } else if let Some(d) = &mut discriminant {
            *d = d.checked_add(1).expect("discriminant overflow");
            check_discriminant(&mut variant_errors, ident.span(), *d);
        } else {
            // No explicit discriminant specified and none recorded from a
            // previous iteration -- this would implicitly become zero.
            discriminant = Some(0);
            // Bit of a hack to reuse the error reporting code:
            check_discriminant(&mut variant_errors, ident.span(), 0);
        }
    }

    let output = quote! {
        #( #variant_errors )*

        impl From<#ident> for u16 {
            fn from(v: #ident) -> Self {
                v as u16
            }
        }
        impl From<#ident> for u32 {
            fn from(v: #ident) -> Self {
                v as u32
            }
        }
        impl core::convert::TryFrom<u32> for #ident {
            type Error = ();
            fn try_from(v: u32) -> Result<Self, Self::Error> {
                Self::from_u32(v).ok_or(())
            }
        }
    };
    output.into()
}

fn check_discriminant(
    variant_errors: &mut Vec<proc_macro2::TokenStream>,
    span: proc_macro2::Span,
    d: i64,
) {
    if d == 0 {
        variant_errors
            .push(compile_error(span, "error enums must not contain zero"));
    }
    if !(0..=0xFFFF).contains(&d) {
        variant_errors
            .push(compile_error(span, "error enum values must fit in a u16"));
    }
}

fn compile_error(
    span: proc_macro2::Span,
    msg: &str,
) -> proc_macro2::TokenStream {
    syn::Error::new(span, msg).into_compile_error()
}
