// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::miim_bridge::MiimBridge;
use crate::pins;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_net_api::NetError;
use vsc7448_pac::phy;

/// Address used on the MDIO link by our Ethernet PHY. Different
/// vendors have different defaults for this, it will likely need to
/// become configurable.
const PHYADDR: u8 = 0x0;

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
    pub fn new(eth: &eth::Ethernet, _sys: &Sys) -> Self {
        let mut bridge = MiimBridge::new(eth);
        let phy = vsc85xx::Phy::new(PHYADDR, &mut bridge);

        // Set up the PHY.
        phy.modify(phy::STANDARD::MODE_CONTROL(), |r| {
            r.set_auto_neg_ena(1);
            r.set_restart_auto_neg(1);
        })
        .unwrap();

        Self {}
    }

    pub fn wake(&self, _eth: &eth::Ethernet) {
        panic!("Wake should never be called, because WAKE_INTERVAL is None");
    }

    /// Calls a function on a `Phy` associated with the given port.
    pub fn phy_fn<T, F: Fn(vsc85xx::Phy<MiimBridge>) -> T>(
        &mut self,
        port: u8,
        callback: F,
        eth: &eth::Ethernet,
    ) -> Result<T, NetError> {
        if port > 0 {
            return Err(NetError::InvalidPort);
        }
        let mut bridge = MiimBridge::new(eth);
        let phy = vsc85xx::Phy::new(PHYADDR, &mut bridge);
        Ok(callback(phy))
    }
}
