//! A driver for the STM32F4 SPI.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//! ## `read` (2)
//!
//! Read into the buffer of lease #0. Returns when completed

#![no_std]
#![no_main]

use stm32f4::stm32f407 as device;
use zerocopy::AsBytes;
use userlib::*;
use abi::LeaseAttributes;

use cortex_m_semihosting::hprintln;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const RCC: Task = SELF;

const OP_WRITE: u32 = 1;
const OP_READ: u32 = 2;

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
    op: u32,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_spi();
    // We also need to modify GPIO as well
    turn_on_gpioa();

    let mut spi = unsafe { &*device::SPI1::ptr() };

    // Most of this is set pretty arbitrarily which is okay for testing
    // XXX Need a better way to captuer this configuration?
    spi.cr1.write(|w| {
            w.cpha()
                .bit(false) // Capture on first transition
                .cpol()
                .bit(false) // Idle low
                .mstr()
                .set_bit() // Master
                .br()
                .bits(0b110) // pCLK/64
                .lsbfirst()
                .clear_bit() // LSB first
                .ssm()
                .set_bit() // Software slave management
                .ssi()
                .set_bit()
                .rxonly()
                .clear_bit() // Allow RX
                .dff()
                .clear_bit() // 8-bit data
                .bidimode()
                .clear_bit() // 2-line unidirectional data
                .spe()
                .set_bit() // actually enable SPI
    });


    // TODO: the fact that we interact with GPIOA directly here is an expedient
    // hack, but control of the GPIOs should probably be centralized somewhere.
    let gpioa = unsafe { &*device::GPIOA::ptr() };

    // Mux the SPI onto the output pins. We're using PA5/6/7, where SPI1 is
    // selected by Alternate Function 5.
    gpioa.moder.modify(|_, w| {
        w.moder5().alternate()
            .moder6().alternate()
            .moder7().alternate()
    });
    gpioa.afrl.modify(|_, w| {
        w.afrl5().af5()
            .afrl6().af5()
            .afrl7().af5()
    });

    // Turn on our interrupt. We haven't enabled any interrupt sources at the
    // SPI side yet, so this won't trigger notifications yet.
    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut tx: Option<Transmit> = None;

    loop {
        let msginfo = sys_recv(&mut [], mask);
        if msginfo.sender == TaskId::KERNEL {
            if msginfo.operation & 1 != 0 {
                if let Some(txs) = tx.as_mut() {
                    // Transmit in progress, check to see if TX is empty.
                    if txs.op == OP_WRITE && spi.sr.read().txe().bit() {
                        if step_transmit(&spi, txs) {
                            tx = None;
                            spi.cr2.modify(|_, w| w.txeie().clear_bit());
                        }

                    } else if txs.op == OP_READ && spi.sr.read().rxne().bit() {
                        if step_receive(&spi, txs) {
                            tx = None;
                            spi.cr2.modify(|_, w| w.rxneie().clear_bit());
                        }
                    } else {
                        hprintln!("Unexpected state {:x} {:x}", txs.op, spi.sr.read().bits());
                        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
                        // XXX Handle more errors
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
                    if rc != 0 || atts & LeaseAttributes::READ.bits() == 0 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    // Okay! Begin a transfer!
                    tx = Some(Transmit {
                        task: msginfo.sender,
                        pos: 0,
                        len,
                        op: OP_WRITE,
                    });

                    // OR the TX register empty signal into the SPI interrupt.
                    spi.cr2.modify(|_, w| w.txeie().set_bit());

                    // We'll do the rest as interrupts arrive.
                },
                OP_READ => {
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
                    if rc != 0 || atts & LeaseAttributes::WRITE.bits() == 0 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    // Okay! Begin a receive!
                    tx = Some(Transmit {
                        task: msginfo.sender,
                        pos: 0,
                        len,
                        op : OP_READ
                    });

                    // Enable the RX ready interrupt.
                    spi.cr2.modify(|_, w| w.rxneie().set_bit());

                    // We'll do the rest as interrupts arrive.
                },

                _ => sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]),
            }
        }
    }
}

fn turn_on_spi() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 140; // see bits in APB2ENR
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

fn step_receive(spi: &device::spi1::RegisterBlock, txs: &mut Transmit) -> bool {
    // Get our byte.
    let byte = spi.dr.read().bits();

    let (rc, len) = sys_borrow_write(txs.task, 0, txs.pos, byte.as_bytes());
    // XXX We're technically a half-word here so this should probably be adjusted
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        true
    } else {
        txs.pos += 1;
        if txs.pos == txs.len {
            sys_reply(txs.task, ResponseCode::Success as u32, &[]);
            true
        } else {
            false
        }
    }
}

fn step_transmit(spi: &device::spi1::RegisterBlock, txs: &mut Transmit) -> bool {
    let mut byte = 0u8;
    let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_bytes_mut());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        true
    } else {
        // Stuff byte into transmitter.
        spi.dr.write(|w| w.dr().bits(u16::from(byte)));

        txs.pos += 1;
        if txs.pos == txs.len {
            sys_reply(txs.task, ResponseCode::Success as u32, &[]);
            true
        } else {
            false
        }
    }
}
