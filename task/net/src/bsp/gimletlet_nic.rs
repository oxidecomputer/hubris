// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[cfg(not(feature = "ksz8463"))]
compile_error!("this BSP requires the ksz8463 feature");

use crate::{
    bsp_support::{self, Ksz8463},
    pins,
};
use drv_spi_api::SpiServer;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{Alternate, Port, Sys};
use ksz8463::{
    Error as RawKszError, MIBCounter, MIBCounterValue, Register as KszRegister,
};
use ringbuf::*;
use task_net_api::PhyError;
use userlib::hl::sleep_for;
use vsc7448_pac::types::PhyRegisterAddress;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    BspConfigured,

    KszErr { err: RawKszError },
    Ksz8463Status { port: u8, status: u16 },
    Ksz8463Control { port: u8, control: u16 },
    Ksz8463Counter { port: u8, counter: MIBCounterValue },
}
ringbuf!(Trace, 32, Trace::None);

////////////////////////////////////////////////////////////////////////////////

pub struct BspImpl {
    ksz8463: Ksz8463,
}

impl bsp_support::Bsp for BspImpl {
    // This system wants to be woken periodically to do logging
    const WAKE_INTERVAL: Option<u64> = Some(5000);

    fn configure_ethernet_pins(sys: &Sys) {
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

    fn new(_eth: &eth::Ethernet, sys: &Sys) -> Self {
        let ksz8463 = loop {
            let spi = bsp_support::claim_spi(sys);
            // SPI4_HEADER is shared by both the SPI4 header and the NIC
            let ksz8463_spi = spi.device(drv_spi_api::devices::SPI4_HEADER);

            // Initialize the KSZ8463 (using SPI4_RESET, PB10)
            sys.gpio_init_reset_pulse(Port::B.pin(10), 10, 1);
            let ksz8463 = Ksz8463::new(ksz8463_spi);

            #[cfg(feature = "vlan")]
            let vlan_mode = ksz8463::VLanMode::Mandatory;
            #[cfg(not(feature = "vlan"))]
            let vlan_mode = ksz8463::VLanMode::Optional;

            match ksz8463.configure(ksz8463::Mode::Copper, vlan_mode) {
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

    fn wake(&self, _eth: &eth::Ethernet) {
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
        }
    }

    /// Calls a function on a `Phy` associated with the given port.
    fn phy_read(
        &mut self,
        _port: u8,
        _reg: PhyRegisterAddress<u16>,
        _eth: &eth::Ethernet,
    ) -> Result<u16, PhyError> {
        Err(PhyError::NotImplemented)
    }

    /// Calls a function on a `Phy` associated with the given port.
    fn phy_write(
        &mut self,
        _port: u8,
        _reg: PhyRegisterAddress<u16>,
        _value: u16,
        _eth: &eth::Ethernet,
    ) -> Result<(), PhyError> {
        Err(PhyError::NotImplemented)
    }

    fn ksz8463(&self) -> &Ksz8463 {
        &self.ksz8463
    }
}
