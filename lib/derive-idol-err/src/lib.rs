// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Adds three `impl` blocks for the given error type:
/// - `From<E> for u16` (Idol encoding)
/// - `From<E> for u32` (Hiffy encoding)
/// - `TryFrom<u32> for E` (Idol decoding)
///
/// The given type must also derive `FromPrimitive`, which is used in the
/// `TryFrom<u32>` implementation.  Sadly, this cannot be automatically added
/// to the type by this macro.
#[proc_macro_derive(IdolError, attributes(idol_death))]
pub fn derive(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, data, .. } = parse_macro_input!(input);
    let mut on_death = None;
    match data {
        syn::Data::Enum(e) => {
            // Search for a variant marked idol_death
            for v in &e.variants {
                for a in &v.attrs {
                    if let Some(attr_ident) = a.path.get_ident() {
                        if attr_ident == "idol_death" {
                            if on_death.is_some() {
                                panic!("duplicate idol_death attribute");
                            }
                            on_death = Some(v.ident.clone());
                        }
                    }
                }
            }
        }
        _ => panic!("unsupported struct or union (only enums supported)"),
    }
    let try_from = if let Some(death_ident) = on_death {
        quote! {
            impl core::convert::TryFrom<u32> for #ident {
                type Error = ();
                fn try_from(v: u32) -> Result<Self, Self::Error> {
                    if ::userlib::extract_new_generation(v).is_some() {
                        Ok(Self::#death_ident)
                    } else {
                        Self::from_u32(v).ok_or(())
                    }
                }
            }
        }
    } else {
        quote! {
            impl core::convert::TryFrom<u32> for #ident {
                type Error = ();
                fn try_from(v: u32) -> Result<Self, Self::Error> {
                    Self::from_u32(v).ok_or(())
                }
            }
        }
    };

    let output = quote! {
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
        #try_from
    };
    output.into()
}
