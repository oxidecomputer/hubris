// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::miim_bridge::MiimBridge;
use crate::GPIO;

use drv_spi_api::{Spi, SpiError};
use drv_stm32h7_eth as eth;
use drv_stm32h7_gpio_api as gpio_api;
use ksz8463::{Ksz8463, MIBCounter, MIBOffset, Register as KszRegister};
use ringbuf::*;
use userlib::{hl::sleep_for, task_slot};
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc85xx::{Phy, PhyVsc85xx, VscError};

task_slot!(SPI, spi_driver);
const KSZ8463_SPI_DEVICE: u8 = 0; // Based on app.toml ordering
const VSC8552_PORT: u8 = 0b11100; // Based on resistor strapping

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Ksz8463Configured,
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
        counter: MIBCounter,
    },

    Vsc8552Configured,
    Vsc8552Status {
        port: u8,
        status: phy::standard::MODE_STATUS,
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
ringbuf!(Trace, 16, Trace::None);

// This system wants to be woken periodically to do logging
pub const WAKE_INTERVAL: Option<u64> = Some(500);

////////////////////////////////////////////////////////////////////////////////

pub struct Bsp {
    ksz: Ksz8463,
}

impl Bsp {
    pub fn new() -> Self {
        let spi = Spi::from(SPI.get_task_id()).device(KSZ8463_SPI_DEVICE);
        let ksz = Ksz8463::new(spi, gpio_api::Port::A.pin(9), true);

        Self { ksz }
    }

    pub fn configure_ethernet_pins(&self) {
        // This board's mapping:
        //
        // RMII REF CLK     PA1
        // RMII RX DV       PA7
        //
        // RMII RXD0        PC4
        // RMII RXD1        PC5
        //
        // RMII TX EN       PG11
        // RMII TXD1        PG12
        // RMII TXD0        PG13
        //
        // MDIO             PA2
        //
        // MDC              PC1
        //
        // (it's _almost_ identical to the STM32H7 Nucleo, except that
        //  TXD1 is on a different pin)
        //
        //  The MDIO/MDC lines run at Speed::Low because otherwise the VSC8504
        //  refuses to talk.
        use gpio_api::*;
        let gpio = Gpio::from(GPIO.get_task_id());
        let eth_af = Alternate::AF11;

        // RMII
        gpio.configure(
            Port::A,
            (1 << 1) | (1 << 7),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
            Port::C,
            (1 << 4) | (1 << 5),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
            Port::G,
            (1 << 11) | (1 << 12) | (1 << 13),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();

        // SMI (MDC and MDIO)
        gpio.configure(
            Port::A,
            1 << 2,
            Mode::Alternate,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
            eth_af,
        )
        .unwrap();
        gpio.configure(
            Port::C,
            1 << 1,
            Mode::Alternate,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
            eth_af,
        )
        .unwrap();
    }

    pub fn configure_phy(&self, eth: &mut eth::Ethernet) {
        // The KSZ8463 connects to the SP over RMII, then sends data to the
        // VSC8552 over 100-BASE FX
        self.ksz.configure();
        ringbuf_entry!(Trace::Ksz8463Configured);

        // The VSC8552 connects the KSZ switch to the management network
        // over SGMII
        configure_vsc8552(eth);
        ringbuf_entry!(Trace::Vsc8552Configured);
    }

    pub fn wake(&self, eth: &mut eth::Ethernet) {
        ringbuf_entry!(match self.ksz.read(KszRegister::P1MBSR) {
            Ok(status) => Trace::Ksz8463Status { port: 1, status },
            Err(err) => Trace::KszErr { err },
        });

        ringbuf_entry!(match self.ksz.read(KszRegister::P1MBCR) {
            Ok(control) => Trace::Ksz8463Control { port: 1, control },
            Err(err) => Trace::KszErr { err },
        });
        ringbuf_entry!(match self
            .ksz
            .read_mib_counter(1, MIBOffset::RxLoPriorityByte)
        {
            Ok(counter) => Trace::Ksz8463Counter { port: 1, counter },
            Err(err) => Trace::KszErr { err },
        });

        // TODO: log more for port 2?
        ringbuf_entry!(match self.ksz.read(KszRegister::P2MBSR) {
            Ok(status) => Trace::Ksz8463Status { port: 2, status },
            Err(err) => Trace::KszErr { err },
        });

        for i in [0, 1] {
            let port = VSC8552_PORT + i;
            let mut phy = Phy {
                port,
                rw: &mut MiimBridge::new(eth),
            };

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
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

// We're talking to a VSC8552, which is compatible with the VSC85xx trait.
impl PhyVsc85xx for MiimBridge<'_> {}

pub fn configure_vsc8552(eth: &mut eth::Ethernet) {
    use gpio_api::*;
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    let nrst = gpio_api::Port::A.pin(10);

    // Start with reset low
    gpio_driver.reset(nrst).unwrap();
    gpio_driver
        .configure_output(nrst, OutputType::PushPull, Speed::Low, Pull::None)
        .unwrap();
    sleep_for(4);

    gpio_driver.set(nrst).unwrap();
    sleep_for(120); // Wait for the chip to come out of reset

    // The VSC8552 patch must be applied to port 0 in the phy
    let mut phy_rw = MiimBridge::new(eth);
    let mut phy0 = Phy {
        port: VSC8552_PORT,
        rw: &mut phy_rw,
    };
    vsc85xx::patch_vsc8552_phy(&mut phy0).unwrap();

    // Port 0 on the PHY is connected to a SFF-8087 Mini-Sas
    vsc85xx::init_vsc8552_phy(&mut phy0).unwrap();

    // Port 1 on the PHY is connected to SMA connectors
    let mut phy1 = Phy {
        port: VSC8552_PORT + 1,
        rw: &mut phy_rw,
    };
    vsc85xx::init_vsc8552_phy(&mut phy1).unwrap();
}
