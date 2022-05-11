// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the RP2040 SYS server.

#![no_std]

use userlib::*;

bitflags::bitflags! {
    /// Bitmask of peripherals controlled by the reset controller.
    pub struct Resets: u32 {
        const ADC = 1 << 0;
        const BUSCTRL = 1 << 1;
        const DMA = 1 << 2;
        const I2C0 = 1 << 3;
        const I2C1 = 1 << 4;
        const IO_BANK0 = 1 << 5;
        const IO_QSPI = 1 << 6;
        const JTAG = 1 << 7;
        const PADS_BANK0 = 1 << 8;
        const PADS_QSPI = 1 << 9;
        const PIO0 = 1 << 10;
        const PIO1 = 1 << 11;
        const PLL_SYS = 1 << 12;
        const PLL_USB = 1 << 13;
        const PWM = 1 << 14;
        const RTC = 1 << 15;
        const SPI0 = 1 << 16;
        const SPI1 = 1 << 17;
        const SYSCFG = 1 << 18;
        const SYSINFO = 1 << 19;
        const TBMAN = 1 << 20;
        const TIMER = 1 << 21;
        const UART0 = 1 << 22;
        const UART1 = 1 << 23;
        const USBCTRL = 1 << 24;
    }
}

/// Basically equivalent to Infallible, except that Infallible doesn't define
/// any `From<T> for Infallible` impls and we need some.
///
/// This should probably move into idol-runtime.
pub enum CantFail {}

impl TryFrom<u32> for CantFail {
    type Error = ();

    fn try_from(_: u32) -> Result<Self, Self::Error> {
        Err(())
    }
}

impl TryFrom<u16> for CantFail {
    type Error = ();

    fn try_from(_: u16) -> Result<Self, Self::Error> {
        Err(())
    }
}

impl From<CantFail> for u16 {
    fn from(x: CantFail) -> Self {
        match x {}
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BitControl {
    Normal = 0,
    Invert = 1,
    ForceLow = 2,
    ForceHigh = 3,
}

pub enum FuncSel0 {
    Spi = 1,
    Uart = 2,
    I2c = 3,
    Pwm = 4,
    Sio = 5,
    Pio0 = 6,
    Pio1 = 7,
    Usb = 9,
    Null = 0x1f,
}

impl Sys {
    /// Requests that a subset of peripherals be put into reset.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the Sys server crash while processing it.
    pub fn enter_reset(&self, resets: Resets) {
        self.enter_reset_raw(resets.bits());
    }

    /// Requests that a subset of peripherals be taken out of reset. The server
    /// will ensure that the corresponding bits in `RESET_DONE` go high before
    /// returning. (TODO: it maybe shouldn't since that commits the Sys server
    /// to a blocking operation...)
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the Sys server crash while processing it.
    pub fn leave_reset(&self, resets: Resets) {
        self.leave_reset_raw(resets.bits());
    }

    /// Changes the GPIO configuration (`GPIOx_CTRL` register) for any subset of
    /// pins in IO BANK0.
    ///
    /// Pins with corresponding 1 bits in the `pins` mask will be changed, other
    /// pins will be unaffected.
    ///
    /// For each of the arguments, `None` will leave the current GPIO setting
    /// unchanged, and `Some` will overwrite it.
    ///
    /// Note that this IPC will generate a sequence of register writes, so the
    /// changes across GPIOs will not be atomic. This is a property of the
    /// RP2040; the IPC offers the ability to set multiple pins anyway to cut
    /// down on round trips.
    ///
    /// This operation is idempotent and will be automatically retried if the
    /// Sys server crashes.
    pub fn gpio_configure(
        &self,
        pins: u32,
        irqover: Option<BitControl>,
        inover: Option<BitControl>,
        oeover: Option<BitControl>,
        outover: Option<BitControl>,
        funcsel: Option<FuncSel0>,
    ) {
        // Pack all that stuff into a compact form that also happens to line up
        // with the register bit layout.
        let mut packed = 0u32;

        if let Some(fs) = funcsel {
            packed |= 0b10_0000 | fs as u32;
        }
        if let Some(bc) = outover {
            packed |= (0b100 | bc as u32) << 8;
        }
        if let Some(bc) = oeover {
            packed |= (0b100 | bc as u32) << 12;
        }
        if let Some(bc) = inover {
            packed |= (0b100 | bc as u32) << 16;
        }
        if let Some(bc) = irqover {
            packed |= (0b100 | bc as u32) << 28;
        }

        self.gpio_configure_raw(pins, packed)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
