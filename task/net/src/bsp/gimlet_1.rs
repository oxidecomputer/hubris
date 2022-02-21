// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{mgmt, miim_bridge::MiimBridge, pins};
use drv_spi_api::Spi;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use ksz8463::Register as KszRegister;
use ringbuf::*;
use userlib::task_slot;
use vsc7448_pac::phy;
use vsc85xx::VscError;

task_slot!(SPI, spi_driver);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Ksz8463Status { port: u8, status: u16 },
    Vsc8552Status { port: u8, status: u16 },
    Vsc8552Err { err: VscError },
}
ringbuf!(Trace, 16, Trace::None);

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

impl Bsp {
    pub fn new(eth: &mut eth::Ethernet, sys: &Sys) -> Self {
        Self(
            mgmt::Config {
                // SP_TO_MGMT_V2P5_EN
                power_en: Some(Port::I.pin(12)),
                power_good: None,
                pll_lock: None,

                // Based on ordering in app.toml
                ksz8463_spi: Spi::from(SPI.get_task_id()).device(2),
                ksz8463_nrst: Port::A.pin(0),
                ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Normal,

                // SP_TO_MGMT_PHY_COMA_MODE
                vsc85x2_coma_mode: Some(Port::D.pin(7)),

                // SP_TO_MGMT_PHY_RESET
                vsc85x2_nrst: Port::A.pin(8),

                vsc85x2_base_port: 0b11110, // Based on resistor strapping
            }
            .build(sys, eth),
        )
    }

    pub fn wake(&self, eth: &mut eth::Ethernet) {
        let p1_sr = self.0.ksz8463.read(KszRegister::P1MBSR).unwrap();
        ringbuf_entry!(Trace::Ksz8463Status {
            port: 1,
            status: p1_sr
        });

        let p2_sr = self.0.ksz8463.read(KszRegister::P2MBSR).unwrap();
        ringbuf_entry!(Trace::Ksz8463Status {
            port: 2,
            status: p2_sr
        });

        let rw = &mut MiimBridge::new(eth);
        for port in [0, 1] {
            match self
                .0
                .vsc85x2
                .phy(port, rw)
                .read(phy::STANDARD::MODE_STATUS())
            {
                Ok(status) => {
                    ringbuf_entry!(Trace::Vsc8552Status {
                        port,
                        status: u16::from(status)
                    })
                }
                Err(err) => ringbuf_entry!(Trace::Vsc8552Err { err }),
            }
        }
    }
}
