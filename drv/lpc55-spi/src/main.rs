//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! Mostly for demonstration purposes, write is verified read is not
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

use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = Task::anonymous;

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

#[derive(FromPrimitive, PartialEq)]
enum Op {
    Write = 1,
    Read = 2,
}

struct Transmit {
    task: hl::Caller<()>,
    len: usize,
    pos: usize,
    op: Op,
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
    let syscon = Syscon::from(TaskId::for_index_and_gen(
        SYSCON as usize,
        Generation::default(),
    ));

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    muck_with_gpios(&syscon);

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual I2C block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM8::ptr() };

    let spi = unsafe { &*device::SPI8::ptr() };

    // Set SPI mode for Flexcomm
    flexcomm.pselid.write(|w| w.persel().spi());

    // Ensure the block is off
    spi.fifocfg
        .modify(|_, w| w.enabletx().disabled().enablerx().disabled());

    spi.cfg.modify(|_, w| {
        w.enable()
            .disabled()
            .master()
            .slave_mode()
            .lsbf()
            .standard() // MSB first
            .cpha()
            .change() // capture on change
            .cpol()
            .low() // rest state of the clock is low
            .loop_()
            .disabled()
    });

    // we're at 12 Mhz so divide by 12 to get 1Mhz
    // (Also not needed if we're slave)
    spi.div.modify(|_, w| unsafe { w.divval().bits(0xb) });

    // Just trigger the FIFOs to hold 1 item for now
    spi.fifotrig.modify(|_, w| unsafe {
        w.txlvlena()
            .enabled()
            .txlvl()
            .bits(0x0)
            .rxlvlena()
            .enabled()
            .rxlvl()
            .bits(0x0)
    });

    // Now that we've configured and set the clock turn
    // it all on
    spi.fifocfg
        .modify(|_, w| w.enabletx().enabled().enablerx().enabled());

    spi.cfg.modify(|_, w| w.enable().enabled());

    // Field messages.
    let mask = 1;
    let mut tx: Option<Transmit> = None;

    sys_irq_control(1, true);

    loop {
        hl::recv(
            &mut [],
            mask,
            &mut tx,
            |txref, bits| {
                if bits & 1 != 0 {
                    if spi.fifostat.read().txnotfull().bit_is_set() {
                        write_byte(&spi, txref);
                    }
                    if spi.fifostat.read().rxnotempty().bit_is_set() {
                        read_byte(&spi, txref);
                    }
                }
                sys_irq_control(1, true);
            },
            |txref, op, msg| match op {
                Op::Write => {
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

                    *txref = Some(Transmit {
                        task: caller,
                        pos: 0,
                        len: info.len,
                        op: Op::Write,
                    });

                    spi.fifointenset.write(|w| w.txlvl().enabled());

                    Ok(())
                }
                Op::Read => {
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
                    if !info.attributes.contains(LeaseAttributes::WRITE) {
                        return Err(ResponseCode::BadArg);
                    }

                    *txref = Some(Transmit {
                        task: caller,
                        pos: 0,
                        len: info.len,
                        op: Op::Read,
                    });

                    // Yes we need both interrupts at the moment
                    spi.fifointenset.write(|w| w.rxlvl().enabled());
                    spi.fifointenset.write(|w| w.txlvl().enabled());
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
}

fn muck_with_gpios(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    // This is quite the array!
    // HS_SPI_MOSI = P0_26 = FUN9
    // HS_SPI_MISO = P1_3 = FUN6
    // HS_SPI_SCK = P1_2 = FUN6
    // HS_SPI_SSEL0 = P0_20 = FUN8
    // HS_SPI_SSEL1 = P1_1 = FUN5
    // HS_SPI_SSEL2 = P1_12 = FUN5
    // HS_SPI_SSEL3 = P1_26 = FUN5
    //
    // Some of the alt functions aren't defined in the HAL crate
    //
    // All of these need to be in digital mode. The NXP C driver
    // also sets the pull-up resistor
    let iocon = unsafe { &*device::IOCON::ptr() };

    iocon.pio0_26.write(|w| unsafe {
        w.func().bits(0x9).digimode().digital().mode().pull_up()
    });
    iocon
        .pio1_3
        .write(|w| w.func().alt6().digimode().digital().mode().pull_up());
    iocon
        .pio1_2
        .write(|w| w.func().alt6().digimode().digital().mode().pull_up());
    iocon.pio0_20.write(|w| unsafe {
        w.func().bits(0x8).digimode().digital().mode().pull_up()
    });
    iocon
        .pio1_1
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
    iocon
        .pio1_12
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
    iocon
        .pio1_26
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
}

fn write_byte(spi: &device::spi0::RegisterBlock, tx: &mut Option<Transmit>) {
    let txs = if let Some(txs) = tx { txs } else { return };

    if txs.op != Op::Write {
        // This hardware block expects us to send at the same time we're
        // receiving. There is a bit to turn it off but accessing it is
        // not easy. For now just send 0 if we're trying to receive but
        // not actually write
        spi.fifowr
            .write(|w| unsafe { w.len().bits(7).txdata().bits(0x00 as u16) });

        return;
    }

    if let Some(byte) = txs.task.borrow(0).read_at::<u8>(txs.pos) {
        // This SPI hardware in particular really wants everything to be
        // decided at write time in terms of asserting/deasserting CS.
        // I think there's a way to make it 'stick' if you don't write
        // it each time but that may need to eventually be added as
        // part of the write API. We may also need to change how we write
        // to this register depending on what device(s) we end up talking
        // to
        txs.pos += 1;
        spi.fifowr.write(|w| unsafe {
            w.len()
                .bits(7)
                // Don't wait for RX while we're TX (may need to change)
                .rxignore()
                .ignore()
                // Just assert all our CS for now
                .txssel0_n()
                .asserted()
                .txssel1_n()
                .asserted()
                .txssel2_n()
                .asserted()
                .txssel3_n()
                .asserted()
                // Mark the end of the transfer if we're at the end of the buffer
                .eot()
                .bit(txs.pos == txs.len)
                .txdata()
                .bits(byte as u16)
        });
        if txs.pos == txs.len {
            spi.fifointenclr.write(|w| w.txlvl().set_bit());
            core::mem::replace(tx, None).unwrap().task.reply(());
        }
    } else {
        spi.fifointenclr.write(|w| w.txlvl().set_bit());
        core::mem::replace(tx, None)
            .unwrap()
            .task
            .reply_fail(ResponseCode::BadArg);
    }
}

fn read_byte(spi: &device::spi0::RegisterBlock, tx: &mut Option<Transmit>) {
    let txs = if let Some(txs) = tx { txs } else { return };

    if txs.op != Op::Read {
        return;
    }

    // TODO check the CS bits and SOT flag
    let byte: u8 = spi.fiford.read().rxdata().bits() as u8;

    let borrow = txs.task.borrow(0);

    if let Some(_) = borrow.write_at(txs.pos, byte) {
        txs.pos += 1;
        if txs.pos == txs.len {
            spi.fifointenclr.write(|w| w.txlvl().set_bit());
            spi.fifointenclr.write(|w| w.rxlvl().set_bit());
            core::mem::replace(tx, None).unwrap().task.reply(());
        }
    } else {
        spi.fifointenclr.write(|w| w.txlvl().set_bit());
        spi.fifointenclr.write(|w| w.rxlvl().set_bit());
        core::mem::replace(tx, None)
            .unwrap()
            .task
            .reply_fail(ResponseCode::BadArg);
    }
}
