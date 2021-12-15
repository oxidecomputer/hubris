// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! Mostly for demonstration purposes, write is verified read is not
//!
//! # IPC protocol
//!
//! ## `read` (1)
//!
//! Reads the buffer into lease #0. Returns when completed
//!
//!
//! ## `write` (2)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//! ## `exchange` (3)
//!
//! Sends the contents of lease #0 and writes received data into lease #1

#![no_std]
#![no_main]

use drv_lpc55_gpio_api::*;
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use userlib::*;

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

// Read/Write is defined from the perspective of the SPI device
#[derive(FromPrimitive, PartialEq)]
enum Op {
    Read = 1,
    Write = 2,
    Exchange = 3,
}

struct Transmit {
    task: hl::Caller<()>,
    len: usize,
    rpos: usize,
    rlease_num: usize,
    wpos: usize,
    wlease_num: usize,
    op: Op,
}

struct SpiDat<'a> {
    spi: &'a mut spi_core::Spi,
    dat: Option<Transmit>,
}

// TODO: it is super unfortunate to have to write this by hand, but deriving
// ToPrimitive makes us check at runtime whether the value fits
impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    muck_with_gpios(&syscon);

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual SPI block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM8::ptr() };

    let registers = unsafe { &*device::SPI8::ptr() };

    let mut spi = spi_core::Spi::from(registers);

    // Set SPI mode for Flexcomm
    flexcomm.pselid.write(|w| w.persel().spi());

    // This should correspond to SPI mode 0
    spi.initialize(
        device::spi0::cfg::MASTER_A::SLAVE_MODE,
        device::spi0::cfg::LSBF_A::STANDARD, // MSB First
        device::spi0::cfg::CPHA_A::CHANGE,
        device::spi0::cfg::CPOL_A::LOW,
        spi_core::TxLvl::TxEmpty,
        spi_core::RxLvl::Rx1Item,
    );

    spi.enable();

    // Field messages.
    let mask = 1;

    let mut dat = SpiDat {
        spi: &mut spi,
        dat: None,
    };

    sys_irq_control(1, true);

    loop {
        hl::recv(
            &mut [],
            mask,
            &mut dat,
            |datref, bits| {
                if bits & 1 != 0 {
                    if datref.spi.can_tx() {
                        write_byte(datref.spi, &mut datref.dat);
                    }

                    if datref.spi.has_byte() {
                        read_byte(datref.spi, &mut datref.dat);
                    }

                    if let Some(txs) = &datref.dat {
                        if txs.rpos == txs.len && txs.wpos == txs.len {
                            if txs.op == Op::Read {
                                datref.spi.disable_tx();
                            }
                            core::mem::replace(&mut datref.dat, None)
                                .unwrap()
                                .task
                                .reply(());
                        }
                    }
                }
                sys_irq_control(1, true);
            },
            |datref, op, msg| match op {
                Op::Write => {
                    let ((), caller) =
                        msg.fixed_with_leases(1).ok_or(ResponseCode::BadArg)?;

                    // Deny incoming transfers if we're already running one.
                    if datref.dat.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let borrow = caller.borrow(0);

                    let borrow_info =
                        borrow.info().ok_or(ResponseCode::BadArg)?;

                    if !borrow_info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    datref.dat = Some(Transmit {
                        task: caller,
                        rpos: borrow_info.len,
                        wpos: 0,
                        len: borrow_info.len,
                        op: Op::Write,
                        rlease_num: 0,
                        wlease_num: 0,
                    });

                    datref.spi.enable_tx();

                    Ok(())
                }
                Op::Read => {
                    let ((), caller) =
                        msg.fixed_with_leases(1).ok_or(ResponseCode::BadArg)?;

                    if datref.dat.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let borrow = caller.borrow(0);
                    let borrow_info =
                        borrow.info().ok_or(ResponseCode::BadArg)?;
                    if !borrow_info.attributes.contains(LeaseAttributes::WRITE)
                    {
                        return Err(ResponseCode::BadArg);
                    }

                    datref.dat = Some(Transmit {
                        task: caller,
                        rpos: borrow_info.len,
                        wpos: 0,
                        len: borrow_info.len,
                        op: Op::Read,
                        rlease_num: 0,
                        wlease_num: 0,
                    });

                    // Turning off receive without send is difficult (requires a
                    // 16 bit write to a particular register) so just send some
                    // bogus data for now
                    datref.spi.enable_tx();
                    datref.spi.enable_rx();

                    Ok(())
                }
                Op::Exchange => {
                    let ((), caller) =
                        msg.fixed_with_leases(2).ok_or(ResponseCode::BadArg)?;

                    if datref.dat.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let borrow_send = caller.borrow(0);
                    let send_info =
                        borrow_send.info().ok_or(ResponseCode::BadArg)?;
                    if !send_info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    let borrow_recv = caller.borrow(1);
                    let recv_info =
                        borrow_recv.info().ok_or(ResponseCode::BadArg)?;
                    if !recv_info.attributes.contains(LeaseAttributes::WRITE) {
                        return Err(ResponseCode::BadArg);
                    }

                    if recv_info.len != send_info.len {
                        return Err(ResponseCode::BadArg);
                    }

                    datref.dat = Some(Transmit {
                        task: caller,
                        rpos: 0,
                        rlease_num: 0,
                        wpos: 0,
                        wlease_num: 1,
                        len: recv_info.len,
                        op: Op::Exchange,
                    });

                    datref.spi.enable_tx();
                    datref.spi.enable_rx();

                    Ok(())
                }
            },
        );
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi);
    syscon.leave_reset(Peripheral::HsLspi);

    syscon.enable_clock(Peripheral::Fc3);
    syscon.leave_reset(Peripheral::Fc3);
}

fn muck_with_gpios(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    let gpio_driver = GPIO.get_task_id();
    let iocon = Gpio::from(gpio_driver);

    // This is quite the array!
    // All of these need to be in digital mode. The NXP C driver
    // also sets the pull-up resistor

    let pin_settings = [
        // HS_SPI_MOSI = P0_26 = FUN9
        (
            Pin::PIO0_26,
            AltFn::Alt9,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_MISO = P1_3 = FUN6
        (
            Pin::PIO1_3,
            AltFn::Alt6,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_SCK = P1_2 = FUN6
        (
            Pin::PIO1_2,
            AltFn::Alt6,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_SSEL1 = P1_1 = FUN5
        // Note that SSEL0, SSEL2 and SSEL3 are not used in the current design
        (
            Pin::PIO1_1,
            AltFn::Alt5,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
    ];

    for (pin, alt, mode, slew, invert, digi, od) in
        core::array::IntoIter::new(pin_settings)
    {
        iocon
            .iocon_configure(pin, alt, mode, slew, invert, digi, od)
            .unwrap();
    }
}

fn write_byte(spi: &mut spi_core::Spi, tx: &mut Option<Transmit>) {
    let txs = if let Some(txs) = tx { txs } else { return };

    if txs.op == Op::Read {
        // This hardware block expects us to send at the same time we're
        // receiving. There is a bit to turn it off but accessing it is
        // not easy. For now just send 0 if we're trying to receive but
        // not actually write
        spi.send_u8(0x0);
        return;
    }

    if txs.rpos == txs.len {
        return;
    }

    if let Some(byte) = txs.task.borrow(txs.rlease_num).read_at::<u8>(txs.rpos)
    {
        txs.rpos += 1;
        spi.send_u8(byte);
        if txs.rpos == txs.len {
            spi.disable_tx();
        }
    } else {
        spi.disable_tx();
        spi.disable_rx();
        core::mem::replace(tx, None)
            .unwrap()
            .task
            .reply_fail(ResponseCode::BadArg);
    }
}

fn read_byte(spi: &mut spi_core::Spi, tx: &mut Option<Transmit>) {
    let txs = if let Some(txs) = tx { txs } else { return };

    if txs.wpos == txs.len {
        // This might actually be an error because we've received another
        // byte when we have no room?
        return;
    }

    let byte = spi.read_u8();

    let borrow = txs.task.borrow(txs.wlease_num);

    if let Some(_) = borrow.write_at(txs.wpos, byte) {
        txs.wpos += 1;
        if txs.wpos == txs.len {
            spi.disable_rx();
        }
    } else {
        spi.disable_rx();
        spi.disable_tx();
        core::mem::replace(tx, None)
            .unwrap()
            .task
            .reply_fail(ResponseCode::BadArg);
    }
}
