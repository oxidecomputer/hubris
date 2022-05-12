// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{mgmt, pins};
use drv_gimlet_seq_api::Sequencer;
use drv_spi_api::Spi;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use userlib::{hl::sleep_for, task_slot};

task_slot!(SPI, spi_driver);
task_slot!(SEQ, seq);

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
    // Wait for the sequencer to turn on the clock
    let seq = Sequencer::from(SEQ.get_task_id());
    while seq.is_clock_config_loaded().unwrap_or(0) == 0 {
        sleep_for(10);
    }
}

impl Bsp {
    pub fn new(eth: &eth::Ethernet, sys: &Sys) -> Self {
        Self(
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
        )
    }

    pub fn wake(&self, eth: &eth::Ethernet) {
        self.0.wake(eth);
    }
}
