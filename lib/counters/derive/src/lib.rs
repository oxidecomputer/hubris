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

/// Derives an implementation of the `Count` trait for the annotated
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
    let data_enum = match input.data {
        syn::Data::Enum(ref data_enum) => data_enum,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "`Count` can only be derived for enums",
            ));
        }
    };
    let variants = &data_enum.variants;
    let mut state = CountGenerator::new(&input.ident, variants.len());

    for variant in variants {
        state.add_variant(variant)?;
    }

    Ok(state.generate(&input))
}

struct CountGenerator<'input> {
    enum_name: &'input syn::Ident,
    field_defs: Vec<proc_macro2::TokenStream>,
    field_inits: Vec<proc_macro2::TokenStream>,
    variant_patterns: Vec<proc_macro2::TokenStream>,
    any_skipped: bool,
}

impl<'input> CountGenerator<'input> {
    fn new(enum_name: &'input syn::Ident, variants: usize) -> Self {
        Self {
            enum_name,
            field_defs: Vec::with_capacity(variants),
            field_inits: Vec::with_capacity(variants),
            variant_patterns: Vec::with_capacity(variants),
            any_skipped: false,
        }
    }

    fn generate(self, input: &DeriveInput) -> impl ToTokens {
        let Self {
            enum_name,
            field_defs,
            field_inits,
            mut variant_patterns,
            any_skipped,
        } = self;

        // If we skipped any variants, generate a catchall case.
        if any_skipped {
            variant_patterns.push(quote! { _ => {} });
        }
        let vis = &input.vis;

        let counts_ty = counts_ty(enum_name);
        quote! {
            #[doc = concat!("Total counts for [`", stringify!(#enum_name), "`].")]
            #[allow(nonstandard_style)]
            #vis struct #counts_ty {
                #(#field_defs),*
            }

            #[automatically_derived]
            impl counters::Count for #enum_name {
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
                    use counters::rmv6m_atomic_hack::AtomicU32Ext;

                    match self {
                        #(#variant_patterns),*
                    };
                }
            }
        }
    }

    fn add_variant(
        &mut self,
        variant: &syn::Variant,
    ) -> Result<(), syn::Error> {
        for attr in &variant.attrs {
            if !attr.path().is_ident("count") {
                continue;
            }
            attr.parse_args_with(SkipAttr::parse)?;
            self.any_skipped = true;
            return Ok(());
        }
        let enum_name = self.enum_name;
        let variant_name = &variant.ident;
        match &variant.fields {
            syn::Fields::Unit => {
                self.variant_patterns.push(quote! { #enum_name::#variant_name => {
                    counters.#variant_name.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                } });
                self.add_def_init(variant_name);
            }
            ref fields => {
                if let Some((i, counted_field)) = find_counted_field(fields)? {
                    self.add_count_children_def_init(
                        variant_name,
                        &counted_field.ty,
                    );
                    if let syn::Fields::Named(_) = fields {
                        let field_name = counted_field.ident.as_ref().unwrap();
                        self.variant_patterns.push(
                            quote! { #enum_name::#variant_name { ref #field_name, .. } => {
                                #field_name.count(&counters.#variant_name);
                            } },
                        );
                    } else {
                        let mut pattern = Vec::new();
                        for _ in 0..i {
                            pattern.push(quote! { _, });
                        }
                        pattern.push(quote! { ref f, });
                        // is the counted field the last one? if not, add a `..`.
                        if fields.len() > i + 1 {
                            pattern.push(quote! { .. });
                        }
                        self.variant_patterns.push(
                            quote! { #enum_name::#variant_name(#(#pattern)*) => {
                                f.count(&counters.#variant_name);
                            } },
                        );
                    }
                } else {
                    self.add_def_init(variant_name);
                    if let syn::Fields::Named(_) = fields {
                        self.variant_patterns.push(quote! {
                            #enum_name::#variant_name { .. } => {
                                counters.#variant_name.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                            }
                        });
                    } else {
                        self.variant_patterns.push(quote! {
                            #enum_name::#variant_name(..) => {
                                counters.#variant_name.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                            }
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Generate a field definition and field initializer for a variant
    /// *without* the `#{count(children)]` annotation.
    fn add_def_init(&mut self, variant_name: &syn::Ident) {
        let Self {
            field_defs,
            field_inits,
            enum_name,
            ..
        } = self;
        field_defs.push(quote! {
            #[doc = concat!(
                " The total number of times a [`",
                stringify!(#enum_name), "::", stringify!(#variant_name),
                "`]"
            )]
            #[doc = " has been recorded by this set of counters."]
            pub #variant_name: core::sync::atomic::AtomicU32
        });
        field_inits.push(
            quote! { #variant_name: core::sync::atomic::AtomicU32::new(0) },
        );
    }

    /// Generate a field def and field initializer for a variant *with*
    /// the `#{count(children)]` annotation.
    fn add_count_children_def_init(
        &mut self,
        variant_name: &syn::Ident,
        variant_type: &syn::Type,
    ) {
        let Self {
            field_defs,
            field_inits,
            enum_name,
            ..
        } = self;
        field_defs.push(quote! {
            #[doc = concat!(
                " The total number of times a [`",
                stringify!(#enum_name), "::", stringify!(#variant_name),
                "`]"
            )]
            #[doc = " has been recorded by this set of counters."]
            pub #variant_name: <#variant_type as counters::Count>::Counters
        });
        field_inits.push(quote! {
            #variant_name: <#variant_type as counters::Count>::NEW_COUNTERS
        });
    }
}

fn find_counted_field(
    fields: &syn::Fields,
) -> syn::Result<Option<(usize, &syn::Field)>> {
    let mut counted_field = None;
    for (i, field) in fields.iter().enumerate() {
        for attr in &field.attrs {
            if !attr.path().is_ident("count") {
                continue;
            }
            attr.parse_args_with(ChildrenAttr::parse)?;

            // TODO(eliza): relax this restriction eventually?
            if counted_field.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "a variant may only have one field annotated \
                    with `#[count(children)]`",
                ));
            } else {
                counted_field = Some((i, field));
            }
        }
    }

    Ok(counted_field)
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct SkipAttr;

#[derive(Copy, Clone, PartialEq, Eq)]
struct ChildrenAttr;

impl Parse for SkipAttr {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let ident = input.fork().parse::<syn::Ident>()?;
        if ident == "skip" {
            // consume the token
            let _: syn::Ident = input.parse()?;
            Ok(Self)
        } else {
            Err(syn::Error::new(
                ident.span(),
                "unrecognized `#[count]` attribute, expected `#[count(skip)]`",
            ))
        }
    }
}

impl Parse for ChildrenAttr {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let ident = input.fork().parse::<syn::Ident>()?;
        if ident == "children" {
            // consume the token
            let _: syn::Ident = input.parse()?;
            Ok(Self)
        } else {
            Err(syn::Error::new(
                ident.span(),
                "unrecognized `#[count]` attribute, expected `#[count(children)]`",
            ))
        }
    }
}

fn counts_ty(ident: &Ident) -> Ident {
    Ident::new(&format!("{ident}Counts"), Span::call_site())
}
