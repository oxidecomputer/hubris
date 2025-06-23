// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Cosmo rev A hardware

#[cfg(not(all(feature = "ksz8463", feature = "mgmt", feature = "vlan")))]
compile_error!("this BSP requires the ksz8463, mgmt, and vlan features");

use crate::{
    bsp_support::{self, Ksz8463},
    mgmt, notifications, pins,
};
use drv_cpu_seq_api::PowerState;
use drv_spi_api::SpiServer;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_jefe_api::Jefe;
use task_net_api::{
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError,
};
use userlib::{sys_recv_notification, FromPrimitive};
use vsc7448_pac::types::PhyRegisterAddress;

////////////////////////////////////////////////////////////////////////////////

pub struct BspImpl(mgmt::Bsp);

impl crate::bsp_support::Bsp for BspImpl {
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
        // Wait for the sequencer to turn on the clock. This requires that Jefe
        // state change notifications are routed to our notification bit 3.
        let jefe = Jefe::from(crate::JEFE.get_task_id());

        loop {
            // This laborious list is intended to ensure that new power states
            // have to be added explicitly here.
            match PowerState::from_u32(jefe.get_state()) {
                Some(PowerState::A2)
                | Some(PowerState::A2PlusFans)
                | Some(PowerState::A0)
                | Some(PowerState::A0PlusHP)
                | Some(PowerState::A0Thermtrip)
                | Some(PowerState::A0Reset) => {
                    break;
                }
                None => {
                    // This happens before we're in a valid power state.
                    //
                    // Only listen to our Jefe notification.
                    sys_recv_notification(
                        notifications::JEFE_STATE_CHANGE_MASK,
                    );
                }
            }
        }
    }

    fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        let spi = bsp_support::claim_spi(sys);
        let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
        Self(
            mgmt::Config {
                power_en: None, // power is enabled automatically
                slow_power_en: false,
                power_good: &[], // TODO

                ksz8463: Ksz8463::new(ksz8463_dev),

                // SP_TO_KSZ8463_RESET_L
                ksz8463_nrst: Port::C.pin(2),
                ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,
                ksz8463_vlan_mode: ksz8463::VLanMode::Mandatory,
                // SP_TO_VSC8562_COMA_MODE_V3P3
                vsc85x2_coma_mode: Some(Port::C.pin(3)),

                // SP_TO_VSC8562_RESET_L_V3P3
                vsc85x2_nrst: Port::A.pin(8),

                vsc85x2_base_port: 0b11110, // Based on resistor strapping
            }
            .build(sys, eth),
        )
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
