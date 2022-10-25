// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{mgmt, pins};
use drv_spi_api::Spi;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_net_api::{
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError,
};
use userlib::task_slot;
use vsc7448_pac::types::PhyRegisterAddress;

task_slot!(SPI, spi_driver);

// This system wants to be woken periodically to do logging
pub const WAKE_INTERVAL: Option<u64> = Some(500);

////////////////////////////////////////////////////////////////////////////////

/// Stateless function to configure ethernet pins before the Bsp struct
/// is actually constructed
pub fn configure_ethernet_pins(sys: &Sys) {
    pins::RmiiPins {
        refclk: Port::A.pin(1), // CLK_50MHZ_SP_RMII_REFCLK
        crs_dv: Port::A.pin(7), // RMII_MGMT_SW_TO_SP_CRS_DV
        tx_en: Port::G.pin(11), // RMII_SP_TO_MGMT_SW_TX_EN
        txd0: Port::G.pin(13),  // RMII_SP_TO_MGMT_SW_TXD0
        txd1: Port::G.pin(12),  // RMII_SP_TO_MGMT_SW_TXD1
        rxd0: Port::C.pin(4),   // RMII_MGMT_SW_TO_SP_RXD0
        rxd1: Port::C.pin(5),   // RMII_MGMT_SW_TO_SP_RXD1
        af: Alternate::AF11,
    }
    .configure(sys);

    pins::MdioPins {
        mdio: Port::A.pin(2), // SP_TO_MGMT_PHY_MDIO_SP_DOMAIN
        mdc: Port::C.pin(1),  // SP_TO_MGMT_PHY_MDC_SP_DOMAIN
        af: Alternate::AF11,
    }
    .configure(sys);
}

pub struct Bsp(mgmt::Bsp);

pub fn preinit() {
    // Nothing to do here
}

impl Bsp {
    pub fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        let bsp = mgmt::Config {
            // SP_TO_MGMT_PHY_A2_PWR_EN
            power_en: Some(Port::I.pin(10)),
            slow_power_en: false,
            power_good: None, // TODO
            pll_lock: None,

            // Based on ordering in app.toml
            ksz8463_spi: Spi::from(SPI.get_task_id()).device(0),
            ksz8463_nrst: Port::C.pin(2), // SP_TO_MGMT_SW_RESET_L
            ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,

            #[cfg(feature = "vlan")]
            ksz8463_vlan_mode: ksz8463::VLanMode::Mandatory,
            #[cfg(not(feature = "vlan"))]
            ksz8463_vlan_mode: ksz8463::VLanMode::Optional,

            // SP_TO_MGMT_PHY_COMA_MODE_SP_DOMAIN
            vsc85x2_coma_mode: Some(Port::D.pin(7)),

            // SP_TO_MGMT_PHY_RESET_L
            vsc85x2_nrst: Port::A.pin(8),

            vsc85x2_base_port: 0b11110, // Based on resistor strapping
        }
        .build(sys, eth);

        Self(bsp)
    }

    pub fn wake(&self, eth: &eth::Ethernet) {
        self.0.wake(eth);
    }

    pub fn phy_read(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        self.0.phy_read(port, reg, eth)
    }

    pub fn phy_write(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        value: u16,
        eth: &eth::Ethernet,
    ) -> Result<(), PhyError> {
        self.0.phy_write(port, reg, value, eth)
    }

    pub fn ksz8463(&self) -> &ksz8463::Ksz8463 {
        &self.0.ksz8463
    }

    pub fn management_link_status(
        &self,
        eth: &eth::Ethernet,
    ) -> Result<ManagementLinkStatus, MgmtError> {
        self.0.management_link_status(eth)
    }

    pub fn management_counters(
        &self,
        eth: &crate::eth::Ethernet,
    ) -> Result<ManagementCounters, MgmtError> {
        self.0.management_counters(eth)
    }
}
