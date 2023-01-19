// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32G0 U(S)ART.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.

#![no_std]
#![no_main]

#[cfg(feature = "g031")]
use stm32g0::stm32g031 as device;

#[cfg(feature = "g070")]
use stm32g0::stm32g070 as device;

#[cfg(feature = "g0b1")]
use stm32g0::stm32g0b1 as device;

use userlib::*;

task_slot!(SYS, sys);

#[derive(Copy, Clone, Debug, FromPrimitive)]
enum Operation {
    Write = 1,
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

// TODO: it is super unfortunate to have to write this by hand, but deriving
// ToPrimitive makes us check at runtime whether the value fits
impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

struct Transmit {
    caller: hl::Caller<()>,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_usart();

    // From thin air, pluck a pointer to the USART register block.
    //
    // Safety: this is needlessly unsafe in the API. The USART is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let usart = unsafe { &*device::USART1::ptr() };

    // The UART has clock and is out of reset, but isn't actually on until we:
    // Work out our baud rate divisor.
    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 16_000_000;
    const BAUDRATE: u32 = 115_200;

    #[cfg(any(feature = "g031", feature = "g070"))]
    {
        usart.brr.write(|w| unsafe { w.bits(CLOCK_HZ / BAUDRATE) });
        usart.cr1.write(|w| w.ue().set_bit());
        usart.cr1.modify(|_, w| w.te().set_bit());
    }
    #[cfg(feature = "g0b1")]
    {
        usart
            .brr
            .write(|w| unsafe { w.brr().bits((CLOCK_HZ / BAUDRATE) as u16) });
        usart.cr1_fifo_disabled().write(|w| w.ue().set_bit());
        // Enable the transmitter.
        usart.cr1_fifo_disabled().modify(|_, w| w.te().set_bit());
    }

    configure_pins();

    // Turn on our interrupt. We haven't enabled any interrupt sources at the
    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(notifications::USART_IRQ, true);

    // Field messages.
    let mask = notifications::USART_IRQ;
    let mut tx: Option<Transmit> = None;

    loop {
        hl::recv(
            // Buffer (none required)
            &mut [],
            // Notification mask
            mask,
            // State to pass through to whichever closure below gets run
            &mut tx,
            // Notification handler
            |txref, bits| {
                if bits & 1 != 0 {
                    // Handling an interrupt. To allow for spurious interrupts,
                    // check the individual conditions we care about, and
                    // unconditionally re-enable the IRQ at the end of the handler.

                    #[cfg(any(feature = "g031", feature = "g070"))]
                    if usart.isr.read().txe().bit() {
                        // TX register empty. Do we need to send something?
                        step_transmit(usart, txref);
                    }

                    #[cfg(feature = "g0b1")]
                    if usart.isr_fifo_disabled().read().txe().bit() {
                        // TX register empty. Do we need to send something?
                        step_transmit(&usart, txref);
                    }

                    sys_irq_control(notifications::USART_IRQ, true);
                }
            },
            // Message handler
            |txref, op, msg| match op {
                Operation::Write => {
                    // Validate lease count and buffer sizes first.
                    let ((), caller) =
                        msg.fixed_with_leases(1).ok_or(ResponseCode::BadArg)?;

                    // Deny incoming writes if we're already running one.
                    if txref.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let borrow = caller.borrow(0);
                    let info = borrow.info().ok_or(ResponseCode::BadArg)?;
                    // Provide feedback to callers if they fail to provide a
                    // readable lease (otherwise we'd fail accessing the borrow
                    // later, which is a defection case and we won't reply at
                    // all).
                    if !info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    // Okay! Begin a transfer!
                    *txref = Some(Transmit {
                        caller,
                        pos: 0,
                        len: info.len,
                    });

                    // OR the TX register empty signal into the USART interrupt.
                    #[cfg(any(feature = "g031", feature = "g070"))]
                    usart.cr1.modify(|_, w| w.txeie().set_bit());
                    #[cfg(feature = "g0b1")]
                    usart
                        .cr1_fifo_disabled()
                        .modify(|_, w| w.txeie().set_bit());

                    // We'll do the rest as interrupts arrive.
                    Ok(())
                }
            },
        );
    }
}

fn turn_on_usart() {
    use drv_stm32xx_sys_api::{Peripheral, Sys};
    let rcc_driver = Sys::from(SYS.get_task_id());

    const PORT: Peripheral = Peripheral::Usart1;

    rcc_driver.enable_clock(PORT);
    rcc_driver.leave_reset(PORT);
}

fn configure_pins() {
    use drv_stm32xx_sys_api::*;

    let gpio_driver = SYS.get_task_id();
    let gpio_driver = Sys::from(gpio_driver);

    // TODO these are really board configs, not SoC configs!
    const TX_RX_MASK: PinSet = Port::C.pin(4).and_pin(5);

    gpio_driver.gpio_configure_alternate(
        TX_RX_MASK,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
        Alternate::AF1,
    );
}

fn step_transmit(
    usart: &device::usart1::RegisterBlock,
    tx: &mut Option<Transmit>,
) {
    // Clearer than just using replace:
    fn end_transmission(
        usart: &device::usart1::RegisterBlock,
        state: &mut Option<Transmit>,
    ) -> hl::Caller<()> {
        #[cfg(any(feature = "g031", feature = "g070"))]
        usart.cr1.modify(|_, w| w.txeie().clear_bit());
        #[cfg(feature = "g0b1")]
        usart
            .cr1_fifo_disabled()
            .modify(|_, w| w.txeie().clear_bit());
        state.take().unwrap().caller
    }

    let txs = if let Some(txs) = tx { txs } else { return };

    if let Some(byte) = txs.caller.borrow(0).read_at::<u8>(txs.pos) {
        // Stuff byte into transmitter.
        usart.tdr.write(|w| w.tdr().bits(u16::from(byte)));

        txs.pos += 1;
        if txs.pos == txs.len {
            end_transmission(usart, tx).reply(());
        }
    } else {
        end_transmission(usart, tx).reply_fail(ResponseCode::BadArg);
    }
}
