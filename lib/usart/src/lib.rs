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

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
pub mod stm32h7;

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use self::stm32h7::Device;

use tinyvec::ArrayVec;

/// Supported baud rates
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum BaudRate {
    Rate9600 = 9_600,
    Rate57600 = 57_600,
    Rate115200 = 115_200,
}

/// Handle to an enabled USART device.
pub struct Usart<const TX_BUF_LEN: usize, const RX_BUF_LEN: usize> {
    usart: Device,
    irq_mask: u32,
    tx_buf: ArrayVec<[u8; TX_BUF_LEN]>,
    rx_buf: ArrayVec<[u8; RX_BUF_LEN]>,
    rx_overrun: bool,
}

/// Errors detected during rx
pub enum RxError {
    /// Data has been lost due to rx buffer overrun
    Overrun,
}

impl<const TX_BUF_LEN: usize, const RX_BUF_LEN: usize>
    Usart<TX_BUF_LEN, RX_BUF_LEN>
{
    /// Start managing `device`.
    ///
    /// Enables the `irq` interrupt; the caller is responsible for calling
    /// [`Usart::handle_interrupt()`] when that interrupt fires.
    pub fn new(usart: Device, irq_mask: u32) -> Self {
        // Turn on our interrupt. We haven't enabled any interrupt sources at
        // the USART side yet, but we will momentarily.
        userlib::sys_irq_control(irq_mask, true);

        // Enable RX interrupts from the USART side.
        usart.enable_rx_interrupts();

        Self {
            usart,
            irq_mask,
            tx_buf: ArrayVec::new(),
            rx_buf: ArrayVec::new(),
            rx_overrun: false,
        }
    }

    /// Handle (by stepping tx/rx if possible) and then reenable the USART
    /// interrupt.
    pub fn handle_interrupt(&mut self) {
        // See if we have any data to transmit.
        if let Some(&byte) = self.tx_buf.get(0) {
            // Write it to the transmitter, if possible.
            if self.usart.try_write_tx(byte) {
                // TODO? `remove()` shifts all remaining tx data down; we could
                // use somehting more ringbuffer-like if this is too expensive.
                self.tx_buf.remove(0);
                if self.tx_buf.is_empty() {
                    self.usart.disable_tx_interrupts();
                }
            }
        }

        // See if any data has come in.
        if let Some(byte) = self.usart.try_read_rx() {
            if self.rx_buf.try_push(byte).is_some() {
                // self.rx_buf is full; we'll treat this the same as a hardware
                // overrun: discard the byte and return an error later
                self.rx_overrun = true;
            }
        }

        // reenable interrupt
        userlib::sys_irq_control(self.irq_mask, true);
    }

    /// Get mutable access to the tx/rx buffers held by `self`.
    pub fn buffers(
        &mut self,
    ) -> (TxBuf<'_, TX_BUF_LEN>, RxBuf<'_, RX_BUF_LEN>) {
        (
            TxBuf {
                usart: &self.usart,
                buf: &mut self.tx_buf,
            },
            RxBuf {
                usart: &self.usart,
                buf: &mut self.rx_buf,
                overrun: &mut self.rx_overrun,
            },
        )
    }
}

/// Tx buffer owned by a [`Usart`].
///
/// Bytes in this buffer will be transmitted as space in the transmitter becomes
/// available.
pub struct TxBuf<'a, const N: usize> {
    usart: &'a Device,
    buf: &'a mut ArrayVec<[u8; N]>,
}

impl<const N: usize> TxBuf<'_, N> {
    /// Truncate the buffer to `new_len`.
    ///
    /// Does nothing if the buffer is already shorter than `new_len`.
    pub fn truncate(&mut self, new_len: usize) {
        self.buf.truncate(new_len);
        if self.buf.is_empty() {
            self.usart.disable_tx_interrupts();
        }
    }

    /// Attempt to push `byte` into `self`.
    ///
    /// Returns `None` on success, or the value if `self` is full.
    pub fn try_push(&mut self, byte: u8) -> Option<u8> {
        if self.buf.is_empty() {
            self.usart.enable_tx_interrupts();
        }
        self.buf.try_push(byte)
    }
}

/// Rx buffer owned by a [`Usart`].
pub struct RxBuf<'a, const N: usize> {
    usart: &'a Device,
    buf: &'a mut ArrayVec<[u8; N]>,
    overrun: &'a mut bool,
}

impl<'a, const N: usize> RxBuf<'a, N> {
    /// Drain `self`, returning the bytes it currently contains.
    ///
    /// When the returned [`Drain`] is dropped, the underlying buffer will be
    /// cleared.
    ///
    /// Note that the same [`Drain`] is returned in both the success and error
    /// cases; the error case exists to notify the caller of an error that
    /// occurred during reception (i.e., an rx overflow).
    pub fn drain(self) -> Result<Drain<'a, N>, (Drain<'a, N>, RxError)> {
        if self.usart.check_and_clear_overrun() || *self.overrun {
            *self.overrun = false;
            Err((Drain(self.buf), RxError::Overrun))
        } else {
            Ok(Drain(self.buf))
        }
    }
}

// tinyvec provides an `ArrayVecDrain`, but it's simulatenously more (it
// implements `Iterator` directly) and less (it requres `[u8; N]` to implement
// `Default`) flexible than we need. we'll just wrap the array vec, expose the
// data as a slice, and clear it when we're dropped.
pub struct Drain<'a, const N: usize>(&'a mut ArrayVec<[u8; N]>);

impl<const N: usize> Drop for Drain<'_, N> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

impl<const N: usize> core::ops::Deref for Drain<'_, N> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const N: usize> Drain<'_, N> {
    pub fn as_slice(&self) -> &[u8] {
        &*self
    }
}
