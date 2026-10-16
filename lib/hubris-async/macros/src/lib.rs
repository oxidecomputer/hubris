// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Procedural macros for `hubris-async`. Use them through `hubris_async`'s
//! re-exports, not directly.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Error, FnArg, ItemFn, ReturnType, Type, parse_macro_input, spanned::Spanned,
};

/// The most arguments a task can take. This must match the `TaskFn` impls in
/// `hubris_async::task`.
const MAX_ARGS: usize = 4;

/// Declares a statically-allocated async task.
///
/// ```ignore
/// #[hubris_async::task]
/// async fn blinky(period: u64) {
///     let mut ticker = Ticker::every(period);
///     loop {
///         ticker.next().await;
///         // ...
///     }
/// }
///
/// executor.spawn(blinky(500));
/// ```
///
/// This turns the `async fn` into a plain `fn` with the same arguments that
/// returns a `SpawnToken`, which can be handed to `Executor::spawn`. The
/// future's storage is a `static` sized exactly to fit the future, so no
/// allocator is required.
///
/// Each task can only be spawned once; calling the function a second time
/// panics.
///
/// The task must return `()`, can't be generic (including `impl Trait`
/// arguments) or take `self`, and takes at most four arguments. Arguments
/// must be `'static`, since the task outlives the call that spawns it.
#[proc_macro_attribute]
pub fn task(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let f = parse_macro_input!(item as ItemFn);
    expand_task(attr, f)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_task(attr: TokenStream2, f: ItemFn) -> syn::Result<TokenStream2> {
    if !attr.is_empty() {
        return Err(Error::new_spanned(
            attr,
            "`#[hubris_async::task]` takes no arguments",
        ));
    }

    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = f;

    // Check everything up front, so mistakes get a clear error here rather
    // than a confusing one from the generated code.
    if sig.asyncness.is_none() {
        return Err(Error::new(
            sig.fn_token.span(),
            "async tasks must be `async fn`",
        ));
    }
    if let Some(c) = &sig.constness {
        return Err(Error::new(c.span(), "async tasks can't be `const`"));
    }
    if let Some(u) = &sig.unsafety {
        return Err(Error::new(u.span(), "async tasks can't be `unsafe`"));
    }
    if let Some(abi) = &sig.abi {
        return Err(Error::new_spanned(abi, "async tasks can't set an ABI"));
    }
    if let Some(v) = &sig.variadic {
        return Err(Error::new_spanned(v, "async tasks can't be variadic"));
    }
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &sig.generics,
            "async tasks can't be generic: each one has a single static slot \
             sized for its future",
        ));
    }
    if let ReturnType::Type(_, ty) = &sig.output
        && !is_unit(ty)
    {
        return Err(Error::new_spanned(ty, "async tasks must return `()`"));
    }
    if sig.inputs.len() > MAX_ARGS {
        return Err(Error::new_spanned(
            &sig.inputs,
            format!("async tasks can take at most {MAX_ARGS} arguments"),
        ));
    }

    // The generated `fn` takes the same arguments, but binds each to a plain
    // name, so the task can use any pattern for its parameters (`mut x`,
    // tuples, ...).
    let mut arg_names = Vec::new();
    let mut arg_types = Vec::new();
    for (i, arg) in sig.inputs.iter().enumerate() {
        match arg {
            FnArg::Receiver(r) => {
                return Err(Error::new_spanned(
                    r,
                    "async tasks can't take `self`",
                ));
            }
            FnArg::Typed(pt) => {
                if let Type::ImplTrait(_) = &*pt.ty {
                    return Err(Error::new_spanned(
                        &pt.ty,
                        "async tasks can't take `impl Trait` arguments: \
                         each one has a single static slot sized for its \
                         future",
                    ));
                }
                arg_names.push(format_ident!("__hubris_async_arg{i}"));
                arg_types.push(&pt.ty);
            }
        }
    }

    let name = &sig.ident;
    let inputs = &sig.inputs;
    let output = &sig.output;

    // The task's body becomes an inner `async fn` with the original name and
    // signature, so it shows up as `name::name` in symbols and panics. The
    // static's name is mangled so it can't collide with anything the body
    // can see.
    let storage = format_ident!("__HUBRIS_ASYNC_TASK_STORAGE");
    Ok(quote! {
        #(#attrs)*
        #vis fn #name(#(#arg_names: #arg_types),*) -> ::hubris_async::SpawnToken {
            async fn #name(#inputs) #output #block

            static #storage: ::hubris_async::TaskStorage<
                { ::hubris_async::__future_size(&#name) },
            > = ::hubris_async::TaskStorage::new();

            #storage.spawn(#name(#(#arg_names),*))
        }
    })
}

fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}
