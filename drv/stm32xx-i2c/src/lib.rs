// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32 I2C interface found in a variety of parts,
//! including (for our purposes) the H7 and G0.

#![no_std]

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[cfg(feature = "g031")]
use stm32g0::stm32g031 as device;

#[cfg(feature = "g030")]
use stm32g0::stm32g030 as device;

#[cfg(any(
    feature = "h743",
    feature = "h753",
    feature = "g031",
    feature = "g030"
))]
pub type RegisterBlock = device::i2c1::RegisterBlock;

#[cfg(any(
    feature = "h743",
    feature = "h753",
    feature = "g031",
    feature = "g030"
))]
pub type Isr = device::i2c1::isr::R;

pub mod ltc4306;
pub mod max7358;
pub mod oximux16;
pub mod pca9545;
pub mod pca9548;

use ringbuf::*;
use userlib::*;

use drv_stm32xx_sys_api as sys_api;

pub struct I2cPins {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::PortIndex,
    pub scl: sys_api::PinSet,
    pub sda: sys_api::PinSet,
    pub function: sys_api::Alternate,
}

/// Single GPIO pin, which is never dynamically remapped
pub struct I2cGpio {
    pub gpio_pins: sys_api::PinSet,
}

pub struct I2cController<'a> {
    pub controller: drv_i2c_api::Controller,
    pub peripheral: sys_api::Peripheral,
    pub notification: u32,
    pub registers: &'a RegisterBlock,
}

pub struct I2cTargetControl {
    pub enable: fn(u32),
    pub wfi: fn(u32),
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum I2cKonamiCode {
    Read,
    Write,
}

///
/// A trait to express an I2C mux driver.
///
pub trait I2cMuxDriver {
    /// Configure the mux, specifying the mux and controller, but also an
    /// instance to a [`Gpio`] task.
    fn configure(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Reset the mux
    fn reset(
        &self,
        mux: &I2cMux<'_>,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Enable the specified segment on the specified mux (or disable
    /// all segments if None is explicitly specified as the segment)
    fn enable_segment(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        segment: Option<drv_i2c_api::Segment>,
    ) -> Result<(), drv_i2c_api::ResponseCode>;
}

pub struct I2cMux<'a> {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::PortIndex,
    pub id: drv_i2c_api::Mux,
    pub driver: &'a dyn I2cMuxDriver,

    /// Optional enable / reset line
    ///
    /// When this is high, the chip is enabled; when it is low, the chip is held
    /// in reset. On the LTC4306, this is an active-high ENABLE; on the PCA954x,
    /// it's an active-low RESET.
    pub nreset: Option<I2cGpio>,
    pub address: u8,
}

///
/// An enum describing the amount to read
///
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReadLength {
    /// Fixed length to read
    Fixed(usize),
    /// Read size is variable: first byte contains length
    Variable,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Copy, Clone, Eq, PartialEq)]
enum Register {
    CR1,
    CR2,
    ISR,
}

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    Wait(Register, u32),
    Write(Register, u32),
    WriteWait(Register, u32),
    Read(Register, u32),
    ReadWait(Register, u32),
    KonamiOperation(I2cKonamiCode),
    Konami(Register, u32),
    Reset(Register, u32),
    Addr(Register, u32),
    AddrMatch,
    AddrNack(u8),
    RxReg(Register, u32),
    Rx(u8, u8),
    RxNack(u8, u8),
    Tx(u8, u8),
    TxBogus(u8),
    TxOverrun(u8),
    TxReg(Register, u32),
    WaitAddr,
    WaitRx,
    WaitTx,
    BusySleep,
    Stop,
    RepeatedStart(#[count(children)] bool),
}

counted_ringbuf!(Trace, 48, Trace::None);

impl I2cMux<'_> {
    /// A convenience routine to translate an error induced by in-band
    /// management into one that can be returned to a caller
    fn error_code(
        &self,
        code: drv_i2c_api::ResponseCode,
    ) -> drv_i2c_api::ResponseCode {
        use drv_i2c_api::ResponseCode;

        match code {
            ResponseCode::NoDevice => ResponseCode::MuxMissing,
            ResponseCode::NoRegister => ResponseCode::BadMuxRegister,
            ResponseCode::BusLocked => ResponseCode::BusLockedMux,
            ResponseCode::BusReset => ResponseCode::BusResetMux,
            _ => code,
        }
    }

    fn configure(
        &self,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        if let Some(pin) = &self.nreset {
            // Set the pins to high _before_ switching to output to avoid
            // glitching.
            sys.gpio_set(pin.gpio_pins);
            // Now, expose them as outputs.
            sys.gpio_configure_output(
                pin.gpio_pins,
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
            );
        }

        Ok(())
    }

    fn reset(
        &self,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        if let Some(pin) = &self.nreset {
            sys.gpio_reset(pin.gpio_pins);
            sys.gpio_set(pin.gpio_pins);
        }

        Ok(())
    }
}

impl I2cController<'_> {
    pub fn enable(&self, sys: &sys_api::Sys) {
        sys.enable_clock(self.peripheral);
        sys.leave_reset(self.peripheral);
    }

    fn configure_timing(&self, i2c: &RegisterBlock) {
        // TODO: this configuration mechanism is getting increasingly hairy. It
        // generally assumes that a given processor runs at a given speed on all
        // boards, which is not at all true. As of recently it's now doing a
        // hybrid of "CPU model" and "board name" sensing. Should move to
        // configuration!
        cfg_if::cfg_if! {
            if #[cfg(any(feature = "h743", feature = "h753"))] {
                cfg_if::cfg_if! {
                    // Due to AMD Milan erratum 1394, the processor needs an
                    // abnormally long data setup time from an I2C target
                    // sending an ACK. (According to the erratum,
                    // "[u]nexpected collisions may be observed on the SMBUS
                    // if Data Setup Time is less than 500 ns.")  In practice,
                    // this means that the delta between SDA being pulled down
                    // by an acknowledging target and the rising edge of SCL
                    // should be 500 ns.  This can be achieved by the target
                    // holding SCL down after pulling down SDA, which in turn
                    // can be effected by setting SCLDEL accordingly high.  If
                    // the [`amd_erratum_1394`] feature has been enabled, we
                    // therefore set SCLDEL to a value that will amount to a
                    // 560 ns setup time; if it is not set, we set SCLDEL to
                    // the ST-prescribed value of 280 ns.
                    if #[cfg(feature = "amd_erratum_1394")] {
                        let scldel = 13;
                    } else {
                        let scldel = 6;
                    }
                }

                // Here our APB1 peripheral clock is 100MHz, yielding the
                // following:
                //
                // - A PRESC of 3, yielding a t_presc of 40 ns
                // - An SCLH of 118, yielding a t_sclh of 4760 ns
                // - An SCLL of 127, yielding a t_scll of 5120 ns
                //
                // Taken together, this yields a t_scl of 9880 ns, which (as
                // above) when added to t_sync1 and t_sync2 will be close to
                // our target of 10000 ns.  We set SCLDEL to our [`scldel`]
                // variable and SDADEL to 0 -- the latter coming from the
                // STM32CubeMX tool as advised by 47.4.5.
                i2c.timingr.write(|w| { w
                    .presc().bits(3)
                    .sclh().bits(118)
                    .scll().bits(127)
                    .scldel().bits(scldel)
                    .sdadel().bits(0)
                });
            } else if #[cfg(target_board = "oxcon2023g0")] {
                // This board runs at 64 MHz, yielding:
                //
                // - A PRESC of 4, yielding a t_presc of 62 ns
                // - An SCLH of 61, yielding a t_sclh of 3844 ns
                // - An SCLL of 91, yielding a t_scll of 5704 ns
                //
                // Taken together, this yields a t_scl of 9548 ns.  Which,
                // when added to our t_sync1 and t_sync2 will be close to our
                // target of 10000 ns.  Finally, we set SCLDEL to 3 and SDADEL
                // to 0 -- values that come from the STM32CubeMX tool (as
                // advised by 47.4.5).
                i2c.timingr.write(|w| { w
                    .presc().bits(4)
                    .sclh().bits(61)
                    .scll().bits(91)
                    .scldel().bits(3)
                    .sdadel().bits(0)
                });
            } else if #[cfg(any(feature = "g031", feature = "g030"))] {
                // On the G0, our APB peripheral clock is 16MHz, yielding:
                //
                // - A PRESC of 0, yielding a t_presc of 62 ns
                // - An SCLH of 61, yielding a t_sclh of 3844 ns
                // - An SCLL of 91, yielding a t_scll of 5704 ns
                //
                // Taken together, this yields a t_scl of 9548 ns.  Which,
                // when added to our t_sync1 and t_sync2 will be close to our
                // target of 10000 ns.  Finally, we set SCLDEL to 3 and SDADEL
                // to 0 -- values that come from the STM32CubeMX tool (as
                // advised by 47.4.5).
                i2c.timingr.write(|w| { w
                    .presc().bits(0)
                    .sclh().bits(61)
                    .scll().bits(91)
                    .scldel().bits(3)
                    .sdadel().bits(0)
                });
            } else {
                compile_error!("unknown STM32xx variant");
            }
        }
    }

    fn configure_timeouts(&self, i2c: &RegisterBlock) {
        cfg_if::cfg_if! {
            //
            // The timeout value is defined to be:
            //
            //   t_timeout = (TIMEOUTA + 1) x 2048 x t_i2cclk
            //
            // We want our t_timeout to be at least 25 ms: on h743 with a 10 ns
            // t_i2cclk this yields 1219.7 (1220); on g031, this is 195.88
            // (196). Note that these numbers make assumptions about the
            // system's clocking and clock tree configuration; TODO.
            //
            if #[cfg(any(feature = "h743", feature = "h753"))] {
                i2c.timeoutr.write(|w| { w
                    .timouten().set_bit()           // Enable SCL timeout
                    .timeouta().bits(1220)          // Timeout value
                    .tidle().clear_bit()            // Want SCL, not IDLE
                });
            } else if #[cfg(any(feature = "g030", feature = "g031"))] {
                i2c.timeoutr.write(|w| { w
                    .timouten().set_bit()           // Enable SCL timeout
                    .timeouta().bits(196)           // Timeout value
                    .tidle().clear_bit()            // Want SCL, not IDLE
                });
            } else {
                compile_error!("unknown STM32xx variant");
            }
        }
    }

    pub fn configure(&self) {
        let i2c = self.registers;

        // Disable PE
        self.stop_peripheral();

        self.configure_timing(i2c);
        self.configure_timeouts(i2c);

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .smbhen().set_bit()         // enable SMBus host mode
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // must enable clock stretching
            .errie().set_bit()          // enable Error Interrupt
            .tcie().set_bit()           // enable Transfer Complete interrupt
            .stopie().clear_bit()       // disable Stop Detection interrupt
            .nackie().set_bit()         // enable NACK interrupt
            .rxie().set_bit()           // enable RX interrupt
            .txie().set_bit()           // enable TX interrupt
        });

        self.restart_peripheral();
    }

    /// Shut off the controller, as per the datasheet: clear PE, wait for it
    /// to become 0.
    ///
    /// This performs a _partial_ reset of the controller's state, as described
    /// in reference manual section 47.4.6. Concretely,
    ///
    /// - CR2 bits START, STOP, and NACK (also PECBYTE which we don't currently
    ///   use) are cleared to zero.
    /// - ISR bits BUSY, TXIS, RXNE, ADDR, NACKF, TCR, TC, STOPF, BERR, ARLO,
    ///   OVR, PECERR, TIMEOUT, and ALERT are cleared to 0.
    /// - ISR bit TXE is set to 1.
    ///
    /// While not explicitly mentioned in the manual, the effect on TXIS and TXE
    /// makes it clear that this flushes anything in the semi-hidden TXDR
    /// register as a side effect, effectively purging state from any pipelined
    /// write in progress.
    pub fn stop_peripheral(&self) {
        let i2c = self.registers;

        // We must keep PE low for 3 APB cycles. APB cycles are not very long,
        // so rather than attempting to time that, the reference manual proposes
        // the following procedure: first, clear the bit...
        i2c.cr1.modify(|_, w| w.pe().clear_bit());

        // ...then, read it back until it reads as clear. Since the read-back is
        // over the APB, this inherently synchronizes us with the APB delay.
        loop {
            let cr1 = i2c.cr1.read();
            ringbuf_entry!(Trace::Reset(Register::CR1, cr1.bits()));
            if cr1.pe().bit_is_clear() {
                break;
            }
        }
    }

    /// Reverse the effect of `stop_peripheral`.
    ///
    /// This sets `PE=1`, turning the I2C peripheral state machine back on. Any
    /// bits that were altered with the transition to `PE=0` are left that way
    /// (see `stop_peripheral` for specifics), all other state is unchanged.
    pub fn restart_peripheral(&self) {
        self.registers.cr1.modify(|_, w| w.pe().set_bit());

        // The reference manual does not describe a required delay /
        // synchronization routine for _setting_ the PE bit, only clearing it.

        ringbuf_entry!(Trace::Reset(Register::CR2, self.registers.cr2.read().bits()));
    }

    /// Restart the controller, as per the datasheet: clear PE, wait for it
    /// to become 0, and set it.
    ///
    /// Note that this performs only a _partial_ reset of the controller's
    /// configuration (see `stop_peripheral` for the specific list). For a full
    /// reset to "reset" state, you'll need to set/clear its reset line through
    /// the Sys task.
    pub fn stop_and_restart_peripheral(&self) {
        self.stop_peripheral();
        self.restart_peripheral();
    }

    ///
    /// A common routine to check for errors from the controller.  Note that
    /// we deliberately return a disjoint error code for each condition.
    /// Some of these are more recoverable than others -- but all of these
    /// conditions should generally result in the controller being reset.
    ///
    fn check_errors(&self, isr: &Isr) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers;

        if isr.arlo().is_lost() {
            i2c.icr.write(|w| w.arlocf().set_bit());
            return Err(drv_i2c_api::ResponseCode::BusReset);
        }

        if isr.berr().is_error() {
            i2c.icr.write(|w| w.berrcf().set_bit());
            return Err(drv_i2c_api::ResponseCode::BusError);
        }

        if isr.timeout().is_timeout() {
            i2c.icr.write(|w| w.timoutcf().set_bit());
            return Err(drv_i2c_api::ResponseCode::BusLocked);
        }

        Ok(())
    }

    ///
    /// A common routine to wait for any of our interrupt-related notification
    /// bits. Note that you'll still want to check the actual interrupt status
    /// bits to distinguish a real interrupt from a stale or mischieviously
    /// posted notification bit.
    ///
    fn wfi(&self) {
        sys_recv_notification(self.notification);
    }

    fn wait_until_notbusy(&self) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers;

        //
        // We will spin for some number of laps, in which we very much expect
        // a functional controller to no longer be busy.  The threshold
        // should err on the side of being a little too high:  if the
        // controller remains busy after BUSY_SLEEP_THRESHOLD laps, we will
        // sleep for two milliseconds -- and we do not want to hit that
        // delay on otherwise functional systems!  (If, on the other hand,
        // the threshold is too high and the controller is hung, we will
        // consume more CPU than we would otherwise -- a relatively benign
        // failure mode for a condition expected to be unusual.)
        //
        const BUSY_SLEEP_THRESHOLD: u32 = 300;

        for lap in 0..=BUSY_SLEEP_THRESHOLD + 1 {
            let isr = i2c.isr.read();
            ringbuf_entry!(Trace::Wait(Register::ISR, isr.bits()));

            //
            // For reasons unclear and unknown, the timeout flag can become
            // set on an otherwise idle I2C controller (almost as if TIDLE has
            // been set -- which we explicitly do not do!).  If, when we walk
            // up to the controller, the timeout flag is set, we clear it and
            // ignore it -- we know that it's spurious.
            //
            if lap == 0 && isr.timeout().is_timeout() {
                i2c.icr.write(|w| w.timoutcf().set_bit());
            }

            if !isr.busy().is_busy() {
                return Ok(());
            }

            self.check_errors(&isr)?;

            if lap == BUSY_SLEEP_THRESHOLD {
                //
                // If we have taken BUSY_SLEEP_THRESHOLD laps, we are going to
                // sleep for two ticks -- which should be far greater than the
                // amount of time we would expect the controller to be busy...
                //
                ringbuf_entry!(Trace::BusySleep);
                hl::sleep_for(2);
            }
            // On lap == BUSY_SLEEP_THRESHOLD + 1 we'll fall out.
        }

        //
        // We have already taken BUSY_SLEEP_THRESHOLD laps AND a two tick sleep
        // -- and the busy bit is still set.  At this point, we need to return
        // an error indicating that we need to reset the controller.  We return
        // a disjoint error code here to be able to know that we hit this
        // condition rather than our more expected conditions on bus lockup
        // (namely, a timeout or arbitration lost).
        //
        Err(drv_i2c_api::ResponseCode::ControllerBusy)
    }

    /// Perform a write to and then a read from the specified device.  Either
    /// the write length or the read length can be zero, but one of these must
    /// be non-zero.  Additionally, both lengths must be less than 256 bytes:
    /// the device can support longer buffers, and the implementation could
    /// be extended in the future to allow them.
    pub fn write_read(
        &self,
        addr: u8,
        wlen: usize,
        getbyte: impl Fn(usize) -> Option<u8>,
        mut rlen: ReadLength,
        mut putbyte: impl FnMut(usize, u8) -> Option<()>,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        // Assert our preconditions as described above
        assert!(wlen > 0 || rlen != ReadLength::Fixed(0));
        assert!(wlen <= 255);

        if let ReadLength::Fixed(rlen) = rlen {
            assert!(rlen <= 255);
        }

        let i2c = self.registers;
        let notification = self.notification;

        self.wait_until_notbusy()?;

        if wlen > 0 {
            #[rustfmt::skip]
            i2c.cr2.modify(|_, w| { w
                .nbytes().bits(wlen as u8)
                .autoend().clear_bit()
                .reload().clear_bit()
                .add10().clear_bit()
                .sadd().bits((addr << 1).into())
                .rd_wrn().clear_bit()
                .start().set_bit()
            });

            let mut pos = 0;

            while pos < wlen {
                loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::Write(Register::ISR, isr.bits()));

                    self.check_errors(&isr)?;

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| w.nackcf().set_bit());
                        // Setting ISR.TXE to 1 flushes anything pending there.
                        i2c.isr.write(|w| w.txe().set_bit());
                        return Err(drv_i2c_api::ResponseCode::NoDevice);
                    }

                    if isr.txis().is_empty() {
                        break;
                    }

                    self.wfi();
                    sys_irq_control(notification, true);
                }

                // Get a single byte.
                let byte =
                    getbyte(pos).ok_or(drv_i2c_api::ResponseCode::BadArg)?;

                // And send it!
                i2c.txdr.write(|w| w.txdata().bits(byte));
                pos += 1;
            }

            // All done; now block until our transfer is complete -- or until
            // we've been NACK'd (denoting an illegal register value)
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::WriteWait(Register::ISR, isr.bits()));

                self.check_errors(&isr)?;

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    // Setting ISR.TXE to 1 flushes anything pending there.
                    i2c.isr.write(|w| w.txe().set_bit());
                    return Err(drv_i2c_api::ResponseCode::NoRegister);
                }

                if isr.tc().is_complete() {
                    break;
                }

                self.wfi();
                sys_irq_control(notification, true);
            }
        }

        let mut overrun = false;

        if rlen != ReadLength::Fixed(0) {
            //
            // If we have both a write and a read, we deliberately do not send
            // a STOP between them to force the RESTART (many devices do not
            // permit a STOP between a register address write and a subsequent
            // read).
            //
            if let ReadLength::Fixed(rlen) = rlen {
                #[rustfmt::skip]
                i2c.cr2.modify(|_, w| { w
                    .nbytes().bits(rlen as u8)
                    .autoend().clear_bit()
                    .reload().clear_bit()
                    .add10().clear_bit()
                    .sadd().bits((addr << 1).into())
                    .rd_wrn().set_bit()
                    .start().set_bit()
                });
            } else {
                #[rustfmt::skip]
                i2c.cr2.modify(|_, w| { w
                    .nbytes().bits(1)
                    .autoend().clear_bit()
                    .reload().set_bit()
                    .add10().clear_bit()
                    .sadd().bits((addr << 1).into())
                    .rd_wrn().set_bit()
                    .start().set_bit()
                });
            }

            let mut pos = 0;

            loop {
                if let ReadLength::Fixed(rlen) = rlen {
                    if pos >= rlen {
                        break;
                    }
                }

                loop {
                    self.wfi();
                    sys_irq_control(notification, true);

                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::Read(Register::ISR, isr.bits()));

                    self.check_errors(&isr)?;

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| w.nackcf().set_bit());
                        // Since we're reading, and not transmitting, we don't
                        // need to do anything special to flush TXDR here --
                        // unlike the write case.
                        return Err(drv_i2c_api::ResponseCode::NoDevice);
                    }

                    if !isr.rxne().is_empty() {
                        break;
                    }
                }

                // Read it!
                let byte: u8 = i2c.rxdr.read().rxdata().bits();

                if rlen == ReadLength::Variable {
                    #[rustfmt::skip]
                    i2c.cr2.modify(|_, w| { w
                        .nbytes().bits(byte)
                        .reload().clear_bit()
                    });

                    rlen = ReadLength::Fixed(byte.into());
                    continue;
                }

                if !overrun && putbyte(pos, byte).is_none() {
                    //
                    // If we're unable to accept what we just read, we need to
                    // keep reading to complete the transfer -- but we will
                    // not call putbyte again and we will return failure.
                    //
                    overrun = true;
                }

                pos += 1;
            }

            // All done; now block until our transfer is complete...
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::ReadWait(Register::ISR, isr.bits()));

                if isr.tc().is_complete() {
                    break;
                }

                self.check_errors(&isr)?;

                self.wfi();
                sys_irq_control(notification, true);
            }
        }

        //
        // Whether we did a write alone, a read alone, or a write followed
        // by a read, we're done now -- manually send a STOP.
        //
        i2c.cr2.modify(|_, w| w.stop().set_bit());

        if overrun {
            Err(drv_i2c_api::ResponseCode::TooMuchData)
        } else {
            Ok(())
        }
    }

    ///
    /// Regrettably, some devices insist on special sequences to be sent to
    /// unlock functionality -- effectively a Konami Code for an I2C device.
    /// Of course, there are only two real I2C operations (namely, read and
    /// write), so we assume that a Konami Code that doesn't involve *actual*
    /// reads and *actual* writes is a sequence of zero-byte read and
    /// zero-byte write operations, expressed as a slice of [`I2cKonamiCode`].
    /// Yes, this is terrible -- and if you are left wondering how anyone
    /// could possibly conceive of such a thing, please see the MAX7358 mux
    /// driver.  (If there is a solace, it is that this is a mux driver and
    /// not an I2C device; absent an actual device that has this same
    /// requirement, we need not open up this odd API to other I2C consumers!)
    ///
    pub fn send_konami_code(
        &self,
        addr: u8,
        ops: &[I2cKonamiCode],
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers;
        let notification = self.notification;

        self.wait_until_notbusy()?;

        for op in ops {
            let opval = match *op {
                I2cKonamiCode::Write => false,
                I2cKonamiCode::Read => true,
            };

            ringbuf_entry!(Trace::KonamiOperation(*op));

            #[rustfmt::skip]
            i2c.cr2.modify(|_, w| { w
                .nbytes().bits(0u8)
                .autoend().clear_bit()
                .reload().clear_bit()
                .add10().clear_bit()
                .sadd().bits((addr << 1).into())
                .rd_wrn().bit(opval)
                .start().set_bit()
            });

            // All done; now block until our transfer is complete -- or until
            // we've been NACK'd (presumably denoting a device throwing hands
            // at our Konami Code sequence).
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::Konami(Register::ISR, isr.bits()));

                self.check_errors(&isr)?;

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    // Even when we're writing, the "konami code" sends no data,
                    // so we haven't loaded TXDR, so we don't need to flush it
                    // on nack.
                    return Err(drv_i2c_api::ResponseCode::NoRegister);
                }

                if isr.tc().is_complete() {
                    break;
                }

                self.wfi();
                sys_irq_control(notification, true);
            }
        }

        //
        // We have sent the cheat keys -- manually send a STOP.
        //
        i2c.cr2.modify(|_, w| w.stop().set_bit());

        Ok(())
    }

    fn configure_as_target(&self) {
        let i2c = self.registers;

        // Disable PE
        i2c.cr1.write(|w| w.pe().clear_bit());

        self.configure_timing(i2c);

        #[rustfmt::skip]
        i2c.oar1.modify(|_, w| { w
            .oa1en().clear_bit()                    // own-address disable 
        });

        #[rustfmt::skip]
        i2c.oar2.modify(|_, w| { w
            .oa2en().set_bit()                  // own-address-2 enable
            .oa2msk().bits(0b111)                // mask 7 == match all
        });

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .gcen().clear_bit()           // disable General Call
            .nostretch().clear_bit()      // enable clock stretching
            .sbc().clear_bit()            // disable byte control 
            .errie().clear_bit()          // \
            .tcie().clear_bit()           //  |
            .stopie().clear_bit()         //  | disable
            .nackie().clear_bit()         //  | all
            .addrie().clear_bit()         //  | interrupt
            .rxie().clear_bit()           //  | sources
            .txie().clear_bit()           // /
        });

        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    pub fn operate_as_target(
        &self,
        ctrl: &I2cTargetControl,
        mut initiate: impl FnMut(u8) -> bool,
        mut rxbyte: impl FnMut(u8, u8),
        mut txbyte: impl FnMut(u8) -> Option<u8>,
    ) -> ! {
        // Note: configure_as_target toggles the CR1.PE bit, which has the side
        // effect of clearing all flags.
        self.configure_as_target();

        let i2c = self.registers;
        let notification = self.notification;

        'addrloop: loop {
            // Flush our TXDR. TODO: does this ever matter in practice? Are we
            // making it to this point with TXE clear?
            i2c.isr.modify(|_, w| w.txe().set_bit());

            // Wait to be addressed.
            let (is_write, addr) = loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::Addr(Register::ISR, isr.bits()));

                // We expect STOPF to have been handled by the transaction loop
                // below, but given that there may be other irrelevant
                // transactions on the bus, we'll go ahead and clear it here.
                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    continue;
                }

                // ADDR being set means that we've been addressed -- either as a
                // result of a START condition, or a repeated START punted by
                // the transaction loop below.
                if isr.addr().is_match() {
                    i2c.icr.write(|w| w.addrcf().set_bit());
                    ringbuf_entry!(Trace::AddrMatch);
                    break (isr.dir().is_write(), isr.addcode().bits());
                }

                // Enable the interrupt sources we care about. Note that despite
                // handling STOPF above, we don't enable an interrupt on it,
                // because we don't actually care.
                i2c.cr1.modify(|_, w| w.addrie().set_bit());
                ringbuf_entry!(Trace::WaitAddr);
                (ctrl.enable)(notification);
                (ctrl.wfi)(notification);
                // Turn interrupt sources back off.
                i2c.cr1.modify(|_, w| w.addrie().clear_bit());
            };

            // See if we want to initiate with this address, NACK'ing it if
            // not.  Note that if we are being sent bytes, it is too late to
            // NACK the address itself; the NACK will be on the write.
            //
            // Note also that, if we decline to respond to the address, we're
            // still going to go through all the transaction machinery below!
            // This helps to ensure that we maintain the flags correctly. It has
            // the semi-strange side effect that we will process transactions
            // sent to any other device on the bus, and send responses that keep
            // SDA in its recessive (high) state so the other device can talk.
            //
            // This means we will inject our clock stretching intervals into
            // _all traffic_ and is probably worth fixing (TODO).
            let initiated = initiate(addr);

            if !initiated {
                // NACK the first byte.
                i2c.cr2.modify(|_, w| w.nack().set_bit());
                ringbuf_entry!(Trace::AddrNack(addr));
            }

            if is_write {
                // During the write phase, the host sends bytes our way, and we
                // have the opportunity to ACK/NACK each one. This phase
                // continues until the host generates either a repeated start or
                // a stop condition.
                //
                // If we're not responding to this transaction, we have set the
                // NACK flag above. However, this only applies to one byte. The
                // host is free to continue clocking us after a NACK, which we
                // handle below.
                'rxloop: loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::RxReg(Register::ISR, isr.bits()));

                    // Note: the order of interrupt flag handling in this
                    // routine is important. More details interleaved below.

                    // Check for and handle RXNE first, to ensure that incoming
                    // data gets handled and isn't left around waiting for
                    // later. We can be confident that the data waiting in RX is
                    // from this transaction, and not a later transaction on the
                    // far side of a STOP/NACK, because we have configured the
                    // controller to clock-stretch if we're repeatedly
                    // addressed, preventing the reception of further data until
                    // we get out of this loop and do it all over again.
                    if isr.rxne().is_not_empty() {
                        // Always take the byte from the shift register, even if
                        // we're ignoring it, lest the shift register clog up.
                        let rx = i2c.rxdr.read().rxdata().bits();

                        if initiated {
                            ringbuf_entry!(Trace::Rx(addr, rx));
                            rxbyte(addr, rx);
                        } else {
                            // We're ignoring this byte. It has already been
                            // NACK'd, and the NACK flag is self-clearing. Ask
                            // to NACK the next. Our request will be canceled by
                            // STOP or ADDR.
                            i2c.cr2.modify(|_, w| w.nack().set_bit());
                            ringbuf_entry!(Trace::RxNack(addr, rx));
                        }

                        // Bounce up to the top of the loop, which will cause
                        // other flags to get handled.
                        continue 'rxloop;
                    }

                    // If we have seen a STOP condition, our current transaction
                    // is over, and we want to ignore the ADDR flag being set
                    // since that'll get handled at the top of the loop.
                    if isr.stopf().is_stop() {
                        ringbuf_entry!(Trace::Stop);
                        i2c.icr.write(|w| w.stopcf().set_bit());
                        continue 'addrloop;
                    }

                    // Note: during this phase we are receiving data from the
                    // controller and generating ACKs/NACKs. This means the
                    // NACKF is irrelevant, as it's only set when a NACK is
                    // _received._

                    // If we've processed all incoming data and have not seen a
                    // STOP condition, then the ADDR flag being set means we've
                    // been addressed in a repeated start.
                    if isr.addr().is_match() {
                        i2c.icr.write(|w| w.addrcf().set_bit());

                        //
                        // If we have an address match, check to see if this is
                        // change in direction; if it is, break out of our receive
                        // loop.
                        //
                        if !isr.dir().is_write() {
                            ringbuf_entry!(Trace::RepeatedStart(true));
                            break 'rxloop;
                        }

                        // Repeated start without a direction change is
                        // slightly weird, but, we'll handle it as best we can.
                        ringbuf_entry!(Trace::RepeatedStart(false));
                        continue 'rxloop;
                    }

                    // Enable the interrupt sources we use.
                    #[rustfmt::skip]
                    i2c.cr1.modify(|_, w| {
                        w.stopie().set_bit()
                            .addrie().set_bit()
                            .rxie().set_bit()
                    });

                    ringbuf_entry!(Trace::WaitRx);
                    (ctrl.enable)(notification);
                    (ctrl.wfi)(notification);

                    // Turn them back off before we potentially break out of the
                    // loop above.
                    #[rustfmt::skip]
                    i2c.cr1.modify(|_, w| {
                        w.stopie().clear_bit()
                            .addrie().clear_bit()
                            .rxie().clear_bit()
                    });
                }
            }

            'txloop: loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::TxReg(Register::ISR, isr.bits()));

                // First, we want to see if we're still transmitting.

                // See if the host has NACK'd us. When our peripheral receives a
                // NACK, it releases the SDA/SCL lines and stops setting TXIS.
                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    // Do _not_ abort the transmission at this point. The host
                    // may do something dumb like continue reading past our
                    // NACK. Wait for STOP or ADDR (repeated start).

                    // Fall through to the other flag handling below.
                }

                // A STOP condition _always_ indicates that the transmission is
                // over... even if we don't think we're done sending. So,
                // process it before attempting to put more data on the wire in
                // response to TXIS below.
                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    break 'txloop;
                }

                // ADDR will be set by a repeated start. We'll handle it by
                // _leaving it set_ and bopping back up to the top to start a
                // new transaction.
                if isr.addr().is_match() {
                    continue 'addrloop;
                }

                // If we get here, it means the host is still clocking bytes out
                // of us, so we need to send _something_ or we'll lock the bus
                // forever.
                if isr.txis().is_empty() {
                    // This byte is deliberately indistinguishable from no
                    // activity from the target on the bus. This is
                    // important since we're wired-ANDing our output with
                    // any other I2C devices at this point.
                    const FILLER: u8 = 0xff;

                    if initiated {
                        match txbyte(addr) {
                            Some(byte) => {
                                ringbuf_entry!(Trace::Tx(addr, byte));
                                i2c.txdr.write(|w| w.txdata().bits(byte));
                            }
                            None => {
                                //
                                // The initiator is asking for more than we've
                                // got, either because it is reading from an
                                // invalid device address, or it wrote to an
                                // invalid register/address, or it's asking
                                // for more data than is supported.  However
                                // it's happening, we don't have a way of
                                // NACK'ing the request once our address is
                                // ACK'd, so we will just return filler data
                                // until the iniatior releases us from their
                                // grip.
                                //
                                ringbuf_entry!(Trace::TxOverrun(addr));
                                i2c.txdr.write(|w| w.txdata().bits(FILLER));
                            }
                        }
                    } else {
                        ringbuf_entry!(Trace::TxBogus(addr));
                        i2c.txdr.write(|w| w.txdata().bits(FILLER));
                    }

                    // Don't WFI because there may be more work to do
                    // immediately.
                    continue 'txloop;
                }

                // Enable the interrupt sources we care about.
                #[rustfmt::skip]
                i2c.cr1.modify(|_, w| {
                    w.txie().set_bit()
                        .addrie().set_bit()
                        .nackie().set_bit()
                        .stopie().set_bit()
                });
                ringbuf_entry!(Trace::WaitTx);
                (ctrl.enable)(notification);
                (ctrl.wfi)(notification);
                // Turn interrupt sources back off.
                #[rustfmt::skip]
                i2c.cr1.modify(|_, w| {
                    w.txie().clear_bit()
                        .addrie().clear_bit()
                        .nackie().clear_bit()
                        .stopie().clear_bit()
                });
            }
        }
    }
}
