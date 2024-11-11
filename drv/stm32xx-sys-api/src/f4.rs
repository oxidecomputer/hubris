// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32F4 specifics

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
    Ahb1,
    Ahb2,
    Ahb3,
    Apb1,
    Apb2,
}

/// Peripheral numbering.
///
/// Peripheral bit numbers per the STM32F4 documentation.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u32)]
pub enum Peripheral {
    GpioA = periph(Group::Ahb1, 0),
    GpioB = periph(Group::Ahb1, 1),
    GpioC = periph(Group::Ahb1, 2),
    GpioD = periph(Group::Ahb1, 3),
    GpioE = periph(Group::Ahb1, 4),
    GpioF = periph(Group::Ahb1, 5),
    GpioG = periph(Group::Ahb1, 6),
    GpioH = periph(Group::Ahb1, 7),
    GpioI = periph(Group::Ahb1, 8),
    GpioJ = periph(Group::Ahb1, 9),
    GpioK = periph(Group::Ahb1, 10),
    Crc = periph(Group::Ahb1, 12),
    Dma1 = periph(Group::Ahb1, 21),
    Dma2 = periph(Group::Ahb1, 22),
    Dma2d = periph(Group::Ahb1, 23),
    EthMac = periph(Group::Ahb1, 25),
    UsbOtgHs = periph(Group::Ahb1, 29),
}
