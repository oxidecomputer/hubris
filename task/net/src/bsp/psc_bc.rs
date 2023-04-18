// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[cfg(not(all(feature = "ksz8463", feature = "mgmt")))]
compile_error!("this BSP requires the ksz8463 and mgmt features");

use crate::{
    bsp_support::{self, Ksz8463},
    mgmt, notifications, pins,
};
use drv_psc_seq_api::PowerState;
use drv_spi_api::SpiServer;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_jefe_api::Jefe;
use task_net_api::{
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError,
};
use userlib::{sys_recv_closed, task_slot, FromPrimitive, TaskId};
use vsc7448_pac::types::PhyRegisterAddress;

task_slot!(JEFE, jefe);

////////////////////////////////////////////////////////////////////////////////

pub struct BspImpl(mgmt::Bsp);

const PG_PINS: [drv_stm32xx_sys_api::PinSet; 2] = [
    Port::C.pin(11), // V1P0_MGMT_PHY_A2_DC_DC_PG
    Port::C.pin(12), // V2P5_MGMT_PHY_A2_LDO_PG
];

impl bsp_support::Bsp for BspImpl {
    // This system wants to be woken periodically to do logging
    const WAKE_INTERVAL: Option<u64> = Some(500);

    /// Stateless function to configure ethernet pins before the Bsp struct
    /// is actually constructed
    fn configure_ethernet_pins(sys: &Sys) {
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

    fn preinit() {
        // Wait for the sequencer to turn read our VPD.
        let jefe = Jefe::from(JEFE.get_task_id());

        loop {
            // This laborious list is intended to ensure that new power states
            // have to be added explicitly here.
            match PowerState::from_u32(jefe.get_state()) {
                Some(PowerState::A2) => {
                    break;
                }
                Some(PowerState::Init) | None => {
                    // This happens before we're in a valid power state.
                    //
                    // Only listen to our Jefe notification. Discard any error
                    // since this can't fail but the compiler doesn't know that.
                    let _ = sys_recv_closed(
                        &mut [],
                        notifications::JEFE_STATE_CHANGE_MASK,
                        TaskId::KERNEL,
                    );
                }
            }
        }
    }

    fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        let spi = bsp_support::claim_spi(sys);
        let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
        let bsp = mgmt::Config {
            // SP_TO_MGMT_PHY_A2_PWR_EN
            power_en: Some(Port::I.pin(10)),
            slow_power_en: false,
            power_good: &PG_PINS,
            pll_lock: None,

            ksz8463: Ksz8463::new(ksz8463_dev),
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
