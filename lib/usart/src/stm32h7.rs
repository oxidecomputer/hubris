// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7-specific USART details.

pub use drv_stm32xx_sys_api::Sys;

use super::BaudRate;

#[cfg(feature = "stm32h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "stm32h753")]
use stm32h7::stm32h753 as device;

/// Which USART device
#[derive(Debug, Clone, Copy)]
pub enum DeviceId {
    Usart3,
}

pub struct Device(&'static device::usart1::RegisterBlock);

impl Device {
    /// Turn on the `device` USART with the given baud rate.
    // TODO passing in a `Sys` seems weird. Should we take the sys task ID
    // instead?  Something else?
    pub fn turn_on(
        sys: &Sys,
        device: DeviceId,
        clock_hz: u32,
        baud_rate: BaudRate,
    ) -> Self {
        // Turn the actual peripheral on so that we can interact with it.
        turn_on_usart(sys, device);

        let usart = match device {
            DeviceId::Usart3 => {
                // From thin air, pluck a pointer to the USART register block.
                //
                // Safety: this is needlessly unsafe in the API. The USART is
                // essentially a static, and we access it through a & reference
                // so aliasing is not a concern. Were it literally a static, we
                // could just reference it.
                #[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
                unsafe {
                    &*device::USART3::ptr()
                }
            }
        };

        // The UART has clock and is out of reset, but isn't actually on until
        // we:
        usart.cr1.write(|w| w.ue().enabled());
        let baud_rate = baud_rate as u32;
        let cycles_per_bit = (clock_hz + (baud_rate / 2)) / baud_rate;
        usart.brr.write(|w| w.brr().bits(cycles_per_bit as u16));

        // Enable the UART, transmitter, and receiver.
        usart
            .cr1
            .modify(|_, w| w.ue().enabled().te().enabled().re().enabled());

        configure_pins(sys, device);

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

fn turn_on_usart(sys: &Sys, device: DeviceId) {
    use drv_stm32xx_sys_api::Peripheral;

    #[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
    const PORT_USART3: Peripheral = Peripheral::Usart3;

    match device {
        DeviceId::Usart3 => {
            sys.enable_clock(PORT_USART3);
            sys.leave_reset(PORT_USART3);
        }
    }
}

fn configure_pins(sys: &Sys, device: DeviceId) {
    use drv_stm32xx_sys_api::*;

    let tx_rx_mask = match device {
        DeviceId::Usart3 => {
            // TODO these are really board configs, not SoC configs!
            #[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
            Port::D.pin(8).and_pin(9)
        }
    };

    sys.gpio_configure_alternate(
        tx_rx_mask,
        OutputType::PushPull,
        Speed::High,
        Pull::None,
        Alternate::AF7,
    )
    .unwrap();
}
