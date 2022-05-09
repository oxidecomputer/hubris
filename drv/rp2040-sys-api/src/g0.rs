// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32G0 specifics

use crate::periph;
use userlib::FromPrimitive;

/// Peripherals appear in "groups." All peripherals in a group are controlled
/// from the same subset of registers in the RCC.
///
/// The reference manual lacks a term for this, so we made this one up. It would
/// be tempting to refer to these as "buses," but in practice there are almost
/// always more groups than there are buses, particularly on M0.
///
/// This is `pub` mostly for use inside driver-servers.
#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
pub enum Group {
    Iop = 0,
    Ahb,
    Apb1,
    Apb2,
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
    GpioF = periph(Group::Iop, 5),
    GpioE = periph(Group::Iop, 4),
    GpioD = periph(Group::Iop, 3),
    GpioC = periph(Group::Iop, 2),
    GpioB = periph(Group::Iop, 1),
    GpioA = periph(Group::Iop, 0),

    Rng = periph(Group::Ahb, 18), // G0x1 only
    Aes = periph(Group::Ahb, 16), // G0x1 only
    Crc = periph(Group::Ahb, 12),
    Flash = periph(Group::Ahb, 8),
    Dma2 = periph(Group::Ahb, 1),
    Dma1 = periph(Group::Ahb, 0),

    LpTim1 = periph(Group::Apb1, 31), // G0x1 only
    LpTim2 = periph(Group::Apb1, 30), // G0x1 only
    Dac1 = periph(Group::Apb1, 29),   // G0x1 only
    Pwr = periph(Group::Apb1, 28),
    Dbg = periph(Group::Apb1, 27),
    Ucpd2 = periph(Group::Apb1, 26), // G0x1 only
    Ucpd1 = periph(Group::Apb1, 25), // G0x1 only
    Cec = periph(Group::Apb1, 24),   // G0x1 only
    I2c3 = periph(Group::Apb1, 23),
    I2c2 = periph(Group::Apb1, 22),
    I2c1 = periph(Group::Apb1, 21),
    LpUart1 = periph(Group::Apb1, 20), // G0x1 only
    Usart4 = periph(Group::Apb1, 19),
    Usart3 = periph(Group::Apb1, 18),
    Usart2 = periph(Group::Apb1, 17),
    Crs = periph(Group::Apb1, 16), // G0x1 only
    Spi3 = periph(Group::Apb1, 15),
    Spi2 = periph(Group::Apb1, 14),
    Usb = periph(Group::Apb1, 13),
    Fdcan = periph(Group::Apb1, 12), // G0x1 only
    Usart6 = periph(Group::Apb1, 9),
    Usart5 = periph(Group::Apb1, 8),
    LpUart2 = periph(Group::Apb1, 7), // G0x1 only
    Tim7 = periph(Group::Apb1, 5),
    Tim6 = periph(Group::Apb1, 4),
    Tim4 = periph(Group::Apb1, 2),
    Tim3 = periph(Group::Apb1, 1),
    Tim2 = periph(Group::Apb1, 0), // G0x1 only

    Adc = periph(Group::Apb2, 20),
    Tim17 = periph(Group::Apb2, 18),
    Tim16 = periph(Group::Apb2, 17),
    Tim15 = periph(Group::Apb2, 16),
    Tim14 = periph(Group::Apb2, 15),
    Usart1 = periph(Group::Apb2, 14),
    Spi1 = periph(Group::Apb2, 12),
    Tim1 = periph(Group::Apb2, 11),
    Syscfg = periph(Group::Apb2, 0),
}
