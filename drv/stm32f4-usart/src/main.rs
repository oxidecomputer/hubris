//! A driver for the STM32F4 U(S)ART.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.

#![no_std]
#![no_main]

use stm32f4::stm32f407 as device;
use zerocopy::AsBytes;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const RCC: Task = SELF;

const OP_WRITE: u32 = 1;

#[repr(u32)]
enum ResponseCode {
    Success = 0,
    BadOp = 1,
    BadArg = 2,
    Busy = 3,
}

struct Transmit {
    task: TaskId,
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
    const CLOCK_HZ: u32 = 16_000_000;
    const BAUDRATE: u32 = 115_200;
    const CYCLES_PER_BIT: u32 = (CLOCK_HZ + (BAUDRATE / 2)) / BAUDRATE;
    usart.brr.write(|w| w.div_mantissa().bits((CYCLES_PER_BIT >> 4) as u16)
        .div_fraction().bits(CYCLES_PER_BIT as u8 & 0xF));

    // Enable the transmitter.
    usart.cr1.modify(|_, w| w.te().enabled());

    turn_on_gpioa();

    // TODO: the fact that we interact with GPIOA directly here is an expedient
    // hack, but control of the GPIOs should probably be centralized somewhere.
    let gpioa = unsafe { &*device::GPIOA::ptr() };

    // Mux the USART onto the output pins. We're using PA2/3, where USART2 is
    // selected by Alternate Function 7.
    gpioa.moder.modify(|_, w| {
        w.moder2().alternate()
            .moder3().alternate()
    });
    gpioa.afrl.modify(|_, w| {
        w.afrl2().af7()
            .afrl3().af7()
    });

    // Turn on our interrupt. We haven't enabled any interrupt sources at the
    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut tx: Option<Transmit> = None;

    loop {
        let msginfo = sys_recv(&mut [], mask);
        if msginfo.sender == TaskId::KERNEL {
            if msginfo.operation & 1 != 0 {
                // Handling an interrupt. To allow for spurious interrupts,
                // check the individual conditions we care about, and
                // unconditionally re-enable the IRQ at the end of the handler.
                if let Some(txs) = tx.as_mut() {
                    // Transmit in progress, check to see if TX is empty.
                    if usart.sr.read().txe().bit() {
                        // TX register empty. Time to send something.
                        if step_transmit(&usart, txs) {
                            tx = None;
                            usart.cr1.modify(|_, w| w.txeie().disabled());
                        }
                    }
                }

                sys_irq_control(1, true);
            }
        } else {
            match msginfo.operation {
                OP_WRITE => {
                    // Deny incoming writes if we're already running one.
                    if tx.is_some() {
                        sys_reply(msginfo.sender, ResponseCode::Busy as u32, &[]);
                        continue;
                    }

                    // Check the lease count and characteristics.
                    if msginfo.lease_count != 1 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    let (rc, atts, len) = sys_borrow_info(msginfo.sender, 0);
                    if rc != 0 || atts & 1 == 0 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    // Okay! Begin a transfer!
                    tx = Some(Transmit {
                        task: msginfo.sender,
                        pos: 0,
                        len,
                    });

                    // OR the TX register empty signal into the USART interrupt.
                    usart.cr1.modify(|_, w| w.txeie().enabled());

                    // We'll do the rest as interrupts arrive.
                },
                _ => sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]),
            }
        }
    }
}

fn turn_on_usart() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 113; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn turn_on_gpioa() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 0; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn step_transmit(usart: &device::usart1::RegisterBlock, txs: &mut Transmit) -> bool {
    let mut byte = 0u8;
    let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_bytes_mut());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        true
    } else {
        // Stuff byte into transmitter.
        usart.dr.write(|w| w.dr().bits(u16::from(byte)));

        txs.pos += 1;
        if txs.pos == txs.len {
            sys_reply(txs.task, ResponseCode::Success as u32, &[]);
            true
        } else {
            false
        }
    }
}
