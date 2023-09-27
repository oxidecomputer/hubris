// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// proc macro to generate iocon function for LPC55
extern crate proc_macro;
use proc_macro::TokenStream;
extern crate proc_macro2;
use quote::quote;

#[proc_macro]
pub fn dynamic_hashcrypt(_item: TokenStream) -> TokenStream {
    let code = quote! {
        static mut USE_ROM: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

        pub fn set_hashcrypt_default() {
            unsafe {
                USE_ROM.store(false, core::sync::atomic::Ordering::Relaxed);
            }
        }

        pub fn set_hashcrypt_rom() {
            unsafe {
                USE_ROM.store(true, core::sync::atomic::Ordering::Relaxed);
            }
        }

        #[allow(non_snake_case)]
        #[no_mangle]
        pub unsafe extern "C" fn HASHCRYPT() {
            if USE_ROM.load(core::sync::atomic::Ordering::Relaxed) {
                lpc55_romapi::skboot_hashcrypt_handler();
            } else {
                kern::arch::DefaultHandler();
            }

        }

    };

    code.into()
}
