use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(IdolError)]
pub fn derive(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, .. } = parse_macro_input!(input);
    // We need to implement From for both u16 *and* u32, because Idol uses
    // one and Hiffy uses the other.
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
