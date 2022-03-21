// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32h7_eth as eth;

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{RX_RING_SZ, TX_RING_SZ};

/// A macro to facilitate declaring mutable statics safely using the
/// "first-mover" pattern.
///
/// Any given use of this macro can only execute once. If execution reaches it
/// again, it will panic. It uses this behavior to ensure that it can hand out
/// &mut references to statics, which are declared within the macro.
///
/// The macro accepts definitions of one or more mutable static arrays. It will
/// arrange for them to be initialized, and return a tuple containing mutable
/// references to each, in the order they're declared.
macro_rules! mutable_statics {
    (
        $(
            $(#[$attr:meta])*
            static mut $name:ident: [$t:ty; $n:expr] = [$init:expr; _];
        )*
    ) => {{
        static TAKEN: AtomicBool = AtomicBool::new(false);
        if TAKEN.swap(true, Ordering::Relaxed) {
            panic!()
        }
        (
            $(
                {
                    $(#[$attr])*
                    static mut $name: MaybeUninit<[$t; $n]> =
                        MaybeUninit::uninit();
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
                    let __ref: &'static mut [MaybeUninit<$t>; $n] = unsafe {
                        &mut *(__ref as *mut _ as *mut _)
                    };
                    for __u in __ref.iter_mut() {
                        *__u = MaybeUninit::new($init);
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

/// Grabs references to the static descriptor/buffer transmit rings. Can only be
/// called once.
pub fn claim_tx_statics() -> (
    &'static mut [eth::ring::TxDesc; TX_RING_SZ],
    &'static mut [eth::ring::Buffer; TX_RING_SZ],
) {
    mutable_statics! {
        #[link_section = ".eth_bulk"]
        static mut TX_DESC: [eth::ring::TxDesc; TX_RING_SZ] =
            [eth::ring::TxDesc::new(); _];
        #[link_section = ".eth_bulk"]
        static mut TX_BUF: [eth::ring::Buffer; TX_RING_SZ] =
            [eth::ring::Buffer::new(); _];
    }
}
/// Grabs references to the static descriptor/buffer receive rings. Can only be
/// called once.
pub fn claim_rx_statics() -> (
    &'static mut [eth::ring::RxDesc; RX_RING_SZ],
    &'static mut [eth::ring::Buffer; RX_RING_SZ],
) {
    mutable_statics! {
        #[link_section = ".eth_bulk"]
        static mut RX_DESC: [eth::ring::RxDesc; RX_RING_SZ] =
            [eth::ring::RxDesc::new(); _];
        #[link_section = ".eth_bulk"]
        static mut RX_BUF: [eth::ring::Buffer; RX_RING_SZ] =
            [eth::ring::Buffer::new(); _];
    }
}
/// Grabs references to the MAC address buffer.  Can only be called once.
pub fn claim_mac_address() -> &'static mut [u8; 6] {
    mutable_statics! {
        static mut MAC_ADDRESS: [u8; 6] = [0; _];
    }
}
