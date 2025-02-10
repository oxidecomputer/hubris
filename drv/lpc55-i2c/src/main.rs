// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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

use drv_lpc55_gpio_api::*;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use userlib::{hl, task_slot, FromPrimitive, LeaseAttributes};

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

#[derive(FromPrimitive)]
enum Op {
    Write = 1,
    Read = 2,
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

struct Transmit {
    addr: u8,
    caller: hl::Caller<()>,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    muck_with_gpios(&syscon);

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual I2C block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM4::ptr() };

    let i2c = unsafe { &*device::I2C4::ptr() };

    // Set I2C mode
    flexcomm.pselid.write(|w| w.persel().i2c());

    // Set up the block
    i2c.cfg.modify(|_, w| w.msten().enabled());

    // Our main clock is 12 Mhz. The HAL crate was making some interesting
    // claims about clocking as well. 100 kbs sounds nice?
    i2c.clkdiv.modify(|_, w| unsafe { w.divval().bits(0x9) });
    i2c.msttime
        .modify(|_, w| w.mstsclhigh().bits(0x4).mstscllow().bits(0x4));

    // Field messages.
    let mut buffer = [0; 1];
    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::Write => {
                let (&addr, caller) = msg
                    .fixed_with_leases::<u8, ()>(1)
                    .ok_or(ResponseCode::BadArg)?;

                let info =
                    caller.borrow(0).info().ok_or(ResponseCode::BadArg)?;
                if !info.attributes.contains(LeaseAttributes::READ) {
                    return Err(ResponseCode::BadArg);
                }

                write_a_buffer(
                    i2c,
                    Transmit {
                        addr,
                        caller,
                        pos: 0,
                        len: info.len,
                    },
                )
            }
            Op::Read => {
                let (&addr, caller) = msg
                    .fixed_with_leases::<u8, ()>(1)
                    .ok_or(ResponseCode::BadArg)?;

                let info =
                    caller.borrow(0).info().ok_or(ResponseCode::BadArg)?;
                if !info.attributes.contains(LeaseAttributes::WRITE) {
                    return Err(ResponseCode::BadArg);
                }

                read_a_buffer(
                    i2c,
                    Transmit {
                        addr,
                        caller,
                        pos: 0,
                        len: info.len,
                    },
                )
            }
        });
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Fc4);
    syscon.leave_reset(Peripheral::Fc4);
}

fn muck_with_gpios(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    // Our GPIOs are P1_21 and P1_21 and need to be set to AF5
    // (see table 320)
    // The existing peripheral API makes doing this via messages
    // maddening so just muck with IOCON manually for now

    let gpio_driver = GPIO.get_task_id();
    let iocon = Pins::from(gpio_driver);

    iocon.iocon_configure(
        Pin::PIO1_21,
        AltFn::Alt5,
        Mode::NoPull,
        Slew::Standard,
        Invert::Disable,
        Digimode::Digital,
        Opendrain::Normal,
        None,
    );

    iocon.iocon_configure(
        Pin::PIO1_20,
        AltFn::Alt5,
        Mode::NoPull,
        Slew::Standard,
        Invert::Disable,
        Digimode::Digital,
        Opendrain::Normal,
        None,
    );
}

fn write_a_buffer(
    i2c: &device::i2c0::RegisterBlock,
    mut txs: Transmit,
) -> Result<(), ResponseCode> {
    // Address to write to
    i2c.mstdat
        .modify(|_, w| unsafe { w.data().bits(txs.addr << 1) });

    // and send it away!
    i2c.mstctl.write(|w| w.mststart().start());

    while i2c.stat.read().mstpending().is_in_progress() {
        continue;
    }

    if !i2c.stat.read().mststate().is_transmit_ready() {
        return Err(ResponseCode::Busy);
    }

    let borrow = txs.caller.borrow(0);

    while txs.pos < txs.len {
        let byte: u8 = borrow.read_at(txs.pos).ok_or(ResponseCode::BadArg)?;
        txs.pos += 1;

        i2c.mstdat.modify(|_, w| unsafe { w.data().bits(byte) });

        i2c.mstctl.write(|w| w.mstcontinue().continue_());

        while i2c.stat.read().mstpending().is_in_progress() {
            continue;
        }

        if !i2c.stat.read().mststate().is_transmit_ready() {
            return Err(ResponseCode::Busy);
        }
    }

    i2c.mstctl.write(|w| w.mststop().stop());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_idle() {
        return Err(ResponseCode::Busy);
    }

    txs.caller.reply(());
    Ok(())
}

fn read_a_buffer(
    i2c: &device::i2c0::RegisterBlock,
    mut txs: Transmit,
) -> Result<(), ResponseCode> {
    i2c.mstdat
        .modify(|_, w| unsafe { w.data().bits((txs.addr << 1) | 1) });

    i2c.mstctl.write(|w| w.mststart().start());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_receive_ready() {
        return Err(ResponseCode::BadArg);
    }

    let borrow = txs.caller.borrow(0);

    while txs.pos < txs.len - 1 {
        let byte = i2c.mstdat.read().data().bits();
        borrow.write_at(txs.pos, byte).ok_or(ResponseCode::BadArg)?;

        i2c.mstctl.write(|w| w.mstcontinue().continue_());

        while i2c.stat.read().mstpending().is_in_progress() {}

        if !i2c.stat.read().mststate().is_receive_ready() {
            return Err(ResponseCode::BadArg);
        }

        txs.pos += 1;
    }

    let byte = i2c.mstdat.read().data().bits();
    borrow.write_at(txs.pos, byte).ok_or(ResponseCode::BadArg)?;

    i2c.mstctl.write(|w| w.mststop().stop());

    while i2c.stat.read().mstpending().is_in_progress() {}

    if !i2c.stat.read().mststate().is_idle() {
        return Err(ResponseCode::BadArg);
    }

    txs.caller.reply(());
    Ok(())
}
