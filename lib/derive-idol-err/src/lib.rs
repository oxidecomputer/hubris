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
#[proc_macro_derive(IdolError)]
pub fn derive(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, .. } = parse_macro_input!(input);
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
        impl core::convert::TryFrom<u32> for #ident {
            type Error = ();
            fn try_from(v: u32) -> Result<Self, Self::Error> {
                Self::from_u32(v).ok_or(())
            }
        }
    };
    output.into()
}
