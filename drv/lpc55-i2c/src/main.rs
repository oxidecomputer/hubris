//! A driver for the LPC55 i2c chip.
//!
//! TODO This currently blocks and should really become interrupt driven
//! before it actually gets used.
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//!
//! ## `read` (2)
//!
//! Reads the buffer into lease #0. Returns when completed

#![no_std]
#![no_main]

use lpc55_pac as device;
use zerocopy::AsBytes;
use userlib::*;

use core::convert::TryInto;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

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
    addr: u8,
    task: TaskId,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm();

    muck_with_gpios();

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual I2C block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here 

    let flexcomm = unsafe { &*device::FLEXCOMM4::ptr() };

    let i2c = unsafe { &*device::I2C4::ptr() };

    // Set I2C mode
    flexcomm.pselid.write( |w| w.persel().i2c() );

    // Set up the block
    i2c.cfg.modify(|_, w| w.msten().enabled() );

    // Our main clock is 12 Mhz. The HAL crate was making some interesting
    // claims about clocking as well. 100 kbs sounds nice?
    i2c.clkdiv.modify(|_, w| unsafe { w.divval().bits(0x9) } );
    i2c.msttime.modify(|_, w| w
            .mstsclhigh().bits(0x4)
            .mstscllow().bits(0x4)
    );

    // Field messages.
    let mask = 1;

    let mut buffer = 0u32;
    loop {
        let msginfo = sys_recv(buffer.as_bytes_mut(), mask);
        if msginfo.sender == TaskId::KERNEL {
            cortex_m_semihosting::hprintln!("Unexpected kernel message?").ok();
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
                        addr: buffer.try_into().unwrap(),
                        task: msginfo.sender,
                        pos: 0,
                        len
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
                        addr: buffer.try_into().unwrap(),
                        task: msginfo.sender,
                        pos: 0,
                        len
                    });

                }
                _ => sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]),
            }
        }
    }
}

fn turn_on_flexcomm() {
    let rcc_driver = TaskId::for_index_and_gen(SYSCON as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 47; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn muck_with_gpios()
{
    let rcc_driver = TaskId::for_index_and_gen(SYSCON as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 13; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    // Our GPIOs are P1_21 and P1_21 and need to be set to AF5
    // (see table 320)
    // The existing peripheral API makes doing this via messages
    // maddening so just muck with IOCON manually for now
    let iocon = unsafe  { &*device::IOCON::ptr() };
    iocon.pio1_21.write( |w| w.func().alt5().
                digimode().digital() );
    iocon.pio1_20.write( |w| w.func().alt5().
                digimode().digital() );
}


fn write_a_buffer(i2c: &device::i2c0::RegisterBlock, txs: &mut Transmit) -> bool {

    // Address to write to
    i2c.mstdat.modify(|_, w| unsafe { w.data().bits(txs.addr << 1) } );

    // and send it away!
    i2c.mstctl.write(|w| w.mststart().start());

    while i2c.stat.read().mstpending().is_in_progress() { continue; }

    if !i2c.stat.read().mststate().is_transmit_ready() {
        sys_reply(txs.task, ResponseCode::Busy as u32, &[]);
        return false;
    }

    while txs.pos < txs.len {
        let mut byte = 0u8;
        let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_bytes_mut());
        if rc != 0 || len != 1 {
            sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
            return false;
        }
        txs.pos += 1;

        i2c.mstdat.modify(|_, w| unsafe { w.data().bits(byte) } );

        i2c.mstctl.write(|w| w.mstcontinue().continue_());

        while i2c.stat.read().mstpending().is_in_progress() { continue; }

        if ! i2c.stat.read().mststate().is_transmit_ready() {
            sys_reply(txs.task, ResponseCode::Busy as u32, &[]);
            return false;
        }
    }

    i2c.mstctl.write(|w| w.mststop().stop());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_idle() {
        sys_reply(txs.task, ResponseCode::Busy as u32, &[]);
        return false;
    }

    sys_reply(txs.task, ResponseCode::Success as u32, &[]);
    return true;
}

fn read_a_buffer(i2c: &device::i2c0::RegisterBlock, txs: &mut Transmit) -> bool {

    i2c.mstdat.modify(|_, w| unsafe { w.data().bits((txs.addr << 1) | 1) } );

    i2c.mstctl.write(|w| w.mststart().start());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_receive_ready() {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        return false;
    }

    while txs.pos < txs.len - 1 {
        let mut byte = i2c.mstdat.read().data().bits();

        let (rc, len) = sys_borrow_write(txs.task, 0, txs.pos, byte.as_bytes_mut());
        if rc != 0 || len != 1 {
            sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
            return false;
        }

        i2c.mstctl.write(|w| w.mstcontinue().continue_());

        while i2c.stat.read().mstpending().is_in_progress() {}

        if !i2c.stat.read().mststate().is_receive_ready() {
            sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
            return false;
        }

        txs.pos += 1;
    }

    let mut byte = i2c.mstdat.read().data().bits();

    let (rc, len) = sys_borrow_write(txs.task, 0, txs.pos, byte.as_bytes_mut());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        return false;
    }


    i2c.mstctl.write(|w| w.mststop().stop());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_idle() {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        return false;
    }

    sys_reply(txs.task, ResponseCode::Success as u32, &[]);
    return true;
}
