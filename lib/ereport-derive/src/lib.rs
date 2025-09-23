// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern crate proc_macro;
use proc_macro::TokenStream;
use quote::{ToTokens, quote};
use syn::spanned::Spanned;
use syn::{
    Attribute, DataEnum, DataStruct, DeriveInput, Generics, Ident, LitStr,
    Visibility, parse_macro_input,
};

/// Derives an implementation of the `EreportData` trait for the annotated
/// `struct` or `enum` type.
#[proc_macro_derive(EreportData, attributes(ereport))]
pub fn derive_ereport_data(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_ereport_data_impl(input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error().into(),
    }
}

fn gen_ereport_data_impl(
    input: DeriveInput,
) -> Result<TokenStream, syn::Error> {
    match &input.data {
        syn::Data::Enum(data) => gen_enum_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        syn::Data::Struct(data) => gen_struct_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        _ => Err(syn::Error::new_spanned(
            input,
            "`EreportData` can only be derived for `struct` and `enum` types",
        )),
    }
}

fn gen_enum_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataEnum,
) -> Result<impl ToTokens, syn::Error> {
    let mut variant_patterns = Vec::new();
    let mut variant_lens = Vec::new();
    // TODO(eliza): support top-level attribute for using the enum's repr instead of its name
    for variant in data.variants {
        let mut name = None;
        for attr in &variant.attrs {
            if attr.path().is_ident("ereport") {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        name = Some(meta.value()?.parse::<LitStr>()?);
                        Ok(())
                    } else {
                        Err(meta.error("expected `rename` attribute"))
                    }
                })?;
            };
        }
        let name = name.unwrap_or_else(|| {
            LitStr::new(&variant.ident.to_string(), variant.ident.span())
        });
        if !matches!(variant.fields, syn::Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "`#[derive(EreportData)]` only supports unit variants for now",
            ));
        }
        let variant_name = &variant.ident;
        variant_patterns.push(quote! {
            #ident::#variant_name => { e.str(#name)?; }
        });
        variant_lens.push(quote! {
            if ::ereport::str_cbor_len(#name) > max {
                max = ::ereport::str_cbor_len(#name);
            }
        });
    }

    Ok(quote! {
        #[automatically_derived]
        impl #generics ::ereport::EreportData for #ident #generics {
            const MAX_CBOR_LEN: usize = {
                let mut max = 0;
                #(#variant_lens;)*
                max
            };
        }

        impl<C, #generics> ::ereport::encode::Encode<C> for #ident #generics {
            fn encode<W: ::ereport::encode::Write>(
                &self,
                e: &mut ::ereport::encode::Encoder<W>,
                _: &mut C,
            ) -> Result<(), ::ereport::encode::Error<W::Error>> {
                match self {
                    #(#variant_patterns,)*
                }
                Ok(())
            }
        }
    })
}

fn gen_struct_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataStruct,
) -> Result<impl ToTokens, syn::Error> {
    let mut field_len_exprs = Vec::new();
    let mut field_encode_exprs = Vec::new();
    let mut where_bounds = Vec::new();
    // let mut data_where_bounds = Vec::new();
    for field in &data.fields {
        let mut field_name = None;
        let mut skipped = false;
        let mut flattened = false;
        let mut skipped_if_nil = false;
        for attr in &field.attrs {
            if attr.path().is_ident("ereport") {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        field_name = Some(meta.value()?.parse::<LitStr>()?);
                        Ok(())
                    } else if meta.path.is_ident("skip") {
                        skipped = true;
                        Ok(())
                    } else if meta.path.is_ident("skip_if_nil") {
                        skipped_if_nil = true;
                        Ok(())
                    } else if meta.path.is_ident("flatten") {
                        flattened = true;
                        Ok(())
                    } else {
                        Err(meta.error(
                            "expected `rename`, `skip`, `skip_if_nil`, or `flatten` attribute",
                        ))
                    }
                })?;
            }
        }
        if skipped {
            continue;
        }

        let field_ident = &field.ident.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(
                field,
                "#[derive(EreportData)] doesn't support tuple structs yet",
            )
        })?;
        let field_name = field_name.unwrap_or_else(|| {
            LitStr::new(&field_ident.to_string(), field_ident.span())
        });

        // TODO(eliza): if we allow more complex ways of encoding fields as different CBOR types, this will have to handle that...
        let field_type = &field.ty;
        if flattened {
            where_bounds.push(quote! {
                #field_type: ::ereport::EncodeFields<()>
            });
            field_len_exprs.push(quote! {
                len += <#field_type as ::ereport::EncodeFields<()>>::MAX_FIELDS_LEN;
            });
            field_encode_exprs.push(quote! {
                ::ereport::EncodeFields::<()>::encode_fields(&self.#field_ident, e, c)?;
            });
        } else {
            field_len_exprs.push(quote! {
                len += ::ereport::str_cbor_len(#field_name);
                len += <#field_type as ::ereport::EreportData>::MAX_CBOR_LEN;
            });
            field_encode_exprs.push(if skipped_if_nil {
                quote! {
                    if !::ereport::Encode::<()>::is_nil(&self.#field_ident) {
                        e.str(#field_name)?;
                        ::ereport::Encode::<()>::encode(&self.#field_ident, e, c)?;
                    }
                }
            } else {
                quote! {
                    e.str(#field_name)?;
                    ::ereport::Encode::<()>::encode(&self.#field_ident, e, c)?;
                }
            });
            where_bounds.push(quote! {
                #field_type: ::ereport::EreportData
            });
        }
    }
    let (impl_generics, tygenerics, prev_where_clause) =
        generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::ereport::EreportData for #ident #tygenerics
       #prev_where_clause
        where #(#where_bounds,)*
        {
            const MAX_CBOR_LEN: usize =
                2 // map begin and end bytes
                + <Self as ::ereport::EncodeFields<()>>::MAX_FIELDS_LEN;
        }

        #[automatically_derived]
        impl #impl_generics ::ereport::encode::Encode<()>
        for #ident #tygenerics
        #prev_where_clause
        where #(#where_bounds,)*
        {
            fn encode<W: ::ereport::encode::Write>(
                &self,
                e: &mut ::ereport::encode::Encoder<W>,
                c: &mut (),
            ) -> Result<(), ::ereport::encode::Error<W::Error>> {
                e.begin_map()?;
                <Self as ::ereport::EncodeFields<()>>::encode_fields(self, e, c)?;
                e.end()?;
                Ok(())
            }
        }

        #[automatically_derived]
        impl #impl_generics ::ereport::EncodeFields<()>
        for #ident #tygenerics
        #prev_where_clause
        where #(#where_bounds,)*
        {
            const MAX_FIELDS_LEN: usize = {
                let mut len = 0;
                #(#field_len_exprs;)*
                len
            };

            fn encode_fields<W: ::ereport::encode::Write>(
                &self,
                e: &mut ::ereport::encode::Encoder<W>,
                c: &mut (),
            ) -> Result<(), ::ereport::encode::Error<W::Error>> {
                #(#field_encode_exprs;)*
                Ok(())
            }
        }

    })
}
