// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the RP2040 SYS server.

#![no_std]

use userlib::*;

bitflags::bitflags! {
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

impl Sys {
    /// Requests that a subset of peripherals be put into reset.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    pub fn enter_reset(&self, resets: Resets) {
        self.enter_reset_raw(resets.bits());
    }

    /// Requests that a subset of peripherals be taken out of reset. The server
    /// will ensure that the corresponding bits in `RESET_DONE` go high before
    /// returning.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    pub fn leave_reset(&self, resets: Resets) {
        self.leave_reset_raw(resets.bits());
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
