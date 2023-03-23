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

impl bsp_support::Bsp for BspImpl {
    // This system wants to be woken periodically to do logging
    const WAKE_INTERVAL: Option<u64> = Some(500);

    /// Stateless function to configure ethernet pins before the Bsp struct
    /// is actually constructed
    fn configure_ethernet_pins(sys: &Sys) {
        pins::RmiiPins {
            refclk: Port::A.pin(1),
            crs_dv: Port::A.pin(7),
            tx_en: Port::G.pin(11),
            txd0: Port::G.pin(13),
            txd1: Port::G.pin(12),
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
            // SP_TO_MGMT_V1P0_EN / SP_TO_MGMT_V2P5_EN
            // (note that the latter also enables the MGMT_PHY_REFCLK)
            power_en: Some(Port::I.pin(10).and_pin(12)),
            slow_power_en: true,
            power_good: None, // TODO
            pll_lock: None,

            ksz8463: Ksz8463::new(ksz8463_dev),
            ksz8463_nrst: Port::C.pin(2),
            ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,

            #[cfg(feature = "vlan")]
            ksz8463_vlan_mode: ksz8463::VLanMode::Mandatory,
            #[cfg(not(feature = "vlan"))]
            ksz8463_vlan_mode: ksz8463::VLanMode::Optional,

            // SP_TO_MGMT_PHY_COMA_MODE
            vsc85x2_coma_mode: Some(Port::D.pin(7)),

            // SP_TO_MGMT_PHY_RESET
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
