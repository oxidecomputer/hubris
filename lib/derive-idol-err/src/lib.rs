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
            panic!("IdolError can only be derived on enums")
        }
    };

    // Assert that each variant is nonzero when cast to a `u32`; zero is
    // reserved for success!
    let variant_nonzero_assertions = data.variants.into_iter().map(|variant| {
        let v = variant.ident;

        // Inline version of
        // ```
        // static_assertions::const_assert_ne!(#ident::#v, 0)
        // ```
        quote! {
            const _: [(); 0 - !{
                const ASSERT: bool = #ident::#v as u32 != 0;
                ASSERT
            } as usize] = [];
        }
    });

    let output = quote! {
        #( #variant_nonzero_assertions )*

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
