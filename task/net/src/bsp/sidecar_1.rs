// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_spi_api::{Spi, SpiDevice, SpiError};
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use ringbuf::*;
use userlib::{hl::sleep_for, task_slot, FromPrimitive};
use vsc7448_pac::types::PhyRegisterAddress;
use vsc85xx::{Phy, PhyRw, PhyVsc85xx, VscError};

task_slot!(SPI, spi_driver);
const KSZ8463_SPI_DEVICE: u8 = 0; // Based on app.toml ordering

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    KszRead(KszRegister, u16),
    KszWrite(KszRegister, u16),
    KszId(u16),
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
        let ksz = Ksz8463(spi);

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

    pub fn configure_phy(&self, eth: &mut eth::Ethernet, sys: &Sys) -> Self {
        // The KSZ8463 connects to the SP over RMII, then sends data to the
        // VSC8552 over 100-BASE FX
        let spi = Spi::from(SPI.get_task_id()).device(KSZ8463_SPI_DEVICE);
        let ksz = Ksz8463(spi);
        ksz.configure(sys);

        // The VSC8552 connects the KSZ switch to the management network
        // over SGMII
        configure_vsc8552(eth, sys);

        Self { ksz }
    }

    pub fn wake(&self, eth: &mut eth::Ethernet) {
        // These log to the ringbuf automatically
        self.ksz.read(KszRegister::P1MBSR).unwrap();
        self.ksz.read(KszRegister::P2MBSR).unwrap();

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

////////////////////////////////////////////////////////////////////////////////

const fn register_offset(address: u16) -> u16 {
    let addr10_2 = address >> 2;
    let mask_shift = 2 /* turn around bits */ + (2 * ((address >> 1) & 0x1));
    (addr10_2 << 6) | ((0x3 as u16) << mask_shift)
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum KszRegister {
    CIDER = register_offset(0x000),
    SGCR1 = register_offset(0x002),
    SGCR2 = register_offset(0x004),
    SGCR3 = register_offset(0x006),
    SGCR6 = register_offset(0x00c),
    SGCR7 = register_offset(0x00e),
    MACAR1 = register_offset(0x010),
    MACAR2 = register_offset(0x012),
    MACAR3 = register_offset(0x014),

    P1MBCR = register_offset(0x04c),
    P1MBSR = register_offset(0x04e),

    P2MBCR = register_offset(0x058),
    P2MBSR = register_offset(0x05a),

    CFGR = register_offset(0x0d8),
    DSP_CNTRL_6 = register_offset(0x734),
}

struct Ksz8463(SpiDevice);
impl Ksz8463 {
    pub fn read(&self, r: KszRegister) -> Result<u16, SpiError> {
        let cmd = (r as u16).to_be_bytes();
        let request = [cmd[0], cmd[1]];
        let mut response = [0; 4];

        self.0.exchange(&request, &mut response)?;
        let v = u16::from_le_bytes(response[2..].try_into().unwrap());
        ringbuf_entry!(Trace::KszRead(r, v));

        Ok(v)
    }

    pub fn write(&self, r: KszRegister, v: u16) -> Result<(), SpiError> {
        let cmd = (r as u16 | 0x8000).to_be_bytes(); // Set MSB to indicate write.
        let data = v.to_le_bytes();
        let request = [cmd[0], cmd[1], data[0], data[1]];

        ringbuf_entry!(Trace::KszWrite(r, v));
        self.0.write(&request[..])?;
        Ok(())
    }

    pub fn write_masked(
        &self,
        r: KszRegister,
        v: u16,
        mask: u16,
    ) -> Result<(), SpiError> {
        let _v = (self.read(r)? & !mask) | (v & mask);
        self.write(r, _v)
    }

    pub fn enabled(&self) -> Result<bool, SpiError> {
        Ok(self.read(KszRegister::CIDER)? & 0x1 != 0)
    }

    pub fn enable(&self) -> Result<(), SpiError> {
        self.write(KszRegister::CIDER, 1)
    }

    pub fn disable(&self) -> Result<(), SpiError> {
        self.write(KszRegister::CIDER, 0)
    }

    /// Configures the KSZ8463 switch in 100BASE-FX mode.
    pub fn configure(&self, sys: &Sys) {
        use sys_api::*;

        // SP_TO_EPE_RESET_L (PA0)
        let rst = Port::A.pin(0);
        sys.gpio_reset(rst).unwrap();
        sys.gpio_configure_output(
            rst,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();
        // Toggle the reset line
        sleep_for(10); // Reset must be held low for 10 ms after power up
        sys.gpio_set(rst).unwrap();
        sleep_for(1); // You have to wait 1 µs, so this is overkill

        let id = self.read(KszRegister::CIDER).unwrap();
        assert_eq!(id & !1, 0x8452);
        ringbuf_entry!(Trace::KszId(id));

        // Configure for 100BASE-FX operation
        self.enable().unwrap();
        self.write_masked(KszRegister::CFGR, 0x0, 0xc0).unwrap();
        self.write_masked(KszRegister::DSP_CNTRL_6, 0, 0x2000)
            .unwrap();
    }
}
