//! A driver for the STM32F4 I2C
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Takes the 7-bit i2c address as an argument.
//!
//! ## `read` (2)
//!
//! Read into the contents to lease #1. Takes the 7-bit i2c address as an
//! argument. Note the address is automatically offset by 1 for reading so there
//! is no need to add 1.
//!
//! Sample write and read to device at address 0x4a. 0x1 is the command to get
//! the ID
//!
//!         let addr : &[u8]= &[0x1];
//!         let mut recv : [u8; 4] = [0; 4];
//!         let a : &mut [u8] = &mut recv;
//!         let (code, _) = sys_send(i2c, 1, &[0x4a], &mut [], &[Lease::from(addr)]);
//!         if code != 0 {
//!             hprintln!("Got error code{}", code);
//!         } else {
//!             hprintln!("Success");
//!         }
//!         let (code, _) = sys_send(i2c, 2, &[0x4a], &mut [], &[Lease::from(a)]);
//!         if code != 0 {
//!             hprintln!("Got error code{}", code);
//!         } else {
//!             hprintln!("Got buffer {:x?}", recv[0]);
//!         }
//!
//!

#![no_std]
#![no_main]

use stm32f4::stm32f407 as device;
use zerocopy::AsBytes;
use userlib::*;
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
    addr: u32,
    task: TaskId,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c();
    turn_on_gpiob();

    // From thin air, pluck a pointer to the I2C register block.
    //
    // Safety: this is needlessly unsafe in the API. The I2C is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let i2c = unsafe { &*device::I2C1::ptr() };


    // TODO: It's time to write a proper GPIO driver to handle this
    let gpiob = unsafe { &*device::GPIOB::ptr() };

    // We're using PB6/9, where I2C1 is selected by Alternate Function 4
    gpiob.pupdr.modify(|_, w| {
        w.pupdr6().pull_down()
            .pupdr9().pull_down()
    });

    gpiob.moder.modify(|_, w| {
        w.moder6().alternate()
            .moder9().alternate()
    });


    gpiob.afrl.modify(|_, w| {
            w.afrl6().af4()
    });
    gpiob.afrh.modify(|_, w| {
            w.afrh9().af4()
   });

    gpiob.otyper.modify(|_, w| {
        w.ot6().open_drain()
        .ot9().open_drain()
    });


    // Make sure the I2C unit is disabled so we can configure it
    i2c.cr1.modify(|_, w| w.pe().clear_bit());

    // We're assuming everything is set up to 48mhz based on how
    // we set up the clock driver
    i2c.cr2.write(|w| unsafe { w.freq().bits(24) });

    i2c.trise.write(|w| w.trise().bits(8));

    i2c.ccr.write(|w| unsafe {
        w.f_s().set_bit().duty().clear_bit().ccr().bits(20000 as u16)
    });

    // Actually turn on the hardware
    i2c.cr1.modify(|_, w| w.pe().set_bit());

    // Field messages.
    let mask = 1;

    let mut buffer = 0u32;
    loop {
        let msginfo = sys_recv(buffer.as_bytes_mut(), mask);
        if msginfo.sender == TaskId::KERNEL {
            hprintln!("Unexpected kernel message?");
        } else {
            match msginfo.operation {
                OP_WRITE => {
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


                    write_a_buffer(&i2c, &mut Transmit {
                        addr: buffer,
                        task: msginfo.sender,
                        pos: 0,
                        len,
                    });
                },
                OP_READ => {
                    // Check the lease count and characteristics.
                    if msginfo.lease_count != 1 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    let (rc, atts, len) = sys_borrow_info(msginfo.sender, 0);
                    if rc != 0 || atts & 2 == 0 {
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }

                    read_a_buffer(&i2c, &mut Transmit {
                        addr: buffer,
                        task: msginfo.sender,
                        pos: 0,
                        len,
                    });
                },
                _ => sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]),
            }
        }
    }
}

fn turn_on_i2c() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 117; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn turn_on_gpiob() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 1; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn turn_on_gpiod() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 3; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn write_a_buffer(i2c: &device::i2c1::RegisterBlock, txs: &mut Transmit) -> bool {
    // Send a START condition
    i2c.cr1.modify(|_, w| w.start().set_bit());

    // Wait until START condition was generated
    while i2c.sr1.read().sb().bit_is_clear() {}

    // Also wait until signalled we're master and everything is waiting for us
    while {
        let sr2 = i2c.sr2.read();
            sr2.msl().bit_is_clear() && sr2.busy().bit_is_clear()
    } {}

    // Set up current address, we're trying to talk to
    i2c.dr.write(|w| unsafe { w.bits(u32::from(txs.addr) << 1) });

    // Wait until address was sent
    while i2c.sr1.read().addr().bit_is_clear() {}

    // Clear condition by reading SR2
    i2c.sr2.read();

    // Send bytes
    while txs.pos < txs.len {
        let mut byte = 0u8;
        let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_bytes_mut());
        if rc != 0 || len != 1 {
            sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
            return false;
        }
        txs.pos += 1;

        while i2c.sr1.read().tx_e().bit_is_clear() {}

        i2c.dr.write(|w| unsafe { w.bits(u32::from(byte)) });

        // Wait until byte is transferred
        while {
            let sr1 = i2c.sr1.read();

            // If we received a NACK, then this is an error
            if sr1.af().bit_is_set() {
                sys_reply(txs.task, ResponseCode::Busy as u32, &[]);
                return false;
            }

            sr1.btf().bit_is_clear()
        } {}
    }
    sys_reply(txs.task, ResponseCode::Success as u32, &[]);
    return true;
}

fn read_a_buffer(i2c: &device::i2c1::RegisterBlock, txs: &mut Transmit) -> bool {
    // Send a START condition
    i2c.cr1.modify(|_, w| w.start().set_bit().ack().set_bit());

    // Wait until START condition was generated
    while i2c.sr1.read().sb().bit_is_clear() {}

    // Also wait until signalled we're master and everything is waiting for us
    while {
        let sr2 = i2c.sr2.read();
            sr2.msl().bit_is_clear() && sr2.busy().bit_is_clear()
    } {}

    // Set up current address, we're trying to talk to
    i2c.dr.write(|w| unsafe { w.bits((u32::from(txs.addr) << 1) + 1) });

    // Wait until address was sent
    while i2c.sr1.read().addr().bit_is_clear() {}

    // Clear condition by reading SR2
    i2c.sr2.read();

    // Send bytes
    while txs.pos < txs.len - 1 {
        while i2c.sr1.read().rx_ne().bit_is_clear() {}
        let mut value = i2c.dr.read().bits() as u8;

        let (rc, len) = sys_borrow_write(txs.task, 0, txs.pos, value.as_bytes_mut());
        if rc != 0 || len != 1 {
            sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
            return false;
        }
        txs.pos += 1;
    }

    i2c.cr1.modify(|_, w| w.ack().clear_bit().stop().set_bit());

    while i2c.sr1.read().rx_ne().bit_is_clear() {}
    let mut value = i2c.dr.read().bits() as u8;

    let (rc, len) = sys_borrow_write(txs.task, 0, txs.pos, value.as_bytes_mut());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        return false;
    }
   

    sys_reply(txs.task, ResponseCode::Success as u32, &[]);
    return true;
}

