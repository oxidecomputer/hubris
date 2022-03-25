// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7-specific USART details.

pub use drv_stm32xx_sys_api;

#[cfg(feature = "h743")]
pub use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
pub use stm32h7::stm32h753 as device;

use drv_stm32xx_sys_api::{Alternate, Peripheral, PinSet, Sys};
use unwrap_lite::UnwrapLite;

pub struct Device(&'static device::usart1::RegisterBlock);

impl Device {
    /// Turn on the `device` USART with the given baud rate.
    pub fn turn_on(
        sys: &Sys,
        usart: &'static device::usart1::RegisterBlock,
        peripheral: Peripheral,
        tx_rx_mask: PinSet,
        alternate: Alternate,
        clock_hz: u32,
        baud_rate: u32,
    ) -> Self {
        // Turn the actual peripheral on so that we can interact with it.
        sys.enable_clock(peripheral);
        sys.leave_reset(peripheral);

        // The UART has clock and is out of reset, but isn't actually on until
        // we:
        usart.cr1.write(|w| w.ue().enabled());
        let cycles_per_bit = (clock_hz + (baud_rate / 2)) / baud_rate;
        usart.brr.write(|w| w.brr().bits(cycles_per_bit as u16));

        // Enable the UART, transmitter, and receiver.
        usart
            .cr1
            .modify(|_, w| w.ue().enabled().te().enabled().re().enabled());

        sys.gpio_configure_alternate(
            tx_rx_mask,
            drv_stm32xx_sys_api::OutputType::PushPull,
            drv_stm32xx_sys_api::Speed::Low,
            drv_stm32xx_sys_api::Pull::None,
            alternate,
        )
        .unwrap_lite();

        Self(usart)
    }

    pub(super) fn enable_rx_interrupts(&self) {
        // Enable interrupts for received bytes.
        self.0.cr1.modify(|_, w| w.rxneie().enabled());
    }

    pub(super) fn try_write_tx(&self, byte: u8) -> bool {
        // See if TX register is empty
        if self.0.isr.read().txe().bit() {
            // Stuff byte into transmitter.
            self.0.tdr.write(|w| w.tdr().bits(u16::from(byte)));
            true
        } else {
            false
        }
    }

    pub(super) fn try_read_rx(&self) -> Option<u8> {
        // See if RX register is nonempty
        if self.0.isr.read().rxne().bit() {
            // Read byte from receiver.
            Some(self.0.rdr.read().bits() as u8)
        } else {
            None
        }
    }

    pub(super) fn check_and_clear_overrun(&self) -> bool {
        // See if the overrun error bit is set
        if self.0.isr.read().ore().bit() {
            // Clear overrun error
            self.0.icr.write(|w| w.orecf().set_bit());
            true
        } else {
            false
        }
    }

    pub(super) fn enable_tx_interrupts(&self) {
        self.0.cr1.modify(|_, w| w.txeie().enabled());
    }

    pub(super) fn disable_tx_interrupts(&self) {
        self.0.cr1.modify(|_, w| w.txeie().disabled());
    }
}
