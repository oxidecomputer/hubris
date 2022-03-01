// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::pins;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};

/// Address used on the MDIO link by our Ethernet PHY. Different
/// vendors have different defaults for this, it will likely need to
/// become configurable.
const PHYADDR: u8 = 0x01;

// The Nucleo dev board doesn't do any periodic logging
pub const WAKE_INTERVAL: Option<u64> = None;

pub fn configure_ethernet_pins(sys: &Sys) {
    pins::RmiiPins {
        refclk: Port::A.pin(1),
        crs_dv: Port::A.pin(7),
        tx_en: Port::G.pin(11),
        txd0: Port::G.pin(13),
        txd1: Port::B.pin(13),
        rxd0: Port::C.pin(4),
        rxd1: Port::C.pin(5),
        af: Alternate::AF11,
    }
    .configure(sys);

    pins::MdioPins {
        mdio: Port::A.pin(2),
        mdc: Port::C.pin(1),
        af: Alternate::AF11,
    }
    .configure(sys);
}

pub fn preinit() {
    // Nothing to do here
}

// Empty handle
pub struct Bsp;
impl Bsp {
    pub fn new(eth: &mut eth::Ethernet, _sys: &Sys) -> Self {
        // Set up the PHY.
        let mii_basic_control =
            eth.smi_read(PHYADDR, eth::SmiClause22Register::Control);
        let mii_basic_control = mii_basic_control
        | 1 << 12 // AN enable
        | 1 << 9 // restart autoneg
        ;
        eth.smi_write(
            PHYADDR,
            eth::SmiClause22Register::Control,
            mii_basic_control,
        );

        // Wait for link-up
        while eth.smi_read(PHYADDR, eth::SmiClause22Register::Status) & (1 << 2)
            == 0
        {
            userlib::hl::sleep_for(1);
        }

        Self {}
    }

    pub fn wake(&self, _eth: &mut eth::Ethernet) {
        panic!("Wake should never be called, because WAKE_INTERVAL is None");
    }
}
