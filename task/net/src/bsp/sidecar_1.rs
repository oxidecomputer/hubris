// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_spi_api::{Spi, SpiDevice, SpiError};
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use ksz8463::{Ksz8463, Register as KszRegister};
use ringbuf::*;
use userlib::{hl::sleep_for, task_slot};
use vsc7448_pac::types::PhyRegisterAddress;
use vsc85xx::{Phy, PhyRw, PhyVsc85xx, VscError};

task_slot!(SPI, spi_driver);
const KSZ8463_SPI_DEVICE: u8 = 0; // Based on app.toml ordering

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Ksz8463Status { port: u8, status: u16 },
    Vsc8552Status { port: u8, status: u16 },
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

pub struct Bsp {
    ksz: Ksz8463,
}

impl Bsp {
    pub fn new() -> Self {
        let spi = Spi::from(SPI.get_task_id()).device(KSZ8463_SPI_DEVICE);
        let ksz = Ksz8463::new(spi, gpio_api::Port::A.pin(0), false);

        Self { ksz }
    }

    pub fn configure_ethernet_pins(&self, sys: &Sys) {
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
        use sys_api::*;

        let eth_af = Alternate::AF11;

        sys.gpio_configure(
            Port::A,
            (1 << 1) | (1 << 7),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        sys.gpio_configure(
            Port::C,
            (1 << 4) | (1 << 5),
            Mode::Alternate,
            OutputType::PushPull,
            Speed::VeryHigh,
            Pull::None,
            eth_af,
        )
        .unwrap();
        sys.gpio_configure(
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
        sys.gpio_configure(
            Port::A,
            1 << 2,
            Mode::Alternate,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
            eth_af,
        )
        .unwrap();
        sys.gpio_configure(
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

<<<<<<< HEAD
    pub fn configure_phy(&self, eth: &mut eth::Ethernet, sys: &Sys) -> Self {
=======
    pub fn configure_phy(&self, eth: &mut eth::Ethernet) {
>>>>>>> 784b5f4a (KSZ and VSC are both talking on mgmt dev kit)
        // The KSZ8463 connects to the SP over RMII, then sends data to the
        // VSC8552 over 100-BASE FX
        self.ksz.configure(sys);

        // The VSC8552 connects the KSZ switch to the management network
        // over SGMII
<<<<<<< HEAD
        configure_vsc8552(eth, sys);

        Self { ksz }
=======
        configure_vsc8552(eth);
>>>>>>> 784b5f4a (KSZ and VSC are both talking on mgmt dev kit)
    }

    pub fn wake(&self, eth: &mut eth::Ethernet) {
        let p1_sr = self.ksz.read(KszRegister::P1MBSR).unwrap();
        ringbuf_entry!(Trace::Ksz8463Status {
            port: 1,
            status: p1_sr
        });

        let p2_sr = self.ksz.read(KszRegister::P2MBSR).unwrap();
        ringbuf_entry!(Trace::Ksz8463Status {
            port: 2,
            status: p2_sr
        });

        for port in [0, 1] {
            let status = eth.smi_read(port, eth::SmiClause22Register::Status);
            ringbuf_entry!(Trace::Vsc8552Status { port, status });
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Helper struct to implement the `PhyRw` trait using direct access through
/// `eth`'s MIIM registers.
struct MiimBridge<'a> {
    eth: &'a mut eth::Ethernet,
}

impl PhyRw for MiimBridge<'_> {
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        Ok(self.eth.smi_read(phy, reg.addr).into())
    }
    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        self.eth.smi_write(phy, reg.addr, value.into());
        Ok(())
    }
}

// We're talking to a VSC8552, which is compatible with the VSC85xx trait.
impl PhyVsc85xx for MiimBridge<'_> {}

pub fn configure_vsc8552(eth: &mut eth::Ethernet, sys: &Sys) {
    use sys_api::*;

    // TODO: wait for PLL lock to happen here

    // Start with reset low and COMA_MODE high
    // - SP_TO_PHY2_RESET_3V3_L (PI14)
    let nrst = Port::I.pin(14);
    sys.gpio_reset(nrst).unwrap();
    sys.gpio_configure_output(
        nrst,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    )
    .unwrap();

    // - SP_TO_PHY2_COMA_MODE (PI15, internal pull-up)
    let coma_mode = Port::I.pin(15);
    sys.gpio_set(coma_mode).unwrap();
    sys.gpio_configure_output(
        coma_mode,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    )
    .unwrap();

    // SP_TO_LDO_PHY2_EN (PI11)
    let phy2_pwr_en = Port::I.pin(11);
    sys.gpio_reset(phy2_pwr_en).unwrap();
    sys.gpio_configure_output(
        phy2_pwr_en,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    )
    .unwrap();
    sys.gpio_reset(phy2_pwr_en).unwrap();
    sleep_for(10); // TODO: how long does this need to be?

    // Power on
    sys.gpio_set(phy2_pwr_en).unwrap();
    sleep_for(4);
    // TODO: sleep for PG lines going high here

    sys.gpio_set(nrst).unwrap();
    sleep_for(120); // Wait for the chip to come out of reset

    // This PHY is on MIIM ports 0 and 1, based on resistor strapping
    let mut phy_rw = MiimBridge { eth };
    let mut phy = Phy {
        port: 0,
        rw: &mut phy_rw,
    };
    vsc85xx::init_vsc8552_phy(&mut phy).unwrap();

    // Disable COMA_MODE
    sys.gpio_reset(coma_mode).unwrap();
}
