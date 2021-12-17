// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the STM32H7 RCC server.

#![no_std]

use userlib::*;

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum RccError {
    NoSuchPeripheral = 1,
}

impl From<u32> for RccError {
    fn from(x: u32) -> Self {
        match x {
            1 => RccError::NoSuchPeripheral,
            _ => panic!(),
        }
    }
}

impl Rcc {
    /// Requests that the clock to a peripheral be turned on.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn enable_clock(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.enable_clock_raw(peripheral as u32).unwrap()
    }

    /// Requests that the clock to a peripheral be turned off.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn disable_clock(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.disable_clock_raw(peripheral as u32).unwrap()
    }

    /// Requests that the reset line to a peripheral be asserted.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn enter_reset(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.enter_reset_raw(peripheral as u32).unwrap()
    }

    /// Requests that the reset line to a peripheral be deasserted.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn leave_reset(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.leave_reset_raw(peripheral as u32).unwrap()
    }
}

//
// A few macros for purposes of defining the Peripheral enum in terms that our
// driver is expecting:
//
// - AHB1ENR[31:0] are indices 31-0.
// - AHB2ENR[31:0] are indices 63-32.
// - AHB3ENR[31:0] are indices 95-64.
// - AHB4ENR[31:0] are indices 127-96.
// - APB1LENR[31:0] are indices 159-128.
// - APB1HENR[31:0] are indices 191-160.
// - APB2ENR[31:0] are indices 223-192.
// - APB3ENR[31:0] are indices 255-224.
// - APB4ENR[31:0] are indices 287-256.
//
macro_rules! ahb1 {
    ($bit:literal) => {
        (0 * 32) + $bit
    };
}

macro_rules! ahb2 {
    ($bit:literal) => {
        (1 * 32) + $bit
    };
}

macro_rules! ahb3 {
    ($bit:literal) => {
        (2 * 32) + $bit
    };
}

macro_rules! ahb4 {
    ($bit:literal) => {
        (3 * 32) + $bit
    };
}

macro_rules! apb1l {
    ($bit:literal) => {
        (4 * 32) + $bit
    };
}

macro_rules! apb1h {
    ($bit:literal) => {
        (5 * 32) + $bit
    };
}

macro_rules! apb2 {
    ($bit:literal) => {
        (6 * 32) + $bit
    };
}

macro_rules! apb3 {
    ($bit:literal) => {
        (7 * 32) + $bit
    };
}

macro_rules! apb4 {
    ($bit:literal) => {
        (8 * 32) + $bit
    };
}

/// Peripheral numbering.
///
/// Peripheral bit numbers per the STM32H7 documentation, starting with the
/// following sections:
///
///    STM32H7 PART    SECTION
///    B3/A3,B0        8.7.38
///    43/53,42,50     8.7.40
///    47/57,45/55     9.7.39
///
/// These are in the order that they appear in the documentation -- which,
/// while thankfully uniform across the STM32H7 variants, is not necessarily
/// an order that is at all sensible!  This is the union of all STM32H7
/// peripherals; not all peripherals will exist on all variants!
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u32)]
pub enum Peripheral {
    AxisRam = ahb3!(31),  // 47 only
    Itcm = ahb3!(30),     // 47 only
    Dtcm2 = ahb3!(29),    // 47 only
    Dtcm1 = ahb3!(28),    // 47 only
    Gfxmmu = ahb3!(24),   // B3 only
    Otf2 = ahb3!(23),     // B3 only
    Otf1 = ahb3!(22),     // B3 only
    Iomngr = ahb3!(21),   // B3 only
    OctoSpi2 = ahb3!(19), // B3 only
    Sdmmc1 = ahb3!(16),

    #[cfg(feature = "h7b3")]
    OctoSpi1 = ahb3!(14), // B3 only
    #[cfg(any(feature = "h743", feature = "h747", feature = "h753"))]
    QuadSpi = ahb3!(14), // 43/47 only

    Fmc = ahb3!(12),
    Flash = ahb3!(8), // 47 only
    JpgDec = ahb3!(5),
    Dma2d = ahb3!(4),
    Mdma = ahb3!(0),

    Usb2Otg = ahb1!(27), // 43/47 only
    Usb1Phy = ahb1!(26),
    Usb1Otg = ahb1!(25),
    Usb2Phy = ahb1!(18), // 43/47 only
    Eth1Rx = ahb1!(17),  // 43/47 only
    Eth1Tx = ahb1!(16),  // 43/47 only
    Eth1Mac = ahb1!(15), // 43/47 only
    Art = ahb1!(14),     // 47 only
    Crc = ahb1!(9),      // B3 only
    Adc1 = ahb1!(5),
    Dma2 = ahb1!(1),
    Dma1 = ahb1!(0),

    Sram3 = ahb2!(31), // 43/47 only
    Sram2 = ahb2!(30),
    Sram1 = ahb2!(29),
    DfsdmDma = ahb2!(11), // B3 only
    Sdmmc2 = ahb2!(9),

    #[cfg(any(feature = "h753"))]
    Rng = ahb2!(6),
    #[cfg(any(feature = "h753"))]
    Hash = ahb2!(5),
    #[cfg(any(feature = "h753"))]
    Crypt = ahb2!(4),

    #[cfg(feature = "h7b3")]
    Hsem = ahb2!(2), // B3 differs from 43/47

    Dcmi = ahb2!(0),

    SmartRunSram = ahb4!(29), // B3 only
    BackupRam = ahb4!(28),

    #[cfg(any(feature = "h743", feature = "h747", feature = "h753"))]
    Hsem = ahb4!(25), // 43/47: differs from B3

    #[cfg(feature = "h7b3")]
    Bdma2 = ahb4!(21),
    #[cfg(any(feature = "h743", feature = "h747", feature = "h757"))]
    Bdma = ahb4!(21),

    GpioK = ahb4!(10),
    GpioJ = ahb4!(9),
    GpioI = ahb4!(8),
    GpioH = ahb4!(7),
    GpioG = ahb4!(6),
    GpioF = ahb4!(5),
    GpioE = ahb4!(4),
    GpioD = ahb4!(3),
    GpioC = ahb4!(2),
    GpioB = ahb4!(1),
    GpioA = ahb4!(0),

    Wwdg = apb3!(6),
    Dsi = apb3!(4), // 47 only
    Ltdc = apb3!(3),

    Uart8 = apb1l!(31),
    Uart7 = apb1l!(30),
    Dac1 = apb1l!(29),
    HdmiCec = apb1l!(27),
    I2c3 = apb1l!(23),
    I2c2 = apb1l!(22),
    I2c1 = apb1l!(21),
    Uart5 = apb1l!(20),
    Uart4 = apb1l!(19),
    Usart3 = apb1l!(18),
    Usart2 = apb1l!(17),
    Spdifrx = apb1l!(16),
    Spi3 = apb1l!(15),
    Spi2 = apb1l!(14),
    Wwdg2 = apb1l!(11), // 47 only
    LpTim1 = apb1l!(9),
    Tim14 = apb1l!(8),
    Tim13 = apb1l!(7),
    Tim12 = apb1l!(6),
    Tim7 = apb1l!(5),
    Tim6 = apb1l!(4),
    Tim5 = apb1l!(3),
    Tim4 = apb1l!(2),
    Tim3 = apb1l!(1),
    Tim2 = apb1l!(0),

    Fdcan = apb1h!(8),
    Mdios = apb1h!(5),
    Opamp = apb1h!(4),
    Swp = apb1h!(2),
    Crsen = apb1h!(1),

    #[cfg(feature = "h7b3")]
    Dfsdm1 = apb2!(30), // B3 differs from 43/47

    Hrtim = apb2!(29), // 43/47 only

    #[cfg(any(feature = "h743", feature = "h747", feature = "h757"))]
    Dfsdm1 = apb2!(28), // 43/47 differ from B3

    Sai3 = apb2!(24), // 43/47 only
    Sai2 = apb2!(23),
    Sai1 = apb2!(22),
    Spi5 = apb2!(20),
    Tim17 = apb2!(18),
    Tim16 = apb2!(17),
    Tim15 = apb2!(16),
    Spi4 = apb2!(13),
    Spi1 = apb2!(12),
    Usart10 = apb2!(7), // B3 only
    Uart9 = apb2!(6),   // B3 only
    Usart6 = apb2!(5),
    Usart1 = apb2!(4),
    Tim8 = apb2!(1),
    Tim1 = apb2!(0),

    Dfsdm2 = apb4!(27), // B3 only
    Dts = apb4!(26),    // B3 only
    Sai4 = apb4!(21),   // 43/47 only
    RtcApb = apb4!(16),
    Vref = apb4!(15),
    Comp1 = apb4!(14),
    Dac2 = apb4!(13),   // B3 only
    LpTim5 = apb4!(12), // 43/47 only
    LpTim4 = apb4!(11), // 43/47 only
    LpTim3 = apb4!(10),
    LpTim2 = apb4!(9),
    I2c4 = apb4!(7),
    Spi6 = apb4!(5),
    LpUart = apb4!(3),
    SysCfg = apb4!(1),
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
