// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// proc macro to generate iocon function for LPC55
extern crate proc_macro;
use proc_macro::TokenStream;
extern crate proc_macro2;
use proc_macro2::TokenStream as QuoteStream;
use quote::{format_ident, quote};

///
/// The way the SVD is written results in a separate named function per
/// IOCON pin. This is great if you have a single board file and never change
/// anything but doesn't match up with how we're modeling things with Hubris.
///
/// This proc macro generates the following function
///
/// ```ignore
/// fn set_iocon(pin: Pin, alt: AltFn, mode: Mode,
///              slew: Slew, invert: Invert, digimode: Digimode,
///              od : Opendrain)
/// ```
///
/// Which ends up being a gigantic switch function to call the right port and
/// pin function.
///
#[proc_macro]
pub fn gen_iocon_table(_item: TokenStream) -> TokenStream {
    let iocon_bits = quote! {
        write(|w|  unsafe { w.func().bits(alt as u8).
                            mode().bits(mode as u8).
                            slew().bit(slew.into()).
                            invert().bit(invert.into()).
                            digimode().bit(digimode.into()).
                            od().bit(od.into()) }),
    };

    // It's easier to keep everything as something compatible with quote
    // and only do the final conversion at the end
    let mut running: Vec<QuoteStream> = vec![];

    // Would love a way to avoid having this here eventually
    cfg_if::cfg_if! {
        if #[cfg(any(target_board = "lpcxpresso55s69"))] {
            let max_pins = 64;
        } else {
            let max_pins = 36;
        }
    }

    for pin in 0..max_pins {
        let pname =
            format_ident!("PIO{}_{}", (pin / 32) as usize, (pin % 32) as usize);
        let full_port =
            format_ident!("pio{}_{}", (pin / 32) as usize, (pin % 32) as usize);

        let combined = quote! { drv_lpc55_gpio_api::Pin::#pname => iocon.#full_port.#iocon_bits };

        running.push(combined.into());
    }

    let table = running.into_iter().collect::<QuoteStream>();

    let last = quote! {
        fn set_iocon(pin : drv_lpc55_gpio_api::Pin,
                    alt : drv_lpc55_gpio_api::AltFn,
                    mode : drv_lpc55_gpio_api::Mode,
                    slew : drv_lpc55_gpio_api::Slew,
                    invert : drv_lpc55_gpio_api::Invert,
                    digimode : drv_lpc55_gpio_api::Digimode,
                    od : drv_lpc55_gpio_api::Opendrain) {

            let iocon = unsafe { &*lpc55_pac::IOCON::ptr() };

            match pin {
                #table
            }

        }

    };

    last.into()
}
