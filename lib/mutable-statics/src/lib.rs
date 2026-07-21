// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

/// A macro to facilitate declaring mutable statics safely using the
/// "first-mover" pattern.
///
/// Any given use of this macro can only execute once. If execution reaches it
/// again, it will panic. It uses this behavior to ensure that it can hand out
/// &mut references to statics, which are declared within the macro.
///
/// The macro accepts definitions of one or more mutable static arrays. It will
/// arrange for them to be initialized by a per-array lambda function, and
/// return a tuple containing mutable references to each, in the order they're
/// declared.
///
/// NOTE: You may prefer this over the `static-cell` crate, as it uses a closure
/// to initialize each field, which means that if you have a `[T; N]` where `T`
/// is not zero-initialized, THIS crate will only store a single `T` in `.text`,
/// whereas static-cell (as of July 2026) will store the entire `[T; N]` into
/// `.data`, which costs both flash (for the initializer) and RAM (for the
/// actual static).
#[macro_export]
macro_rules! mutable_statics {
    (
        $(
            $(#[$attr:meta])*
            static mut $name:ident: [$t:ty; $n:expr] = [$init:expr; _];
        )*
    ) => {{
        static TAKEN: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        if TAKEN.swap(true, core::sync::atomic::Ordering::Relaxed) {
            panic!()
        }
        (
            $(
                {
                    $(#[$attr])*
                    static mut $name: core::mem::MaybeUninit<[$t; $n]> =
                        core::mem::MaybeUninit::uninit();
                    // Safety: unsafe because of reference to mutable static;
                    // safe because the AtomicBool swap above, combined with the
                    // lexical scoping of $name, means that this reference can't
                    // be aliased by any other reference in the program.
                    let __ref = unsafe {
                        &mut $name
                    };
                    // Dereferencing from `MaybeUninit<[$t; $n]>` to
                    // `[MaybeUninit<$t>; $n]` (which is safe to do).
                    let __ref: &'static mut [core::mem::MaybeUninit<$t>; $n] =
                        __ref.as_mut();

                    // Initialize each field using the provided closure (which
                    // is also safe to do).
                    __ref
                        .iter_mut()
                        .for_each(|mu| { mu.write($init()); });

                    // Safety: unsafe because of the transmute, from
                    // `&'static [MaybeUninit<$t>; $n]` to `&'static [$t; $n]`,
                    // safe because we are only doing so after initializing
                    // every field
                    let __ref: &'static mut [$t; $n] = unsafe {
                        core::mem::transmute(__ref)
                    };
                    __ref
                }
            ),*
        )
    }};
}
