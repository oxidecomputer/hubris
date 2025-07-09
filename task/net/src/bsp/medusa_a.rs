// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Medusa model A

#[cfg(not(all(feature = "ksz8463", feature = "mgmt")))]
compile_error!("this BSP requires the ksz8463, mgmt, and vlan features");

use crate::{
    bsp_support::{self, Ksz8463},
    mgmt,
    miim_bridge::MiimBridge,
    pins,
};
use drv_spi_api::SpiServer;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_net_api::{
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError,
};
use userlib::UnwrapLite;
use vsc7448_pac::types::PhyRegisterAddress;

////////////////////////////////////////////////////////////////////////////////

pub struct BspImpl(mgmt::Bsp);

impl bsp_support::Bsp for BspImpl {
    // This system wants to be woken periodically to do logging
    const WAKE_INTERVAL: Option<u64> = Some(500);

    /// Stateless function to configure ethernet pins before the Bsp struct
    /// is actually constructed
    fn configure_ethernet_pins(sys: &Sys) {
        pins::RmiiPins {
            refclk: Port::A.pin(1), // CLK_50M_SP_RMII_REFCLK
            crs_dv: Port::A.pin(7), // RMII_SP_TO_EPE_RX_DV
            tx_en: Port::G.pin(11), // RMII_SP_TO_EPE_TX_EN
            txd0: Port::G.pin(13),  // RMII_SP_TO_EPE_TXD0
            txd1: Port::G.pin(12),  // RMII_SP_TO_EPE_TXD1
            rxd0: Port::C.pin(4),   // RMII_SP_TO_EPE_RDX0 (typo in schematic)
            rxd1: Port::C.pin(5),   // RMII_SP_TO_EPE_RXD1
            af: Alternate::AF11,
        }
        .configure(sys);

        pins::MdioPins {
            mdio: Port::A.pin(2), // MIIM_SP_TO_PHY_MDIO_3V3
            mdc: Port::C.pin(1),  // MIIM_SP_TO_PHY_MDC_3V3
            af: Alternate::AF11,
        }
        .configure(sys);
    }

    fn preinit() {
        // TODO
    }

    fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        let spi = bsp_support::claim_spi(sys);
        let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
        let bsp = mgmt::Config {
            // SP_TO_LDO_PHY2_EN on pin I.13 (turns on both P2V5 and P1V0) turns on automatically
            // once V3P3_SYS goes high. We leave manual control of the pin to the sequencer.
            power_en: None,
            slow_power_en: false,
            power_good: &[], // TODO

            ksz8463: Ksz8463::new(ksz8463_dev),
            // SP_TO_EPE_RESET_L
            ksz8463_nrst: Port::A.pin(0),
            ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,
            ksz8463_vlan_mode: ksz8463::VLanMode::Off,
            // SP_TO_PHY_A_COMA_MODE_3V3
            vsc85x2_coma_mode: Some(Port::I.pin(15)),
            // SP_TO_PHY_A_RESET_3V3_L
            vsc85x2_nrst: Port::I.pin(14),
            vsc85x2_base_port: 0,
        }
        .build(sys, eth);

        Self(bsp)
    }

    fn wake(&self, eth: &eth::Ethernet) {
        self.0.wake(eth);
    }

    fn phy_read(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        self.0.phy_read(port, reg, eth)
    }

    fn phy_write(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        value: u16,
        eth: &eth::Ethernet,
    ) -> Result<(), PhyError> {
        self.0.phy_write(port, reg, value, eth)
    }

    fn ksz8463(&self) -> &Ksz8463 {
        &self.0.ksz8463
    }

    fn management_link_status(
        &self,
        eth: &eth::Ethernet,
    ) -> Result<ManagementLinkStatus, MgmtError> {
        self.0.management_link_status(eth)
    }

    fn management_counters(
        &self,
        eth: &crate::eth::Ethernet,
    ) -> Result<ManagementCounters, MgmtError> {
        self.0.management_counters(eth)
    }
}
