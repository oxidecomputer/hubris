// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32F4 U(S)ART.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.

#![no_std]
#![no_main]

#[cfg(feature = "stm32f4")]
use stm32f4::stm32f407 as device;

#[cfg(feature = "stm32f3")]
use stm32f3::stm32f303 as device;

use userlib::*;
use zerocopy::IntoBytes;

task_slot!(RCC, rcc_driver);

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
    let usart = unsafe { &*device::USART2::ptr() };

    // The UART has clock and is out of reset, but isn't actually on until we:
    usart.cr1.write(|w| w.ue().enabled());
    // Work out our baud rate divisor.
    const BAUDRATE: u32 = 115_200;

    #[cfg(feature = "stm32f3")]
    {
        const CLOCK_HZ: u32 = 8_000_000;
        usart
            .brr
            .write(|w| w.brr().bits((CLOCK_HZ / BAUDRATE) as u16));
    }

    #[cfg(feature = "stm32f4")]
    {
        const CLOCK_HZ: u32 = 16_000_000;
        const CYCLES_PER_BIT: u32 = (CLOCK_HZ + (BAUDRATE / 2)) / BAUDRATE;
        usart.brr.write(|w| {
            w.div_mantissa()
                .bits((CYCLES_PER_BIT >> 4) as u16)
                .div_fraction()
                .bits(CYCLES_PER_BIT as u8 & 0xF)
        });
    }

    // Enable the transmitter.
    usart.cr1.modify(|_, w| w.te().enabled());

    turn_on_gpioa();

    // TODO: the fact that we interact with GPIOA directly here is an expedient
    // hack, but control of the GPIOs should probably be centralized somewhere.
    let gpioa = unsafe { &*device::GPIOA::ptr() };

    // Mux the USART onto the output pins. We're using PA2/3, where USART2 is
    // selected by Alternate Function 7.
    gpioa
        .moder
        .modify(|_, w| w.moder2().alternate().moder3().alternate());
    gpioa.afrl.modify(|_, w| w.afrl2().af7().afrl3().af7());

    // Turn on our interrupt. We haven't enabled any interrupt sources at the
    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(notifications::USART_IRQ_MASK, true);

    // Field messages.
    let mut tx: Option<Transmit> = None;

    loop {
        hl::recv(
            // Buffer (none required)
            &mut [],
            // Notification mask
            notifications::USART_IRQ_MASK,
            // State to pass through to whichever closure below gets run
            &mut tx,
            // Notification handler
            |txref, bits| {
                if bits & 1 != 0 {
                    // Handling an interrupt. To allow for spurious interrupts,
                    // check the individual conditions we care about, and
                    // unconditionally re-enable the IRQ at the end of the handler.

                    #[cfg(feature = "stm32f3")]
                    let txe = usart.isr.read().txe().bit();
                    #[cfg(feature = "stm32f4")]
                    let txe = usart.sr.read().txe().bit();
                    if txe {
                        // TX register empty. Do we need to send something?
                        step_transmit(usart, txref);
                    }

                    sys_irq_control(notifications::USART_IRQ_MASK, true);
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
                    usart.cr1.modify(|_, w| w.txeie().enabled());

                    // We'll do the rest as interrupts arrive.
                    Ok(())
                }
            },
        );
    }
}

fn turn_on_usart() {
    let rcc_driver = RCC.get_task_id();

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 113; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(
        rcc_driver,
        ENABLE_CLOCK,
        pnum.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(
        rcc_driver,
        LEAVE_RESET,
        pnum.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);
}

fn turn_on_gpioa() {
    let rcc_driver = RCC.get_task_id();

    const ENABLE_CLOCK: u16 = 1;

    #[cfg(feature = "stm32f3")]
    let pnum = 17; // see bits in AHBENR
    #[cfg(feature = "stm32f4")]
    let pnum = 0; // see bits in AHB1ENR

    let (code, _) = userlib::sys_send(
        rcc_driver,
        ENABLE_CLOCK,
        pnum.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(
        rcc_driver,
        LEAVE_RESET,
        pnum.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);
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
        usart.cr1.modify(|_, w| w.txeie().disabled());
        state.take().unwrap().caller
    }

    let txs = if let Some(txs) = tx { txs } else { return };

    if let Some(byte) = txs.caller.borrow(0).read_at::<u8>(txs.pos) {
        // Stuff byte into transmitter.
        #[cfg(feature = "stm32f3")]
        usart.tdr.write(|w| w.tdr().bits(u16::from(byte)));
        #[cfg(feature = "stm32f4")]
        usart.dr.write(|w| w.dr().bits(u16::from(byte)));

        txs.pos += 1;
        if txs.pos == txs.len {
            end_transmission(usart, tx).reply(());
        }
    } else {
        end_transmission(usart, tx).reply_fail(ResponseCode::BadArg);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
