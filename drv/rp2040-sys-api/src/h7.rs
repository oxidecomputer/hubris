// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 specifics

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
    Ahb4,
    Apb1L,
    Apb1H,
    Apb2,
    Apb3,
    Apb4,
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
    AxisRam = periph(Group::Ahb3, 31),  // 47 only
    Itcm = periph(Group::Ahb3, 30),     // 47 only
    Dtcm2 = periph(Group::Ahb3, 29),    // 47 only
    Dtcm1 = periph(Group::Ahb3, 28),    // 47 only
    Gfxmmu = periph(Group::Ahb3, 24),   // B3 only
    Otf2 = periph(Group::Ahb3, 23),     // B3 only
    Otf1 = periph(Group::Ahb3, 22),     // B3 only
    Iomngr = periph(Group::Ahb3, 21),   // B3 only
    OctoSpi2 = periph(Group::Ahb3, 19), // B3 only
    Sdmmc1 = periph(Group::Ahb3, 16),

    #[cfg(feature = "h7b3")]
    OctoSpi1 = periph(Group::Ahb3, 14), // B3 only
    #[cfg(any(feature = "h743", feature = "h747", feature = "h753"))]
    QuadSpi = periph(Group::Ahb3, 14), // 43/47 only

    Fmc = periph(Group::Ahb3, 12),
    Flash = periph(Group::Ahb3, 8), // 47 only
    JpgDec = periph(Group::Ahb3, 5),
    Dma2d = periph(Group::Ahb3, 4),
    Mdma = periph(Group::Ahb3, 0),

    Usb2Otg = periph(Group::Ahb1, 27), // 43/47 only
    Usb1Phy = periph(Group::Ahb1, 26),
    Usb1Otg = periph(Group::Ahb1, 25),
    Usb2Phy = periph(Group::Ahb1, 18), // 43/47 only
    Eth1Rx = periph(Group::Ahb1, 17),  // 43/47 only
    Eth1Tx = periph(Group::Ahb1, 16),  // 43/47 only
    Eth1Mac = periph(Group::Ahb1, 15), // 43/47 only
    Art = periph(Group::Ahb1, 14),     // 47 only
    Crc = periph(Group::Ahb1, 9),      // B3 only
    Adc1 = periph(Group::Ahb1, 5),
    Dma2 = periph(Group::Ahb1, 1),
    Dma1 = periph(Group::Ahb1, 0),

    Sram3 = periph(Group::Ahb2, 31), // 43/47 only
    Sram2 = periph(Group::Ahb2, 30),
    Sram1 = periph(Group::Ahb2, 29),
    DfsdmDma = periph(Group::Ahb2, 11), // B3 only
    Sdmmc2 = periph(Group::Ahb2, 9),

    #[cfg(any(feature = "h753", feature = "h743"))]
    Rng = periph(Group::Ahb2, 6),
    #[cfg(any(feature = "h753"))]
    Hash = periph(Group::Ahb2, 5),
    #[cfg(any(feature = "h753"))]
    Crypt = periph(Group::Ahb2, 4),

    #[cfg(feature = "h7b3")]
    Hsem = periph(Group::Ahb2, 2), // B3 differs from 43/47

    Dcmi = periph(Group::Ahb2, 0),

    SmartRunSram = periph(Group::Ahb4, 29), // B3 only
    BackupRam = periph(Group::Ahb4, 28),

    #[cfg(any(feature = "h743", feature = "h747", feature = "h753"))]
    Hsem = periph(Group::Ahb4, 25), // 43/47: differs from B3

    #[cfg(feature = "h7b3")]
    Bdma2 = periph(Group::Ahb4, 21),
    #[cfg(any(feature = "h743", feature = "h747", feature = "h757"))]
    Bdma = periph(Group::Ahb4, 21),

    GpioK = periph(Group::Ahb4, 10),
    GpioJ = periph(Group::Ahb4, 9),
    GpioI = periph(Group::Ahb4, 8),
    GpioH = periph(Group::Ahb4, 7),
    GpioG = periph(Group::Ahb4, 6),
    GpioF = periph(Group::Ahb4, 5),
    GpioE = periph(Group::Ahb4, 4),
    GpioD = periph(Group::Ahb4, 3),
    GpioC = periph(Group::Ahb4, 2),
    GpioB = periph(Group::Ahb4, 1),
    GpioA = periph(Group::Ahb4, 0),

    Wwdg = periph(Group::Apb3, 6),
    Dsi = periph(Group::Apb3, 4), // 47 only
    Ltdc = periph(Group::Apb3, 3),

    Uart8 = periph(Group::Apb1L, 31),
    Uart7 = periph(Group::Apb1L, 30),
    Dac1 = periph(Group::Apb1L, 29),
    HdmiCec = periph(Group::Apb1L, 27),
    I2c3 = periph(Group::Apb1L, 23),
    I2c2 = periph(Group::Apb1L, 22),
    I2c1 = periph(Group::Apb1L, 21),
    Uart5 = periph(Group::Apb1L, 20),
    Uart4 = periph(Group::Apb1L, 19),
    Usart3 = periph(Group::Apb1L, 18),
    Usart2 = periph(Group::Apb1L, 17),
    Spdifrx = periph(Group::Apb1L, 16),
    Spi3 = periph(Group::Apb1L, 15),
    Spi2 = periph(Group::Apb1L, 14),
    Wwdg2 = periph(Group::Apb1L, 11), // 47 only
    LpTim1 = periph(Group::Apb1L, 9),
    Tim14 = periph(Group::Apb1L, 8),
    Tim13 = periph(Group::Apb1L, 7),
    Tim12 = periph(Group::Apb1L, 6),
    Tim7 = periph(Group::Apb1L, 5),
    Tim6 = periph(Group::Apb1L, 4),
    Tim5 = periph(Group::Apb1L, 3),
    Tim4 = periph(Group::Apb1L, 2),
    Tim3 = periph(Group::Apb1L, 1),
    Tim2 = periph(Group::Apb1L, 0),

    Fdcan = periph(Group::Apb1H, 8),
    Mdios = periph(Group::Apb1H, 5),
    Opamp = periph(Group::Apb1H, 4),
    Swp = periph(Group::Apb1H, 2),
    Crsen = periph(Group::Apb1H, 1),

    #[cfg(feature = "h7b3")]
    Dfsdm1 = periph(Group::Apb2, 30), // B3 differs from 43/47

    Hrtim = periph(Group::Apb2, 29), // 43/47 only

    #[cfg(any(feature = "h743", feature = "h747", feature = "h757"))]
    Dfsdm1 = periph(Group::Apb2, 28), // 43/47 differ from B3

    Sai3 = periph(Group::Apb2, 24), // 43/47 only
    Sai2 = periph(Group::Apb2, 23),
    Sai1 = periph(Group::Apb2, 22),
    Spi5 = periph(Group::Apb2, 20),
    Tim17 = periph(Group::Apb2, 18),
    Tim16 = periph(Group::Apb2, 17),
    Tim15 = periph(Group::Apb2, 16),
    Spi4 = periph(Group::Apb2, 13),
    Spi1 = periph(Group::Apb2, 12),
    Usart10 = periph(Group::Apb2, 7), // B3 only
    Uart9 = periph(Group::Apb2, 6),   // B3 only
    Usart6 = periph(Group::Apb2, 5),
    Usart1 = periph(Group::Apb2, 4),
    Tim8 = periph(Group::Apb2, 1),
    Tim1 = periph(Group::Apb2, 0),

    Dfsdm2 = periph(Group::Apb4, 27), // B3 only
    Dts = periph(Group::Apb4, 26),    // B3 only
    Sai4 = periph(Group::Apb4, 21),   // 43/47 only
    RtcApb = periph(Group::Apb4, 16),
    Vref = periph(Group::Apb4, 15),
    Comp1 = periph(Group::Apb4, 14),
    Dac2 = periph(Group::Apb4, 13),   // B3 only
    LpTim5 = periph(Group::Apb4, 12), // 43/47 only
    LpTim4 = periph(Group::Apb4, 11), // 43/47 only
    LpTim3 = periph(Group::Apb4, 10),
    LpTim2 = periph(Group::Apb4, 9),
    I2c4 = periph(Group::Apb4, 7),
    Spi6 = periph(Group::Apb4, 5),
    LpUart = periph(Group::Apb4, 3),
    SysCfg = periph(Group::Apb4, 1),
}
