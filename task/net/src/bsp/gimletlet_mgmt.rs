// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{mgmt, miim_bridge::MiimBridge, pins};
use drv_spi_api::{Spi, SpiError};
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use drv_user_leds_api::UserLeds;
use ksz8463::{MIBCounter, MIBCounterValue, Register as KszRegister};
use ringbuf::*;
use userlib::task_slot;
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc85xx::VscError;

task_slot!(SPI, spi_driver);
task_slot!(USER_LEDS, user_leds);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    BspConfigured,

    KszErr {
        err: SpiError,
    },
    Ksz8463Status {
        port: u8,
        status: u16,
    },
    Ksz8463Control {
        port: u8,
        control: u16,
    },
    Ksz8463Counter {
        port: u8,
        counter: MIBCounterValue,
    },
    Ksz8463MacTable(ksz8463::MacTableEntry),

    Vsc8552Status {
        port: u8,
        status: phy::standard::MODE_STATUS,
    },
    Vsc8552MacPcsStatus {
        port: u8,
        status: phy::extended_3::MAC_SERDES_PCS_STATUS,
    },
    Vsc8552MacPcsControl {
        port: u8,
        control: phy::extended_3::MAC_SERDES_PCS_CONTROL,
    },
    Vsc8552MediaSerdesStatus {
        port: u8,
        status: phy::extended_3::MEDIA_SERDES_STATUS,
    },
    Vsc8552Err {
        err: VscError,
    },
    Vsc8552BypassControl {
        port: u8,
        control: phy::standard::BYPASS_CONTROL,
    },
    Vsc8552Status100 {
        port: u8,
        status: u16,
    },
    Vsc8552TxGoodCounter {
        port: u8,
        counter: phy::extended_3::MEDIA_SERDES_TX_GOOD_PACKET_COUNTER,
    },
    Vsc8552RxCRCGoodCounter {
        port: u8,
        counter: phy::extended_3::MEDIA_MAC_SERDES_RX_GOOD_COUNTER,
    },
}
ringbuf!(Trace, 32, Trace::None);

// This system wants to be woken periodically to do logging
pub const WAKE_INTERVAL: Option<u64> = Some(500);

////////////////////////////////////////////////////////////////////////////////

pub fn preinit() {
    // Nothing to do here
}

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

pub struct Bsp {
    mgmt: mgmt::Bsp,
    leds: UserLeds,
}

impl Bsp {
    pub fn new(eth: &mut eth::Ethernet, sys: &Sys) -> Self {
        let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());

        // Turn on an LED to indicate that we're configuring
        leds.led_off(0).unwrap();
        leds.led_on(3).unwrap();

        let mgmt = mgmt::Config {
            power_en: None,
            slow_power_en: false,
            power_good: None,
            pll_lock: None,

            // SPI device is based on ordering in app.toml
            ksz8463_spi: Spi::from(SPI.get_task_id()).device(0),
            ksz8463_nrst: Port::A.pin(9),
            ksz8463_rst_type: mgmt::Ksz8463ResetSpeed::Slow,

            vsc85x2_coma_mode: None,
            vsc85x2_nrst: Port::A.pin(10),
            vsc85x2_base_port: 0b11100, // Based on resistor strapping
        }
        .build(sys, eth);
        ringbuf_entry!(Trace::BspConfigured);

        leds.led_on(0).unwrap();
        leds.led_off(3).unwrap();

        Self { mgmt, leds }
    }

    pub fn wake(&self, eth: &mut eth::Ethernet) {
        // Run the BSP wake function, which logs summarized data to a different
        // ringbuf; we'll still do verbose logging of full registers below.
        self.mgmt.wake(eth);

        for port in [1, 2] {
            ringbuf_entry!(match self
                .mgmt
                .ksz8463
                .read(KszRegister::PxMBSR(port))
            {
                Ok(status) => Trace::Ksz8463Status { port, status },
                Err(err) => Trace::KszErr { err },
            });
            ringbuf_entry!(match self
                .mgmt
                .ksz8463
                .read(KszRegister::PxMBCR(port))
            {
                Ok(control) => Trace::Ksz8463Control { port, control },
                Err(err) => Trace::KszErr { err },
            });
            ringbuf_entry!(match self
                .mgmt
                .ksz8463
                .read_mib_counter(port, MIBCounter::RxLoPriorityByte)
            {
                Ok(counter) => Trace::Ksz8463Counter { port, counter },
                Err(err) => Trace::KszErr { err },
            });
        }

        // Read the MAC table for fun
        ringbuf_entry!(match self.mgmt.ksz8463.read_dynamic_mac_table(0) {
            Ok(mac) => Trace::Ksz8463MacTable(mac),
            Err(err) => Trace::KszErr { err },
        });

        let mut any_comma = false;
        let mut any_link = false;
        let rw = &mut MiimBridge::new(eth);
        for i in [0, 1] {
            let mut phy = self.mgmt.vsc85x2.phy(i, rw).phy;
            let port = phy.port;

            ringbuf_entry!(match phy.read(phy::STANDARD::MODE_STATUS()) {
                Ok(status) => Trace::Vsc8552Status { port, status },
                Err(err) => Trace::Vsc8552Err { err },
            });

            // This is a non-standard register address
            let extended_status =
                PhyRegisterAddress::<u16>::from_page_and_addr_unchecked(0, 16);
            ringbuf_entry!(match phy.read(extended_status) {
                Ok(status) => Trace::Vsc8552Status100 { port, status },
                Err(err) => Trace::Vsc8552Err { err },
            });

            ringbuf_entry!(match phy.read(phy::STANDARD::BYPASS_CONTROL()) {
                Ok(control) => Trace::Vsc8552BypassControl { port, control },
                Err(err) => Trace::Vsc8552Err { err },
            });

            ringbuf_entry!(match phy
                .read(phy::EXTENDED_3::MEDIA_SERDES_TX_GOOD_PACKET_COUNTER())
            {
                Ok(counter) => Trace::Vsc8552TxGoodCounter { port, counter },
                Err(err) => Trace::Vsc8552Err { err },
            });
            ringbuf_entry!(match phy
                .read(phy::EXTENDED_3::MEDIA_MAC_SERDES_RX_GOOD_COUNTER())
            {
                Ok(counter) => Trace::Vsc8552RxCRCGoodCounter { port, counter },
                Err(err) => Trace::Vsc8552Err { err },
            });
            ringbuf_entry!(match phy
                .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())
            {
                Ok(status) => {
                    any_link |= (status.0 & (1 << 2)) != 0;
                    any_comma |= (status.0 & (1 << 0)) != 0;
                    Trace::Vsc8552MacPcsStatus { port, status }
                }
                Err(err) => Trace::Vsc8552Err { err },
            });
            ringbuf_entry!(match phy
                .read(phy::EXTENDED_3::MEDIA_SERDES_STATUS())
            {
                Ok(status) => Trace::Vsc8552MediaSerdesStatus { port, status },
                Err(err) => Trace::Vsc8552Err { err },
            });
            ringbuf_entry!(match phy
                .read(phy::EXTENDED_3::MAC_SERDES_PCS_CONTROL())
            {
                Ok(control) => {
                    Trace::Vsc8552MacPcsControl { port, control }
                }
                Err(err) => Trace::Vsc8552Err { err },
            });
        }

        if any_link {
            self.leds.led_on(1).unwrap();
        } else {
            self.leds.led_off(1).unwrap();
        }
        if any_comma {
            self.leds.led_on(2).unwrap();
        } else {
            self.leds.led_off(2).unwrap();
        }
    }
}
