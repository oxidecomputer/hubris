// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32h7_eth as eth;

use crate::{RX_RING_SZ, TX_RING_SZ};
use mutable_statics::mutable_statics;

/// Grabs references to the static descriptor/buffer transmit rings. Can only be
/// called once.
pub fn claim_tx_statics() -> (
    &'static mut [eth::ring::TxDesc; TX_RING_SZ],
    &'static mut [eth::ring::Buffer; TX_RING_SZ],
) {
    mutable_statics! {
        #[link_section = ".eth_desc"]
        static mut TX_DESC: [eth::ring::TxDesc; TX_RING_SZ] =
            [eth::ring::TxDesc::new; _];
        #[link_section = ".eth_bulk"]
        static mut TX_BUF: [eth::ring::Buffer; TX_RING_SZ] =
            [eth::ring::Buffer::new; _];
    }
}
/// Grabs references to the static descriptor/buffer receive rings. Can only be
/// called once.
pub fn claim_rx_statics() -> (
    &'static mut [eth::ring::RxDesc; RX_RING_SZ],
    &'static mut [eth::ring::Buffer; RX_RING_SZ],
) {
    mutable_statics! {
        #[link_section = ".eth_desc"]
        static mut RX_DESC: [eth::ring::RxDesc; RX_RING_SZ] =
            [eth::ring::RxDesc::new; _];
        #[link_section = ".eth_bulk"]
        static mut RX_BUF: [eth::ring::Buffer; RX_RING_SZ] =
            [eth::ring::Buffer::new; _];
    }
}
