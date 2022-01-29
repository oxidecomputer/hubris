// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the STM32G0 RCC server.

#![no_std]

use unwrap_lite::UnwrapLite;
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
        self.enable_clock_raw(peripheral as u32).unwrap_lite()
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
        self.disable_clock_raw(peripheral as u32).unwrap_lite()
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
        self.enter_reset_raw(peripheral as u32).unwrap_lite()
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
        self.leave_reset_raw(peripheral as u32).unwrap_lite()
    }
}

//
// A few macros for purposes of defining the Peripheral enum in terms that our
// driver is expecting:
//
// - RCC_IOPENR[31:0] and RCC_IOPRSTR[31:0] are indices 31-0.
// - RCC_AHBENR[31:0] and RCC_AHBRSTR[31:0] are indices 63-32.
// - RCC_APBENR1[31:0] and RCC_APBRSTR1[31:0] are indices 95-64.
// - RCC_APBENR2[31:0] and RCC_APBRSTR2[31:0] are indices 127-96.
//
macro_rules! iop {
    ($bit:literal) => {
        (0 * 32) + $bit
    };
}

macro_rules! ahb {
    ($bit:literal) => {
        (1 * 32) + $bit
    };
}

macro_rules! apb1 {
    ($bit:literal) => {
        (2 * 32) + $bit
    };
}

macro_rules! apb2 {
    ($bit:literal) => {
        (3 * 32) + $bit
    };
}

/// Peripheral numbering.
///
/// Peripheral bit numbers per the STM32G0 documentation, starting at section:
///
///    STM32G0 PART     MANUAL      SECTION
///    G0x0             RM0454      5.4.8 (RCC_IOPRSTR)
///    G0x1             RM0444      5.4.9 (RCC_IOPRSTR)
///
/// These are in the order that they appear in the documentation.   This is
/// the union of all STM32G0 peripherals; not all peripherals will exist on
/// all variants!
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u32)]
pub enum Peripheral {
    GpioF = iop!(5),
    GpioE = iop!(4),
    GpioD = iop!(3),
    GpioC = iop!(2),
    GpioB = iop!(1),
    GpioA = iop!(0),

    Rng = ahb!(18), // G0x1 only
    Aes = ahb!(16), // G0x1 only
    Crc = ahb!(12),
    Flash = ahb!(8),
    Dma2 = ahb!(1),
    Dma1 = ahb!(0),

    LpTim1 = apb1!(31), // G0x1 only
    LpTim2 = apb1!(30), // G0x1 only
    Dac1 = apb1!(29),   // G0x1 only
    Pwr = apb1!(28),
    Dbg = apb1!(27),
    Ucpd2 = apb1!(26), // G0x1 only
    Ucpd1 = apb1!(25), // G0x1 only
    Cec = apb1!(24),   // G0x1 only
    I2c3 = apb1!(23),
    I2c2 = apb1!(22),
    I2c1 = apb1!(21),
    LpUart1 = apb1!(20), // G0x1 only
    Usart4 = apb1!(19),
    Usart3 = apb1!(18),
    Usart2 = apb1!(17),
    Crs = apb1!(16), // G0x1 only
    Spi3 = apb1!(15),
    Spi2 = apb1!(14),
    Usb = apb1!(13),
    Fdcan = apb1!(12), // G0x1 only
    Usart6 = apb1!(9),
    Usart5 = apb1!(8),
    LpUart2 = apb1!(7), // G0x1 only
    Tim7 = apb1!(5),
    Tim6 = apb1!(4),
    Tim4 = apb1!(2),
    Tim3 = apb1!(1),
    Tim2 = apb1!(0), // G0x1 only

    Adc = apb2!(20),
    Tim17 = apb2!(18),
    Tim16 = apb2!(17),
    Tim15 = apb2!(16),
    Tim14 = apb2!(15),
    Usart1 = apb2!(14),
    Spi1 = apb2!(12),
    Tim1 = apb2!(11),
    Syscfg = apb2!(0),
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
