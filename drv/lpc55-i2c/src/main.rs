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

use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = Task::anonymous;

#[derive(FromPrimitive, PartialEq)]
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
    op: Op,
    caller: hl::Caller<()>,
    state: TransmitState,
    addr: u8,
    len: usize,
    pos: usize,
}

#[derive(PartialEq, Debug)]
enum TransmitState {
    Starting,
    Transmitting,
    Stopping,
    ClearingInterrupt,
    Done,
}

impl Transmit {
    fn step(
        &mut self,
        i2c: &device::i2c0::RegisterBlock,
    ) -> Result<(), ResponseCode> {
        match self.state {
            TransmitState::Starting => {
                if !i2c.stat.read().mststate().is_idle() {
                    return Err(ResponseCode::Busy);
                }

                let mut addr = self.addr << 1;

                // If we're reading, we need to set this bit. Otherwise, we're
                // writing.
                if self.op == Op::Read {
                    addr |= 1;
                }

                i2c.mstdat.modify(|_, w| unsafe { w.data().bits(addr) });
                i2c.mstctl.write(|w| w.mststart().start());

                self.state = TransmitState::Transmitting;
            }
            TransmitState::Transmitting => {
                // are we done trasmitting bytes?
                if self.pos == self.len {
                    self.state = TransmitState::Stopping;
                    return Ok(());
                }

                let ready = match self.op {
                    Op::Write => i2c.stat.read().mststate().is_transmit_ready(),
                    Op::Read => i2c.stat.read().mststate().is_receive_ready(),
                };

                if !ready {
                    return Err(ResponseCode::Busy);
                }

                let borrow = self.caller.borrow(0);
                match self.op {
                    Op::Write => {
                        let byte: u8 = borrow
                            .read_at(self.pos)
                            .ok_or(ResponseCode::BadArg)?;

                        i2c.mstdat
                            .modify(|_, w| unsafe { w.data().bits(byte) });
                    }
                    Op::Read => {
                        let byte = i2c.mstdat.read().data().bits();
                        borrow
                            .write_at(self.pos, byte)
                            .ok_or(ResponseCode::BadArg)?;
                    }
                }

                i2c.mstctl.write(|w| w.mstcontinue().continue_());

                self.pos += 1;
            }
            TransmitState::Stopping => {
                let ready = match self.op {
                    Op::Write => i2c.stat.read().mststate().is_transmit_ready(),
                    Op::Read => i2c.stat.read().mststate().is_receive_ready(),
                };

                if !ready {
                    return Err(ResponseCode::Busy);
                }

                // time to stop!
                i2c.mstctl.write(|w| w.mststop().stop());

                self.state = TransmitState::ClearingInterrupt;
            }
            TransmitState::ClearingInterrupt => {
                if !i2c.stat.read().mststate().is_idle() {
                    return Err(ResponseCode::Busy);
                }

                // now that we're done, turn off the interrupt
                i2c.intenclr.write(|w| w.mstpendingclr().set_bit());

                self.state = TransmitState::Done;
            }
            TransmitState::Done => {
                // If we're done, then we're done. No need to do anything else.
            }
        }

        Ok(())
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

    // turn on interrupts
    sys_irq_control(1, true);

    // Field messages.
    let mut buffer = [0; 1];

    let mask = 1;

    let mut transmission: Option<Transmit> = None;

    loop {
        hl::recv(
            &mut buffer,
            mask,
            &mut transmission,
            |transmission, notification| {
                if notification & 1 != 0 {
                    // Okay so... This take is really annoying. Reply consumes
                    // caller, and so if we don't use take, we can't call it.
                    // But that means we have to put it back whenever we don't
                    // actually want to use it, which is annoying and
                    // error-prone. But I haven't come up with a cleaner way,
                    // so for now, here we are.
                    if let Some(mut txs) = transmission.take() {
                        // check mstpending
                        if !i2c.stat.read().mstpending().is_pending() {
                            //spurious, put it back
                            *transmission = Some(txs);
                        } else {
                            txs.step(&i2c).unwrap_or_else(|code| {
                                sys_reply(
                                    txs.caller.task_id(),
                                    code as u32,
                                    &[],
                                )
                            });

                            if txs.state == TransmitState::Done {
                                txs.caller.reply(());
                            } else {
                                *transmission = Some(txs);
                            }
                        }
                    }

                    // re-enable interrupts
                    sys_irq_control(1, true);
                }
            },
            |transmission, op, msg| {
                let (&addr, caller) =
                    msg.fixed_with_leases(1).ok_or(ResponseCode::BadArg)?;

                let info =
                    caller.borrow(0).info().ok_or(ResponseCode::BadArg)?;

                // if we want to read, we need to write into our buffer,
                // and if we want to write, we need to read from our buffer
                let attr = match op {
                    Op::Read => LeaseAttributes::WRITE,
                    Op::Write => LeaseAttributes::READ,
                };

                if !info.attributes.contains(attr) {
                    return Err(ResponseCode::BadArg);
                }

                // Deny incoming writes if we're already running one.
                if transmission.is_some() {
                    return Err(ResponseCode::Busy);
                }

                // stash this away for the interrupt handler
                *transmission = Some(Transmit {
                    op,
                    addr,
                    caller,
                    pos: 0,
                    len: info.len,
                    state: TransmitState::Starting,
                });

                // enable the interrupt
                i2c.intenset.write(|w| w.mstpendingen().enabled());

                Ok(())
            },
        )
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
    let iocon = unsafe { &*device::IOCON::ptr() };
    iocon
        .pio1_21
        .write(|w| w.func().alt5().digimode().digital());
    iocon
        .pio1_20
        .write(|w| w.func().alt5().digimode().digital());
}
