// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! USART interface.
//!
//! This was formerly a driver and the implementation still looks largely
//! driver-like in that it interacts with other drivers and system interrupts,
//! but USARTs are inherently single-owner. Structuring USARTs as a lib allows
//! the calling task to use the USART more directly than if it were a
//! full-fledged driver of its own.

#![no_std]

pub use drv_stm32xx_sys_api;

#[cfg(feature = "h743")]
pub use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
pub use stm32h7::stm32h753 as device;

use drv_stm32xx_sys_api::{Alternate, Peripheral, PinSet, Sys};

/// Handle to an enabled USART device.
pub struct Usart {
    usart: &'static device::usart1::RegisterBlock,
}

impl Usart {
    /// Turn on the `USART` described by `usart`, `peripheral`, `tx_rx_mask`,
    /// and `alternate`, with the baud rate defined by `clock_hz` and
    /// `baud_rate`.
    ///
    /// Enables interrupts from the `USART` when bytes are available to receive;
    /// the caller is responsible for enabling and handling the corresponding
    /// kernel interrupt.
    pub fn turn_on(
        sys: &Sys,
        usart: &'static device::usart1::RegisterBlock,
        peripheral: Peripheral,
        pins: &[(PinSet, Alternate)],
        clock_hz: u32,
        baud_rate: u32,
        hardware_flow_control: bool,
    ) -> Self {
        // Turn the actual peripheral on so that we can interact with it.
        sys.enable_clock(peripheral);
        sys.leave_reset(peripheral);

        // Set FIFO interrupt thresholds; must be done before enabling the
        // UART.
        //
        // Two TODOs/questions here:
        // 1. Do we want the caller to be able to choose the threshold?
        // 2. Do we want a threshold on the RX fifo? for now, use the
        //    `rxne()` bit to check for nonempty (instead of waiting for the
        //    FIFO to reach a threshold)
        //
        // Safety: 0b11x values are undefined; see RM0433 48.7.4. We're using a
        // fixed, defined value.
        unsafe {
            // 0b101 == interrupt when TX fifo is completely empty
            usart.cr3.write(|w| w.txftcfg().bits(0b101));
        }

        if hardware_flow_control {
            usart.cr3.modify(|_, w| w.rtse().enabled().ctse().enabled());
        }

        // Enable the UART in FIFO mode.
        usart.cr1.write(|w| w.fifoen().set_bit().ue().enabled());

        // set the baud rate
        let cycles_per_bit = (clock_hz + (baud_rate / 2)) / baud_rate;
        usart.brr.write(|w| w.brr().bits(cycles_per_bit as u16));

        // Enable the transmitter and receiver.
        usart.cr1.modify(|_, w| w.te().enabled().re().enabled());

        for &(mask, alternate) in pins {
            sys.gpio_configure_alternate(
                mask,
                drv_stm32xx_sys_api::OutputType::PushPull,
                drv_stm32xx_sys_api::Speed::Low,
                drv_stm32xx_sys_api::Pull::None,
                alternate,
            );
        }

        // Enable RX interrupts from the USART side.
        usart.cr1.modify(|_, w| w.rxneie().enabled());

        Self { usart }
    }

    /// Try to push `byte` into the USART's TX FIFO, returning `true` on success
    /// or `false` if the FIFO is currently full.
    pub fn try_tx_push(&self, byte: u8) -> bool {
        // Check if TX fifo is not full
        if self.usart.isr.read().txe().bit() {
            // Stuff byte into TX fifo.
            self.usart.tdr.write(|w| w.tdr().bits(u16::from(byte)));
            true
        } else {
            false
        }
    }

    /// Try to pop a byte from the USART's RX FIFO, returning `Some(_)` on
    /// success or `None` if the FIFO is currently empty.
    pub fn try_rx_pop(&self) -> Option<u8> {
        // See if RX register is nonempty
        if self.usart.isr.read().rxne().bit() {
            // Read byte from receiver.
            Some(self.usart.rdr.read().bits() as u8)
        } else {
            None
        }
    }

    // TODO Do we need check+clear methods for other error flags?
    pub fn check_and_clear_rx_overrun(&self) -> bool {
        // See if the overrun error bit is set
        if self.usart.isr.read().ore().bit() {
            // Clear overrun error
            self.usart.icr.write(|w| w.orecf().set_bit());
            true
        } else {
            false
        }
    }

    pub fn enable_rx_interrupt(&self) {
        self.usart.cr1.modify(|_, w| w.rxneie().enabled());
    }

    pub fn disable_rx_interrupt(&self) {
        self.usart.cr1.modify(|_, w| w.rxneie().disabled());
    }

    // TODO? The name of these methods may be bad if we allow callers to specify
    // the tx fifo threshold (and can set it to something other than "empty")
    pub fn enable_tx_fifo_empty_interrupt(&self) {
        self.usart.cr3.modify(|_, w| w.txftie().set_bit());
    }

    pub fn disable_tx_fifo_empty_interrupt(&self) {
        self.usart.cr3.modify(|_, w| w.txftie().clear_bit());
    }
}
