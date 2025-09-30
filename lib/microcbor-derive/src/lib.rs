// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern crate proc_macro;
use proc_macro::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::{
    Attribute, DataEnum, DataStruct, DeriveInput, Generics, Ident, LitStr,
    Visibility, parse_macro_input,
};

/// Derives an implementation of the `Encode` and `StaticCborLen` traits for the
/// annotated `struct` or `enum` type.
#[proc_macro_derive(Encode, attributes(cbor))]
pub fn derive_encode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_encode_impl(input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error().into(),
    }
}

/// Derives an implementation of the `EncodeFields` trait for the annotated
/// `struct` or `enum` type.
///
/// Deriving `EncodeFields` allows the implementing type to be annotated with
/// `#[cbor(flatten)]` when nested within another type that derives `Encode` or
/// `EncodeFields`.
///
/// Types that derive `EncodeFields` may only have named fields. If the deriving
/// type is an `enum`, all variants must have named fields; attempting to derive
/// `EncodeFields` on an enum that has both named (struct-like) variants and
/// unnamed (tuple-like) variants will result in a compilation error.
///
/// The same type may derive both `Encode` and `EncodeFields` to be able to be
/// encoded both as its own map and flattened into existing maps.
#[proc_macro_derive(EncodeFields, attributes(cbor))]
pub fn derive_encode_fields(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_encode_fields_impl(input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error().into(),
    }
}

fn gen_encode_impl(input: DeriveInput) -> Result<TokenStream, syn::Error> {
    match &input.data {
        syn::Data::Enum(data) => gen_enum_encode_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        syn::Data::Struct(data) => gen_encode_struct_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        _ => Err(syn::Error::new_spanned(
            input,
            "`StaticCborLen` can only be derived for `struct` and `enum` \
             types",
        )),
    }
}

const HELPER_ATTR: &str = "cbor";

fn gen_enum_encode_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataEnum,
) -> Result<impl ToTokens, syn::Error> {
    let mut variant_patterns = Vec::new();
    let mut variant_lens = Vec::new();
    let mut all_where_bounds = Vec::new();
    // TODO(eliza): support top-level attribute for using the enum's repr
    // instead of its name
    for variant in data.variants {
        let mut name = None;
        for attr in &variant.attrs {
            if attr.path().is_ident(HELPER_ATTR) {
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

        let variant_name = &variant.ident;
        match variant.fields {
            syn::Fields::Unit => {
                variant_patterns.push(quote! {
                    #ident::#variant_name => { e.str(#name)?; }
                });
                variant_lens.push(quote! {
                    if ::microcbor::str_cbor_len(#name) > max {
                        max = ::microcbor::str_cbor_len(#name);
                    }
                });
            }
            syn::Fields::Named(ref fields) => {
                let mut field_gen =
                    FieldGenerator::for_variant(FieldType::Named);
                for field in &fields.named {
                    field_gen.add_field(field)?;
                }
                let FieldGenerator {
                    field_patterns,
                    field_len_exprs,
                    field_encode_exprs,
                    where_bounds,
                    ..
                } = field_gen;
                all_where_bounds.extend(where_bounds);
                let match_pattern = quote! {
                    #ident::#variant_name { #(#field_patterns,)* }
                };
                variant_patterns.push(quote! {
                    #match_pattern => {
                        e.begin_map()?;
                        #(#field_encode_exprs)*
                        e.end()?;
                    }
                });
                variant_lens.push(quote! {
                    #[allow(non_snake_case)]
                    let #variant_name = {
                        let mut len = 2; // map begin and end bytes
                        #(#field_len_exprs;)*
                        len
                    };
                    if #variant_name > max {
                        max = #variant_name;
                    }
                });
            }
            syn::Fields::Unnamed(fields) => {
                let mut field_gen =
                    FieldGenerator::for_variant(FieldType::Unnamed);
                for field in &fields.unnamed {
                    field_gen.add_field(field)?;
                }
                let FieldGenerator {
                    field_patterns,
                    field_len_exprs,
                    field_encode_exprs,
                    where_bounds,
                    ..
                } = field_gen;
                all_where_bounds.extend(where_bounds);
                let match_pattern = quote! {
                    #ident::#variant_name( #(#field_patterns,)* )
                };
                // If exactly one field was generated, encode just that field.
                if let ([len_expr], [encode_expr]) =
                    (&field_len_exprs[..], &field_encode_exprs[..])
                {
                    variant_patterns.push(quote! {
                        #match_pattern => {
                            #encode_expr
                        }
                    });
                    variant_lens.push(quote! {
                        #[allow(non_snake_case)]
                        let #variant_name = {
                            // it's a lil goofy that we still do it this way,
                            // but the len expressions are generated as
                            // `len += ..`
                            let mut len = 0;
                            #len_expr;
                            len
                        };
                        if #variant_name > max {
                            max = #variant_name;
                        }
                    });
                } else {
                    // TODO: Since we don't flatten anything into the array
                    // generated for unnamed fields, we could use the
                    // determinate length encoding and save a byte...
                    variant_patterns.push(quote! {
                        #match_pattern => {
                            e.begin_array()?;
                            #(#field_encode_exprs)*
                            e.end()?;
                        }
                    });
                    variant_lens.push(quote! {
                        #[allow(non_snake_case)]
                        let #variant_name = {
                            let mut len = 2; // array begin and end bytes
                            #(#field_len_exprs;)*
                            len
                        };
                        if #variant_name > max {
                            max = #variant_name;
                        }
                    });
                }
            }
        }
    }
    let (impl_generics, tygenerics, prev_where_clause) =
        generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::microcbor::StaticCborLen
        for #ident #tygenerics
        #prev_where_clause
        where #(#all_where_bounds,)*
        {
            const MAX_CBOR_LEN: usize = {
                let mut max = 0;
                #(#variant_lens;)*
                max
            };
        }

        #[automatically_derived]
        impl #impl_generics ::microcbor::encode::Encode<()>
        for #ident #tygenerics
        #prev_where_clause
        where #(#all_where_bounds,)*
        {
            fn encode<W: ::microcbor::encode::Write>(
                &self,
                e: &mut ::microcbor::encode::Encoder<W>,
                c: &mut (),
            ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                match self {
                    #(#variant_patterns,)*
                }
                Ok(())
            }
        }
    })
}

fn gen_encode_fields_impl(
    input: DeriveInput,
) -> Result<TokenStream, syn::Error> {
    match &input.data {
        syn::Data::Enum(data) => gen_encode_fields_enum_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        syn::Data::Struct(data) => gen_encode_fields_struct_impl(
            input.attrs,
            input.vis,
            input.ident,
            input.generics,
            data.clone(),
        )
        .map(|tokens| tokens.to_token_stream().into()),
        _ => Err(syn::Error::new_spanned(
            input,
            "`microcbor::EncodeFields` can only be derived for `struct` and \
             `enum` types",
        )),
    }
}

fn gen_encode_struct_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataStruct,
) -> Result<impl ToTokens, syn::Error> {
    let field_type = match data.fields {
        syn::Fields::Named(_) => FieldType::Named,
        syn::Fields::Unnamed(_) => FieldType::Unnamed,
        syn::Fields::Unit => {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "`#[derive(microcbor::Encode)]` is not supported on unit \
                 structs",
            ));
        }
    };
    let mut field_gen = FieldGenerator::for_struct(field_type);
    for field in &data.fields {
        field_gen.add_field(field)?;
    }
    let (impl_generics, tygenerics, prev_where_clause) =
        generics.split_for_impl();

    let FieldGenerator {
        where_bounds,
        field_encode_exprs,
        field_len_exprs,
        field_patterns,
        ..
    } = field_gen;

    match (field_type, &field_encode_exprs[..], &field_len_exprs[..]) {
        (FieldType::Named, encode_exprs, len_exprs) => Ok(quote! {
            #[automatically_derived]
            impl #impl_generics ::microcbor::StaticCborLen for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                const MAX_CBOR_LEN: usize = {
                    let mut len = 2;  // map begin and end bytes
                    #(#len_exprs;)*
                    len
                };
            }

            #[automatically_derived]
            impl #impl_generics ::microcbor::encode::Encode<()>
            for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                fn encode<W: ::microcbor::encode::Write>(
                    &self,
                    e: &mut ::microcbor::encode::Encoder<W>,
                    c: &mut (),
                ) -> Result<(), ::microcbor::encode::Error<W::Error>> {

                    let Self { #(#field_patterns,)* } = self;
                    e.begin_map()?;
                    #(#encode_exprs)*
                    e.end()?;
                    Ok(())
                }
            }
        }),
        // If there's exactly one non-skipped field, encode transparently as a
        // single value.
        (FieldType::Unnamed, [encode_expr], [len_expr]) => Ok(quote! {
            #[automatically_derived]
            impl #impl_generics ::microcbor::StaticCborLen for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                const MAX_CBOR_LEN: usize = {
                    let mut len = 0;
                    #len_expr;
                    len
                };
            }

            #[automatically_derived]
            impl #impl_generics ::microcbor::encode::Encode<()>
            for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                fn encode<W: ::microcbor::encode::Write>(
                    &self,
                    e: &mut ::microcbor::encode::Encoder<W>,
                    c: &mut (),
                ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                    let Self( #(#field_patterns,)* ) = self;
                    #encode_expr
                    Ok(())
                }
            }
        }),
        (FieldType::Unnamed, encode_exprs, len_exprs) => Ok(quote! {
            #[automatically_derived]
            impl #impl_generics ::microcbor::StaticCborLen for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                const MAX_CBOR_LEN: usize = {
                    let mut len = 2; // array begin and end bytes
                    #(#len_exprs;)*
                    len
                };
            }

            #[automatically_derived]
            impl #impl_generics ::microcbor::encode::Encode<()>
            for #ident #tygenerics
            #prev_where_clause
            where #(#where_bounds,)*
            {
                fn encode<W: ::microcbor::encode::Write>(
                    &self,
                    e: &mut ::microcbor::encode::Encoder<W>,
                    c: &mut (),
                ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                    let Self( #(#field_patterns,)* ) = self;
                    // TODO: Since we don't flatten anything into the array
                    // generated for unnamed fields, we could use the
                    // determinate length encoding and save a byte...
                    e.begin_array()?;
                    #(#encode_exprs)*
                    e.end()?;
                    Ok(())
                }
            }
        }),
    }
}

fn gen_encode_fields_struct_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataStruct,
) -> Result<impl ToTokens, syn::Error> {
    let syn::Fields::Named(ref fields) = data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "`microcbor::EncodeFields` may only be derived for structs with \
             named fields",
        ));
    };
    let mut field_gen = FieldGenerator::for_struct(FieldType::Named);
    for field in &fields.named {
        field_gen.add_field(field)?;
    }
    let (impl_generics, tygenerics, prev_where_clause) =
        generics.split_for_impl();

    let FieldGenerator {
        where_bounds,
        field_encode_exprs,
        field_len_exprs,
        field_patterns,
        ..
    } = field_gen;

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::microcbor::EncodeFields<()>
        for #ident #tygenerics
        #prev_where_clause
        where #(#where_bounds,)*
        {
            const MAX_FIELDS_LEN: usize = {
                let mut len = 0;
                #(#field_len_exprs;)*
                len
            };

            fn encode_fields<W: ::microcbor::encode::Write>(
                &self,
                e: &mut ::microcbor::encode::Encoder<W>,
                c: &mut (),
            ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                let Self { #(#field_patterns,)* } = self;
                #(#field_encode_exprs)*
                Ok(())
            }
        }
    })
}

fn gen_encode_fields_enum_impl(
    _attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataEnum,
) -> Result<impl ToTokens, syn::Error> {
    let mut variant_patterns = Vec::new();
    let mut variant_lens = Vec::new();
    let mut all_where_bounds = Vec::new();
    for variant in data.variants {
        let variant_name = &variant.ident;
        let syn::Fields::Named(ref fields) = variant.fields else {
            // If there's a unit variant, we cannot generate an
            // `EncodeField` impl for flattening this type.
            return Err(syn::Error::new_spanned(
                &variant,
                "deriving `microcbor::EncodeFields` for an `enum` type \
                 requires that all variants have named fields",
            ));
        };

        let mut field_gen = FieldGenerator::for_variant(FieldType::Named);
        for field in &fields.named {
            field_gen.add_field(field)?;
        }
        let FieldGenerator {
            field_patterns,
            field_len_exprs,
            field_encode_exprs,
            where_bounds,
            ..
        } = field_gen;
        all_where_bounds.extend(where_bounds);
        let match_pattern = quote! {
            #ident::#variant_name { #(#field_patterns,)* }
        };
        variant_lens.push(quote! {
            #[allow(non_snake_case)]
            let #variant_name = {
                // no map begin and end bytes, as we are flattening
                // the fields into a higher-level map.
                let mut len = 0;
                #(#field_len_exprs;)*
                len
            };
            if #variant_name > max {
                max = #variant_name;
            }
        });
        variant_patterns.push(quote! {
            #match_pattern => {
                #(#field_encode_exprs)*
            }
        });
    }
    let (impl_generics, tygenerics, prev_where_clause) =
        generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::microcbor::EncodeFields<()>
        for #ident #tygenerics
        #prev_where_clause
        where #(#all_where_bounds,)*
        {
            const MAX_FIELDS_LEN: usize = {
                let mut max = 0;
                #(#variant_lens;)*
                max
            };

            fn encode_fields<W: ::microcbor::encode::Write>(
                &self,
                e: &mut ::microcbor::encode::Encoder<W>,
                c: &mut (),
            ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                match self {
                    #(#variant_patterns,)*
                }
                Ok(())
            }
        }
    })
}

struct FieldGenerator {
    field_patterns: Vec<proc_macro2::TokenStream>,
    field_len_exprs: Vec<proc_macro2::TokenStream>,
    field_encode_exprs: Vec<proc_macro2::TokenStream>,
    where_bounds: Vec<proc_macro2::TokenStream>,
    any_skipped: bool,
    field_type: FieldType,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum FieldType {
    Named,
    Unnamed,
}

impl FieldGenerator {
    fn for_struct(field_type: FieldType) -> Self {
        Self {
            field_patterns: Vec::new(),
            field_len_exprs: Vec::new(),
            field_encode_exprs: Vec::new(),
            where_bounds: Vec::new(),
            any_skipped: false,
            field_type,
        }
    }

    fn for_variant(field_type: FieldType) -> Self {
        Self {
            field_patterns: Vec::new(),
            field_len_exprs: Vec::new(),
            field_encode_exprs: Vec::new(),
            where_bounds: Vec::new(),
            any_skipped: false,
            field_type,
        }
    }

    fn add_field(&mut self, field: &syn::Field) -> Result<(), syn::Error> {
        let mut field_name = None;
        let mut skipped = false;
        let mut flattened = false;
        let mut skipped_if_nil = false;
        for attr in &field.attrs {
            if attr.path().is_ident(HELPER_ATTR) {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        if field.ident.is_none() {
                            return Err(meta.error(
                                "`#[cbor(rename = \"...\")]` is only \
                                supported on named fields",
                            ));
                        }
                        field_name = Some(meta.value()?.parse::<LitStr>()?);
                        Ok(())
                    } else if meta.path.is_ident("skip") {
                        skipped = true;
                        Ok(())
                    } else if meta.path.is_ident("skip_if_nil") {
                        skipped_if_nil = true;
                        Ok(())
                    } else if meta.path.is_ident("flatten") {
                        if self.field_type == FieldType::Unnamed {
                            return Err(meta.error(
                                "`#[cbor(flatten)]` is only supported on \
                                 structs and enum variants with named fields",
                            ));
                        }
                        flattened = true;
                        Ok(())
                    } else {
                        Err(meta.error(
                            "expected `rename`, `skip`, `skip_if_nil`, or \
                             `flatten` attribute",
                        ))
                    }
                })?;
            }
        }
        if skipped {
            self.any_skipped = true;
        }

        let (field_ident, encode_name, name_len) =
            match (self.field_type, skipped) {
                (FieldType::Unnamed, true) => {
                    self.field_patterns.push(quote! { _ });
                    return Ok(());
                }
                (FieldType::Named, true) => {
                    let field_ident = field.ident.as_ref().unwrap();
                    self.field_patterns.push(quote! { #field_ident: _ });
                    return Ok(());
                }
                (FieldType::Named, false) => {
                    let field_ident = field.ident.as_ref().expect(
                        "if we are generating named fields, there should \
                             be an ident for each field",
                    );
                    let field_name = field_name.unwrap_or_else(|| {
                        LitStr::new(
                            &field_ident.to_string(),
                            field_ident.span(),
                        )
                    });
                    self.field_patterns.push(quote! { #field_ident });
                    let encode_name = quote! {
                        e.str(#field_name)?;
                    };
                    let name_len = quote! {
                        len += ::microcbor::str_cbor_len(#field_name);
                    };
                    (field_ident.clone(), encode_name, name_len)
                }
                (FieldType::Unnamed, false) => {
                    let num = self.field_patterns.len();
                    let field_ident = format_ident!("__field_{num}");
                    self.field_patterns.push(quote! { #field_ident });
                    let encode_name = quote! {};
                    let name_len = quote! {};

                    (field_ident, encode_name, name_len)
                }
            };

        // TODO(eliza): if we allow more complex ways of encoding fields as
        // different CBOR types, this will have to handle that...
        let field_type = &field.ty;
        if flattened {
            self.where_bounds.push(quote! {
                #field_type: ::microcbor::EncodeFields<()>
            });
            self.field_len_exprs.push(quote! {
                len += <#field_type as ::microcbor::EncodeFields<()>>::MAX_FIELDS_LEN;
            });
            self.field_encode_exprs.push(quote! {
                ::microcbor::EncodeFields::<()>::encode_fields(#field_ident, e, c)?;
            });
        } else {
            self.field_len_exprs.push(quote! {
                #name_len
                len += <#field_type as ::microcbor::StaticCborLen>::MAX_CBOR_LEN;
            });
            self.field_encode_exprs.push(if skipped_if_nil {
                quote! {
                    if !::microcbor::Encode::<()>::is_nil(#field_ident) {
                        #encode_name
                        ::microcbor::Encode::<()>::encode(#field_ident, e, c)?;
                    }
                }
            } else {
                quote! {
                    #encode_name
                    ::microcbor::Encode::<()>::encode(#field_ident, e, c)?;
                }
            });
            self.where_bounds.push(quote! {
                #field_type: ::microcbor::StaticCborLen
            });
        }

        Ok(())
    }
}
