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
                    // Safety: unsafe because of dereference of a raw pointer
                    // (after we cast it) -- safe because we are casting here
                    // from MaybeUninit<[$t; $n]> to [MaybeUninit<$t>; $n],
                    // which is safe by definition.
                    let __ref: &'static mut [core::mem::MaybeUninit<$t>; $n] =
                        unsafe {
                            &mut *(__ref as *mut _ as *mut _)
                        };
                    for __u in __ref.iter_mut() {
                        *__u = core::mem::MaybeUninit::new($init());
                    }
                    // Safety: unsafe because of dereference of a raw pointer
                    // (after we cast it) -- safe because we are casting here
                    // from [MaybeUninit<$t>; $n] to [$t; $n] after
                    // initializing.
                    let __ref: &'static mut [$t; $n] = unsafe {
                        &mut *(__ref as *mut _ as *mut _)
                    };
                    __ref
                }
            ),*
        )
    }};
}
