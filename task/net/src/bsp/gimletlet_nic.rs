// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::pins;
use drv_spi_api::Spi;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use ksz8463::{
    Error as KszError, Ksz8463, MIBCounter, MIBCounterValue,
    Register as KszRegister,
};
use ringbuf::*;
use task_net_api::PhyError;
use userlib::{hl::sleep_for, task_slot};
use vsc7448_pac::types::PhyRegisterAddress;

task_slot!(SPI, spi_driver);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    BspConfigured,

    KszErr { err: KszError },
    Ksz8463Status { port: u8, status: u16 },
    Ksz8463Control { port: u8, control: u16 },
    Ksz8463Counter { port: u8, counter: MIBCounterValue },
    Ksz8463MacTable(ksz8463::MacTableEntry),
    Ksz8463EmptyMacTable,
}
ringbuf!(Trace, 32, Trace::None);

// This system wants to be woken periodically to do logging
pub const WAKE_INTERVAL: Option<u64> = Some(5000);

////////////////////////////////////////////////////////////////////////////////

pub fn preinit() {
    // Nothing to do here
}

pub fn configure_ethernet_pins(sys: &Sys) {
    pins::RmiiPins {
        refclk: Port::A.pin(1),
        crs_dv: Port::A.pin(7),
        tx_en: Port::B.pin(11),
        txd0: Port::B.pin(12),
        txd1: Port::B.pin(13),
        rxd0: Port::C.pin(4),
        rxd1: Port::C.pin(5),
        af: Alternate::AF11,
    }
    .configure(sys);
}

pub struct Bsp {
    ksz8463: Ksz8463,
}

impl Bsp {
    pub fn new(_eth: &eth::Ethernet, sys: &Sys) -> Self {
        let ksz8463 = loop {
            // SPI device is based on ordering in app.toml
            let ksz8463_spi = Spi::from(SPI.get_task_id()).device(0);

            // Initialize the KSZ8463 (using SPI4_RESET, PB10)
            sys.gpio_init_reset_pulse(Port::B.pin(10), 10, 1).unwrap();
            let ksz8463 = Ksz8463::new(ksz8463_spi);
            match ksz8463
                .configure(ksz8463::Mode::Copper, ksz8463::VLanMode::Mandatory)
            {
                Err(err) => {
                    ringbuf_entry!(Trace::KszErr { err });
                    sleep_for(100);
                }
                _ => break ksz8463,
            }
        };
        ringbuf_entry!(Trace::BspConfigured);

        Self { ksz8463 }
    }

    pub fn wake(&self, _eth: &eth::Ethernet) {
        for port in [1, 2] {
            ringbuf_entry!(
                match self.ksz8463.read(KszRegister::PxMBSR(port)) {
                    Ok(status) => Trace::Ksz8463Status { port, status },
                    Err(err) => Trace::KszErr { err },
                }
            );
            ringbuf_entry!(
                match self.ksz8463.read(KszRegister::PxMBCR(port)) {
                    Ok(control) => Trace::Ksz8463Control { port, control },
                    Err(err) => Trace::KszErr { err },
                }
            );
            ringbuf_entry!(match self
                .ksz8463
                .read_mib_counter(port, MIBCounter::RxLoPriorityByte)
            {
                Ok(counter) => Trace::Ksz8463Counter { port, counter },
                Err(err) => Trace::KszErr { err },
            });

            // Read the MAC table for fun
            ringbuf_entry!(match self.ksz8463.read_dynamic_mac_table(0) {
                Ok(Some(mac)) => Trace::Ksz8463MacTable(mac),
                Ok(None) => Trace::Ksz8463EmptyMacTable,
                Err(err) => Trace::KszErr { err },
            });
        }
    }

    /// Calls a function on a `Phy` associated with the given port.
    pub fn phy_read(
        &mut self,
        _port: u8,
        _reg: PhyRegisterAddress<u16>,
        _eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        Err(PhyError::NotImplemented)
    }

    /// Calls a function on a `Phy` associated with the given port.
    pub fn phy_write(
        &mut self,
        _port: u8,
        _reg: PhyRegisterAddress<u16>,
        _value: u16,
        _eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        Err(PhyError::NotImplemented)
    }
}
