// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{mgmt, pins};
use drv_gimlet_seq_api::PowerState;
use drv_spi_api::Spi;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use task_jefe_api::Jefe;
use userlib::{sys_recv_closed, task_slot, FromPrimitive, TaskId};

task_slot!(SPI, spi_driver);
task_slot!(JEFE, jefe);

// This system wants to be woken periodically to do logging
pub const WAKE_INTERVAL: Option<u64> = Some(500);

////////////////////////////////////////////////////////////////////////////////

/// Stateless function to configure ethernet pins before the Bsp struct
/// is actually constructed
pub fn configure_ethernet_pins(sys: &Sys) {
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

pub struct Bsp(mgmt::Bsp);

pub fn preinit() {
    // Wait for the sequencer to turn on the clock. This requires that Jefe
    // state change notifications are routed to our notification bit 3.
    let jefe = Jefe::from(JEFE.get_task_id());

    loop {
        // This laborious list is intended to ensure that new power states have
        // to be added explicitly here.
        match PowerState::from_u32(jefe.get_state()) {
            Some(PowerState::A2)
            | Some(PowerState::A2PlusMono)
            | Some(PowerState::A2PlusFans)
            | Some(PowerState::A1)
            | Some(PowerState::A0) => {
                break;
            }
            None => {
                // This happens before we're in a valid power state.
                //
                // Only listen to our Jefe notification. Discard any error since
                // this can't fail but the compiler doesn't know that.
                let _ = sys_recv_closed(&mut [], 1 << 3, TaskId::KERNEL);
            }
        }
    }
}

impl Bsp {
    pub fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        let out = Self(
            mgmt::Config {
                // SP_TO_MGMT_V1P0_EN, SP_TO_MGMT_V2P5_EN
                power_en: Some(Port::I.pin(10).and_pin(12)),
                slow_power_en: false,
                power_good: None, // TODO
                pll_lock: None,   // TODO?

                // Based on ordering in app.toml
                ksz8463_spi: Spi::from(SPI.get_task_id()).device(2),

                // SP_TO_MGMT_MUX_RESET_L
                ksz8463_nrst: Port::C.pin(2),
                ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,
                ksz8463_vlan_mode: ksz8463::VLanMode::Mandatory,

                // SP_TO_MGMT_PHY_COMA_MODE
                vsc85x2_coma_mode: Some(Port::D.pin(7)),

                // SP_TO_MGMT_PHY_RESET
                vsc85x2_nrst: Port::A.pin(8),

                vsc85x2_base_port: 0b11110, // Based on resistor strapping
            }
            .build(sys, eth),
        );
        use crate::miim_bridge::MiimBridge;
        let rw = &mut MiimBridge::new(eth);
        for i in 0..2 {
            use vsc7448_pac::phy;
            let phy = &mut out.0.vsc85x2.phy(i, rw);

            // Errata: this bit must be disabled for loopback mode to work.
            phy.phy
                .modify(
                    phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER(),
                    |r| {
                        let mut v = u16::from(*r);
                        v &= !(1 << 13);
                        *r = v.into();
                    },
                )
                .unwrap();

            // Enable far-end loopback
            phy.phy
                .modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
                    let mut v = u16::from(*r);
                    v |= 1 << 3;
                    *r = v.into();
                })
                .unwrap();

            /*
            // Disable Rx to avoid DDOSing yourself
            out.0
                .ksz8463
                .modify(ksz8463::Register::PxCR2(i + 1), |r| {
                    *r &= !(1 << 9);
                })
                .unwrap();
            */
        }
        out
    }

    pub fn wake(&self, eth: &eth::Ethernet) {
        self.0.wake(eth);
    }
}
