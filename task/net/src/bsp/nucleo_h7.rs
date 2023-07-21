// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::miim_bridge::MiimBridge;
use crate::pins;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_net_api::PhyError;
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc85xx::PhyRw;

/// Address used on the MDIO link by our Ethernet PHY. Different
/// vendors have different defaults for this, it will likely need to
/// become configurable.
const PHYADDR: u8 = 0x0;

userlib::task_slot!(USER_LEDS, user_leds);

// Empty handle
pub struct BspImpl;

impl crate::bsp_support::Bsp for BspImpl {
    const WAKE_INTERVAL: Option<u64> = Some(1000);

    fn preinit() {}

    fn configure_ethernet_pins(sys: &Sys) {
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

    fn new(eth: &eth::Ethernet, _sys: &Sys) -> Self {
        // Unlike most Microchip PHYs, the LAN8742A-CZ-TR does not use register
        // 31 to switch between register pages (since it only uses page 0).
        // This means that we use the _raw read and write functions, since the
        // higher-level funtions assume page switching.
        let phy = MiimBridge::new(eth);
        let mut r = phy::standard::MODE_CONTROL::from(
            phy.read_raw(PHYADDR, phy::STANDARD::MODE_CONTROL().addr)
                .unwrap(),
        );
        r.set_auto_neg_ena(1);
        r.set_restart_auto_neg(1);
        phy.write_raw(PHYADDR, phy::STANDARD::MODE_CONTROL().addr, r.into())
            .unwrap();

        Self {}
    }

    fn phy_read(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        if port != 0 {
            return Err(PhyError::InvalidPort);
        }
        let phy = MiimBridge::new(eth);
        let out = phy
            .read_raw(PHYADDR, reg.addr)
            .map_err(|_| PhyError::Other)?;
        Ok(out)
    }

    fn phy_write(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        value: u16,
        eth: &eth::Ethernet,
    ) -> Result<(), PhyError> {
        if port != 0 {
            return Err(PhyError::InvalidPort);
        }
        let phy = MiimBridge::new(eth);
        phy.write_raw(PHYADDR, reg.addr, value)
            .map_err(|_| PhyError::Other)?;
        Ok(())
    }

    fn wake(&self, eth: &eth::Ethernet) {
        if eth.rx_is_stopped() {
            let user_leds =
                drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
            user_leds.led_on(0).unwrap();
        }
    }
}
