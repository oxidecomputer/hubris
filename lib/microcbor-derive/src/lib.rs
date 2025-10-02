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

/// Derives an implementation of the [`Encode`] and `StaticCborLen` traits for the
/// annotated `struct` or `enum` type.
///
/// All fields of the deriving type must implement the [`Encode`] and
/// `StaticCborLen` traits, with the following exceptions:
///
/// - If the field is annotated with the `#[cbor(skip)]` attribute,
///   it need not implement any traits, as it is skipped.
/// - If the field is annotated with the `#[cbor(flatten)]` attribute,
///   it must instead implement the [`EncodeFields`] trait.
///
/// Because fields must implement `StaticCborLen`, the maximum length in bytes
/// of the encoded representation can be computed at compile-time.
///
/// # Encoding
///
/// The generated CBOR is encoded as follows:
///
/// - Structs with named fields, and struct-like enum variants, are encoded
///   as CBOR maps of strings to values. The keys in the encoded map are the
///   string representations of the Rust identifier names of the encoded
///   fields, unless overridden by the `#[cbor(rename = "..")]` attribute.
/// - Structs with unnamed fields ("tuple structs") and enum variants with
///   unnamed fields are encoded as CBOR arrays of the values of those
///   fields, in declaration order.
/// - If a tuple struct or tuple-like enum variant has only a single field,
///   it is encoded "transparently", i.e. as the CBOR value of that field,
///   rather than as a single-element array.
/// - Unit enum variants are encoded as strings. By default, the string
///   representation is the Rust identifier name of the variant, unless
///   overridden by the `#[cbor(rename = "...")]` attribute.
///
///   Someday, I may add a way to encode enum variants as their `repr`
///   values, but I haven't done that yet.
///
/// ## Tagged Enum Encoding
///
/// The `#[cbor(tag = "...")]` attribute may be placed on an enum type to encode
/// its variants with a tag field, similar to [`serde`'s "internally tagged" enum
/// representations](https://serde.rs/enum-representations.html#internally-tagged).
///
/// If the `#[cbor(tag = "tag_field_name")]` attribute is present, any variant
/// of the enum will additionally encode a key-value pair where the key is the
/// provided tag field name, and the value is the variant's name (or the value
/// of a `#[cbor(rename = "...")]` attribute on that variant if one is present).
///
/// When the enum derives `#[microcbor::Encode]`, it will be encoded as a map
/// with the tag key-value pair added (in addition to any other fields defined
/// by the enum) variant). If the variant has no other fields, the map will
/// contain only the tag key-value pair.
///
/// When the enum derives `#[microcbor::EncodeFields]`, the tag field will be
/// added to the parent map into which the encoded fields are flattened. If the
/// enum has no other fields, only one additional key-value pair will be added.
///
/// **Note**: The tagged representation is not supported for tuple-like enum
/// variants with unnamed fields.
///
/// For example:
/// ```rust
/// #[derive(microcbor::Encode, microcbor::EncodeFields)]
/// #[cbor(tag = "type")]
/// enum MyEnum {
///     // will encode as { "type": "Variant1" }
///     Variant1,
///     // will encode as { "type": "Variant2", "a": 1, "b": 2 }
///     Variant2 { a: u64, b: u64 },
///     // will encode as { "type": "my_cool_variant", "c": 1, "d": 2 }
///     #[cbor(rename = "my_cool_variant")]
///     Variant3 { c: u64, d: u64 },
///     // will encode as { "type": "my_cool_unit_variant"}
///     #[cbor(rename = "my_cool_unit_variant")]
///     Variant4,
/// }
/// ```
///
/// # Helper Attributes
///
/// This derive macro supports a `#[cbor(...)]` attribute, which may be placed
/// on fields or variants of a deriving type to modify how they are encoded.
///
/// ## Enum Type Definition Attributes
///
/// The following `#[cbor(...)]` attributes are may be placed on the *definition*
/// of an enum type:
///
/// - `#[cbor(tag = "..")]`: Uses the [tagged enum
///   representation](#tagged-enum-representation) with the specified tag name
///   when encoding this enum. Note that this attribute may *not* be used on
///   enums which have tuple-like (unnamed fields) variants.
///
/// ## Field Attributes
///
/// The following `#[cbor(..)]` attributes are supported on fields of structs
/// and enum variants:
///
/// - `#[cbor(skip)]`: Completely skip ignoring this field. If a field is
///   skipped, it will not be included in the encoded CBOR output.
///
/// - `#[cbor(skip_if_nil)]`: Skip encoding this field if it would encode a
///   CBOR `nil` value.
///
///   This attribute will cause the generated `Encode` implementation to call
///   the value's `Encode::is_nil` method to determine if the field would emit
///   a `nil` value. If it returns `true`, the field will no tbe encoded at
///   all.
///
/// - `#[cbor(flatten)]`: Flatten this field into the CBOR map generated for
///   the enclosing type, rather than as a nested CBOR map.
///
///   This attribute may only be placed on fields which are of types that
///   implement the [`EncodeFields`] trait. [`EncodeFields`] may be derived
///   for any struct or enum type which has named fields.
///
///   Only structs and enum variants whose fields are named may use the
///   `#[cbor(flatten)]` attribute on their fields. Using `#[cbor(flatten)]`
///   on fields of a tuple struct or tuple-like enum variant will result in a
///   compile error. An enum type which has both struct-like and tuple-like
///   variants *may* use `#[cbor(flatten)]`, but only within its struct-like
///  variants.
///
/// - `#[cbor(rename = "...")]`: Use a different name for this field when
///   encoding it as CBOR.
///
///   This attribute will cause the field to be encoded with the string
///   provided in the `rename` attribute as its key, rather than the Rust
///   field name. This attribute may, of course, only be used on structs
///   and enum variants with named fields.
///
/// ## Variant Attributes
///
/// The following `#[cbor(..)]` attributes may be placed on variants of
/// an enum type:
///
/// - `#[cbor(rename = "...")]`: Use a different name for this variant when
///   encoding it as CBOR.
///
///   Enum variants without fields are encoded as strings. By default, the
///   Rust identifier is used as the encoded representation of a unit
///   variant. If the variant is annotated with the `#[cbor(rename = "...")]`
///   attribute, the provided string constant will be used as the encoded
///   representation of the variant, instead.
///
///   This attribute may only be placed on unit variants, unless the enum type
///   uses the tagged representation.
#[proc_macro_derive(Encode, attributes(cbor))]
pub fn derive_encode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match gen_encode_impl(input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error().into(),
    }
}

/// Derives an implementation of the [`EncodeFields`] trait for the annotated
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
///
/// # Helper Attributes
///
/// All [the attributes](macro@Encode#helper-attributes) recognized by
/// `#[derive(Encode)]` may also be placed on the fields of a type that derives
/// `EncodeFields`.
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
const RENAME_ATTR: &str = "rename";

fn gen_enum_encode_impl(
    attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataEnum,
) -> Result<impl ToTokens, syn::Error> {
    // TODO(eliza): support top-level attribute for using the enum's repr
    // instead of its name
    let EnumDefAttrs { tag_field_name } = EnumDefAttrs::parse(&attrs)?;
    let mut variant_patterns = Vec::new();
    let mut variant_lens = Vec::new();
    let mut all_where_bounds = Vec::new();

    for variant in data.variants {
        let EnumVariantAttrs { rename } =
            EnumVariantAttrs::parse(&variant.attrs)?;
        let name = rename.unwrap_or_else(|| {
            LitStr::new(&variant.ident.to_string(), variant.ident.span())
        });

        let variant_name = &variant.ident;
        match variant.fields {
            syn::Fields::Unit => match tag_field_name {
                None => {
                    variant_patterns.push(quote! {
                        #ident::#variant_name => {
                            __microcbor_renamed_encoder.str(#name)?;
                        }
                    });
                    variant_lens.push(quote! {
                        if ::microcbor::str_cbor_len(#name) > max {
                            max = ::microcbor::str_cbor_len(#name);
                        }
                    });
                }
                Some(ref tag_field_name) => {
                    variant_patterns.push(quote! {
                        #ident::#variant_name => {
                            __microcbor_renamed_encoder
                                .map(1)?
                                .str(#tag_field_name)?
                                .str(#name)?;
                        }
                    });
                    variant_lens.push(quote! {
                        #[allow(non_snake_case)]
                        let #variant_name = {
                            // this will encode exactly 1 field, so we use the
                            // length-prefixed repr to save a byte.
                            let mut len = ::microcbor::u64_cbor_len(1);
                            len += ::microcbor::str_cbor_len(#tag_field_name);
                            len += ::microcbor::str_cbor_len(#name);
                            len
                        };
                        if #variant_name > max {
                            max = #variant_name;
                        }
                    });
                }
            },
            syn::Fields::Named(ref fields) => {
                let mut field_gen =
                    FieldGenerator::for_variant(FieldType::Named);
                for field in &fields.named {
                    field_gen.add_field(field)?;
                }
                if let Some(ref tag_field_name) = tag_field_name {
                    field_gen.add_tag_field(tag_field_name, &name);
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
                        __microcbor_renamed_encoder.begin_map()?;
                        #(#field_encode_exprs)*
                        __microcbor_renamed_encoder.end()?;
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
                if let Some(ref tag_field_name) = tag_field_name {
                    field_gen.add_tag_field(tag_field_name, &name);
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
                            __microcbor_renamed_encoder.begin_array()?;
                            #(#field_encode_exprs)*
                            __microcbor_renamed_encoder.end()?;
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
                __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                __microcbor_renamed_ctx: &mut (),
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
                    __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                    __microcbor_renamed_ctx: &mut (),
                ) -> Result<(), ::microcbor::encode::Error<W::Error>> {

                    let Self { #(#field_patterns,)* } = self;
                    __microcbor_renamed_encoder.begin_map()?;
                    #(#encode_exprs)*
                    __microcbor_renamed_encoder.end()?;
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
                    __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                    __microcbor_renamed_ctx: &mut (),
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
                    __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                    __microcbor_renamed_ctx: &mut (),
                ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                    let Self( #(#field_patterns,)* ) = self;
                    // TODO: Since we don't flatten anything into the array
                    // generated for unnamed fields, we could use the
                    // determinate length encoding and save a byte...
                    __microcbor_renamed_encoder.begin_array()?;
                    #(#encode_exprs)*
                    __microcbor_renamed_encoder.end()?;
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
                __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                __microcbor_renamed_ctx: &mut (),
            ) -> Result<(), ::microcbor::encode::Error<W::Error>> {
                let Self { #(#field_patterns,)* } = self;
                #(#field_encode_exprs)*
                Ok(())
            }
        }
    })
}

fn gen_encode_fields_enum_impl(
    attrs: Vec<Attribute>,
    _vis: Visibility,
    ident: Ident,
    generics: Generics,
    data: DataEnum,
) -> Result<impl ToTokens, syn::Error> {
    let EnumDefAttrs { tag_field_name } = EnumDefAttrs::parse(&attrs)?;
    let mut variant_patterns = Vec::new();
    let mut variant_lens = Vec::new();
    let mut all_where_bounds = Vec::new();
    for variant in data.variants {
        let variant_name = &variant.ident;
        let EnumVariantAttrs { rename } =
            EnumVariantAttrs::parse(&variant.attrs)?;

        let mut field_gen = FieldGenerator::for_variant(FieldType::Named);
        match variant.fields {
            syn::Fields::Named(ref fields) => {
                for field in &fields.named {
                    field_gen.add_field(field)?;
                }
            }
            syn::Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &variant,
                    "`microcbor::EncodeFields` cannot be derived for an `enum` \
                    type with unnamed (tuple-like) variants",
                ));
            }
            syn::Fields::Unit if tag_field_name.is_some() => {}
            syn::Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    &variant,
                    "`microcbor::EncodeFields` may only be derived for an \
                    `enum` type with unit variants if the enum has the \
                     `#[cbor(tag = \"...\")]` attribute",
                ));
            }
        };

        if let Some(ref tag_field) = tag_field_name {
            match rename {
                Some(tag) => field_gen.add_tag_field(tag_field, &tag),
                None => {
                    let tag = LitStr::new(
                        &variant.ident.to_string(),
                        variant.ident.span(),
                    );
                    field_gen.add_tag_field(tag_field, &tag);
                }
            };
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
                __microcbor_renamed_encoder: &mut ::microcbor::encode::Encoder<W>,
                __microcbor_renamed_ctx: &mut (),
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

    fn add_tag_field(&mut self, tag_field_name: &LitStr, tag: &LitStr) {
        self.field_len_exprs.push(quote! {
            len += ::microcbor::str_cbor_len(#tag_field_name)
        });
        self.field_len_exprs.push(quote! {
            len += ::microcbor::str_cbor_len(#tag)
        });
        self.field_encode_exprs.push(quote! {
            __microcbor_renamed_encoder
                .str(#tag_field_name)?
                .str(#tag)?;
        });
    }

    fn add_field(&mut self, field: &syn::Field) -> Result<(), syn::Error> {
        let mut field_name = None;
        let mut skipped = false;
        let mut flattened = false;
        let mut skipped_if_nil = false;
        for attr in &field.attrs {
            if attr.path().is_ident(HELPER_ATTR) {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident(RENAME_ATTR) {
                        if field.ident.is_none() {
                            return Err(meta.error(format!(
                                "`#[cbor({RENAME_ATTR} = \"...\")]` is only \
                                supported on named fields",
                            )));
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
                        Err(meta.error(format!(
                            "expected `{RENAME_ATTR}`, `skip`, `skip_if_nil`, or \
                             `flatten` attribute",
                        )))
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
                        __microcbor_renamed_encoder.str(#field_name)?;
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
                ::microcbor::EncodeFields::<()>::encode_fields(
                    #field_ident,
                    __microcbor_renamed_encoder,
                    __microcbor_renamed_ctx,
                )?;
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
                        ::microcbor::Encode::<()>::encode(
                            #field_ident,
                            __microcbor_renamed_encoder,
                            __microcbor_renamed_ctx,
                        )?;
                    }
                }
            } else {
                quote! {
                    #encode_name
                    ::microcbor::Encode::<()>::encode(
                        #field_ident,
                        __microcbor_renamed_encoder,
                        __microcbor_renamed_ctx,
                    )?;
                }
            });
            self.where_bounds.push(quote! {
                #field_type: ::microcbor::StaticCborLen
            });
        }

        Ok(())
    }
}

struct EnumDefAttrs {
    /// Are we asked to generate a tag field?
    tag_field_name: Option<LitStr>,
}

impl EnumDefAttrs {
    fn parse(attrs: &[Attribute]) -> Result<Self, syn::Error> {
        const TAG: &str = "tag";

        let mut tag_field_name = None;
        for attr in attrs {
            if attr.path().is_ident(HELPER_ATTR) {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident(TAG) {
                        tag_field_name = Some(meta.value()?.parse::<LitStr>()?);
                        Ok(())
                    } else {
                        Err(meta.error(format!("expected `{TAG}` attribute")))
                    }
                })?;
            };
        }
        Ok(Self { tag_field_name })
    }
}

struct EnumVariantAttrs {
    rename: Option<LitStr>,
}

impl EnumVariantAttrs {
    fn parse(attrs: &[Attribute]) -> Result<Self, syn::Error> {
        let mut rename = None;
        for attr in attrs {
            if attr.path().is_ident(HELPER_ATTR) {
                attr.meta.require_list()?.parse_nested_meta(|meta| {
                    if meta.path.is_ident(RENAME_ATTR) {
                        rename = Some(meta.value()?.parse::<LitStr>()?);
                        Ok(())
                    } else {
                        Err(meta.error(format!(
                            "expected `{RENAME_ATTR}` attribute"
                        )))
                    }
                })?;
            };
        }
        Ok(Self { rename })
    }
}
