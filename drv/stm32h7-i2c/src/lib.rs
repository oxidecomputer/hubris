// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32H7 I2C interface

#![no_std]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[cfg(feature = "h7b3")]
pub type RegisterBlock = device::i2c3::RegisterBlock;

#[cfg(any(feature = "h743", feature = "h753"))]
pub type RegisterBlock = device::i2c1::RegisterBlock;

pub mod ltc4306;
pub mod max7358;
pub mod pca9548;

use ringbuf::*;
use userlib::*;

use drv_stm32xx_sys_api as sys_api;

pub struct I2cPin {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::PortIndex,
    pub gpio_pins: sys_api::PinSet,
    pub function: sys_api::Alternate,
}

pub struct I2cController<'a> {
    pub controller: drv_i2c_api::Controller,
    pub peripheral: sys_api::Peripheral,
    pub notification: u32,
    pub registers: &'a RegisterBlock,
}

///
/// A structure that defines interrupt control flow functions that will be
/// used to pass control flow into the kernel to either enable or wait for
/// interrupts.  Note that this is deliberately a struct and not a trait,
/// allowing the [`I2cMuxDriver`] trait to itself be a trait object.
///
pub struct I2cControl {
    pub enable: fn(u32),
    pub wfi: fn(u32),
}

#[derive(Copy, Clone, PartialEq)]
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
        mux: &I2cMux,
        controller: &I2cController,
        sys: &sys_api::Sys,
        ctrl: &I2cControl,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Reset the mux
    fn reset(
        &self,
        mux: &I2cMux,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Enable the specified segment on the specified mux
    fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: drv_i2c_api::Segment,
        ctrl: &I2cControl,
    ) -> Result<(), drv_i2c_api::ResponseCode>;
}

pub struct I2cMux<'a> {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::PortIndex,
    pub id: drv_i2c_api::Mux,
    pub driver: &'a dyn I2cMuxDriver,
    pub enable: Option<I2cPin>,
    pub address: u8,
}

///
/// An enum describing the amount to read
///
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ReadLength {
    /// Fixed length to read
    Fixed(usize),
    /// Read size is variable: first byte contains length
    Variable,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    WaitISR(u32),
    WriteISR(u32),
    WriteWaitISR(u32),
    ReadISR(u32),
    ReadWaitISR(u32),
    RxISR(u32),
    KonamiISR(u32),
    Konami(I2cKonamiCode),
    ResetISR(u32),
    AddrISR(u32),
    AddrMatch,
    AddrNack(u8),
    Rx(u8, u8),
    RxNack(u8, u8),
    Tx(u8, u8),
    TxBogus(u8),
    TxOverrun(u8),
    TxISR(u32),
    WaitAddr,
    WaitRx,
    WaitTx,
    BusySleep,
    None,
}

ringbuf!(Trace, 48, Trace::None);

impl<'a> I2cMux<'_> {
    /// A convenience routine to translate an error induced by in-band
    /// management into one that can be returned to a caller
    fn error_code(
        &self,
        code: drv_i2c_api::ResponseCode,
    ) -> drv_i2c_api::ResponseCode {
        use drv_i2c_api::ResponseCode;

        match code {
            ResponseCode::NoDevice => ResponseCode::BadMuxAddress,
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
        if let Some(pin) = &self.enable {
            // Set the pins to high _before_ switching to output to avoid
            // glitching.
            sys.gpio_set(pin.gpio_pins).unwrap();
            // Now, expose them as outputs.
            sys.gpio_configure_output(
                pin.gpio_pins,
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            )
            .unwrap();
        }

        Ok(())
    }

    fn reset(
        &self,
        sys: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        if let Some(pin) = &self.enable {
            sys.gpio_reset(pin.gpio_pins).unwrap();
            sys.gpio_set(pin.gpio_pins).unwrap();
        }

        Ok(())
    }
}

impl<'a> I2cController<'a> {
    pub fn enable(&self, sys: &sys_api::Sys) {
        sys.enable_clock(self.peripheral);
        sys.leave_reset(self.peripheral);
    }

    fn configure_timing(&self, i2c: &RegisterBlock) {
        cfg_if::cfg_if! {
            if #[cfg(feature = "h7b3")] {
                // We want to set our timing to achieve a 100kHz SCL. Given
                // our APB4 peripheral clock of 280MHz, here is how we
                // configure our timing:
                //
                // - A PRESC of 7, yielding a t_presc of 28.57 ns.
                // - An SCLH of 137 (0x89), yielding a t_sclh of 3942.86 ns.
                // - An SCLL of 207 (0xcf), yielding a t_scll of 5942.86 ns.
                //
                // Taken together, this yields a t_scl of 9885.71 ns.  Which,
                // when added to our t_sync1 and t_sync2 will be close to our
                // target of 10000 ns.  Finally, we set SCLDEL to 8 and SDADEL
                // to 0 -- values that come from the STM32CubeMX tool (as
                // advised by 52.4.10).
                i2c.timingr.write(|w| { w
                    .presc().bits(7)
                    .sclh().bits(137)
                    .scll().bits(207)
                    .scldel().bits(8)
                    .sdadel().bits(0)
                });

                #[cfg(feature = "amd_erratum_1394")]
                compile_error!("no support for amd_erratum_1394 on h7b3");
            } else if #[cfg(any(feature = "h743", feature = "h753"))] {
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
            } else {
                compile_error!("unknown STM32H7 variant");
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
            // t_i2cclk this yields 1219.7 (1220); on h7b3, this is 3416.9
            // (3417).
            //
            if #[cfg(any(feature = "h743", feature = "h753"))] {
                i2c.timeoutr.write(|w| { w
                    .timouten().set_bit()           // Enable SCL timeout
                    .timeouta().bits(1220)          // Timeout value
                    .tidle().clear_bit()            // Want SCL, not IDLE
                });
            } else if #[cfg(feature = "h7b3")] {
                i2c.timeoutr.write(|w| { w
                    .timouten().set_bit()           // Enable SCL timeout
                    .timeouta().bits(3417)          // Timeout value
                    .tidle().clear_bit()            // Want SCL, not IDLE
                });
            }
        }
    }

    pub fn configure(&self) {
        let i2c = self.registers;

        // Disable PE
        i2c.cr1.write(|w| w.pe().clear_bit());

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

        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    /// Reset the controller, as per the datasheet: clear PE, wait for it
    /// to become 0, and set it.
    pub fn reset(&self) {
        let i2c = self.registers;

        // We must keep PE low for 3 APB cycles (e.g., 30 ns on h743).  To
        // assure that this is done properly, we follow the procedure outlined
        // in the datasheet:  first, clear it...
        i2c.cr1.modify(|_, w| w.pe().clear_bit());

        // ...wait until we see it disabled.
        loop {
            let cr1 = i2c.cr1.read();
            ringbuf_entry!(Trace::ResetISR(cr1.bits()));
            if cr1.pe().is_disabled() {
                break;
            }
        }

        // And then finally set it
        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    fn wait_until_notbusy(&self) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers;

        let mut laps = 0;
        const BUSY_SLEEP_THRESHOLD: u32 = 3;

        loop {
            let isr = i2c.isr.read();
            ringbuf_entry!(Trace::WaitISR(isr.bits()));

            if !isr.busy().is_busy() {
                break;
            }

            if isr.arlo().is_lost() {
                i2c.icr.write(|w| w.arlocf().set_bit());
                return Err(drv_i2c_api::ResponseCode::BusReset);
            }

            if isr.timeout().is_timeout() {
                i2c.icr.write(|w| w.timoutcf().set_bit());
                return Err(drv_i2c_api::ResponseCode::BusLocked);
            }

            laps += 1;

            if laps == BUSY_SLEEP_THRESHOLD {
                //
                // If we have taken BUSY_SLEEP_THRESHOLD laps, we are going to
                // sleep for two ticks -- which should be far greater than the
                // amount of time we would expect the controller to be busy...
                //
                ringbuf_entry!(Trace::BusySleep);
                hl::sleep_for(2);
            } else if laps > BUSY_SLEEP_THRESHOLD {
                //
                // We have already taken BUSY_SLEEP_THRESHOLD laps AND a two
                // tick sleep -- and the busy bit is still set.  At this point,
                // we need to return an error indicating that we need to reset
                // the controller.  We return a disjoint error code here to
                // be able to know that we hit this condition rather than our
                // more expected conditions on bus lockup (namely, a timeout
                // or arbitration lost).
                //
                return Err(drv_i2c_api::ResponseCode::ControllerLocked);
            }
        }

        Ok(())
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
        ctrl: &I2cControl,
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
                .add10().clear_bit()
                .sadd().bits((addr << 1).into())
                .rd_wrn().clear_bit()
                .start().set_bit()
            });

            let mut pos = 0;

            while pos < wlen {
                loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::WriteISR(isr.bits()));

                    if isr.timeout().is_timeout() {
                        i2c.icr.write(|w| w.timoutcf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::BusLocked);
                    }

                    if isr.arlo().is_lost() {
                        i2c.icr.write(|w| w.arlocf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::BusReset);
                    }

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| w.nackcf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::NoDevice);
                    }

                    if isr.txis().is_empty() {
                        break;
                    }

                    (ctrl.wfi)(notification);
                    (ctrl.enable)(notification);
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
                ringbuf_entry!(Trace::WriteWaitISR(isr.bits()));

                if isr.timeout().is_timeout() {
                    i2c.icr.write(|w| w.timoutcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusLocked);
                }

                if isr.arlo().is_lost() {
                    i2c.icr.write(|w| w.arlocf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusReset);
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::NoRegister);
                }

                if isr.tc().is_complete() {
                    break;
                }

                (ctrl.wfi)(notification);
                (ctrl.enable)(notification);
            }
        }

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
                    (ctrl.wfi)(notification);
                    (ctrl.enable)(notification);

                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::ReadISR(isr.bits()));

                    if isr.timeout().is_timeout() {
                        i2c.icr.write(|w| w.timoutcf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::BusLocked);
                    }

                    if isr.arlo().is_lost() {
                        i2c.icr.write(|w| w.arlocf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::BusReset);
                    }

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| w.nackcf().set_bit());
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

                putbyte(pos, byte).ok_or(drv_i2c_api::ResponseCode::BadArg)?;
                pos += 1;
            }

            // All done; now block until our transfer is complete...
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::ReadWaitISR(isr.bits()));

                if isr.tc().is_complete() {
                    break;
                }

                if isr.timeout().is_timeout() {
                    i2c.icr.write(|w| w.timoutcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusLocked);
                }

                if isr.arlo().is_lost() {
                    i2c.icr.write(|w| w.arlocf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusReset);
                }

                (ctrl.wfi)(notification);
                (ctrl.enable)(notification);
            }
        }

        //
        // Whether we did a write alone, a read alone, or a write followed
        // by a read, we're done now -- manually send a STOP.
        //
        i2c.cr2.modify(|_, w| w.stop().set_bit());

        Ok(())
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
        ctrl: &I2cControl,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers;
        let notification = self.notification;

        self.wait_until_notbusy()?;

        for op in ops {
            let opval = match *op {
                I2cKonamiCode::Write => false,
                I2cKonamiCode::Read => true,
            };

            ringbuf_entry!(Trace::Konami(*op));

            #[rustfmt::skip]
            i2c.cr2.modify(|_, w| { w
                .nbytes().bits(0u8)
                .autoend().clear_bit()
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
                ringbuf_entry!(Trace::KonamiISR(isr.bits()));

                if isr.timeout().is_timeout() {
                    i2c.icr.write(|w| w.timoutcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusLocked);
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::NoRegister);
                }

                if isr.arlo().is_lost() {
                    i2c.icr.write(|w| w.arlocf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusReset);
                }

                if isr.tc().is_complete() {
                    break;
                }

                (ctrl.wfi)(notification);
                (ctrl.enable)(notification);
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
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // enable clock stretching
            .sbc().clear_bit()          // disable byte control 
            .errie().set_bit()          // enable Error Interrupt
            .tcie().set_bit()           // enable Transfer Complete interrupt
            .stopie().set_bit()         // enable Stop Detection interrupt
            .nackie().set_bit()         // enable NACK interrupt
            .addrie().set_bit()         // enable Address interrupt
            .rxie().set_bit()           // enable RX interrupt
            .txie().set_bit()           // enable TX interrupt
        });

        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    pub fn operate_as_target<'b>(
        &self,
        ctrl: &I2cControl,
        mut initiate: impl FnMut(u8) -> bool,
        mut rxbyte: impl FnMut(u8, u8),
        mut txbyte: impl FnMut(u8) -> Option<u8>,
    ) -> ! {
        self.configure_as_target();

        let i2c = self.registers;
        let notification = self.notification;

        (ctrl.enable)(notification);

        'addrloop: loop {
            let (is_write, addr) = loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::AddrISR(isr.bits()));

                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    continue;
                }

                if isr.addr().is_match_() {
                    ringbuf_entry!(Trace::AddrMatch);
                    break (isr.dir().is_write(), isr.addcode().bits());
                }

                ringbuf_entry!(Trace::WaitAddr);
                (ctrl.wfi)(notification);
                (ctrl.enable)(notification);
            };

            // Flush our TXDR
            i2c.isr.modify(|_, w| w.txe().set_bit());

            // Clear our Address interrupt
            i2c.icr.write(|w| w.addrcf().set_bit());

            //
            // See if we want to initiate with this address, NACK'ing it if
            // not.  Note that if we are being sent bytes, it is too late to
            // NACK the address itself; the NACK will be on the write.
            //
            let initiated = initiate(addr);

            if !initiated {
                i2c.cr2.modify(|_, w| w.nack().set_bit());
                ringbuf_entry!(Trace::AddrNack(addr));
            }

            if is_write {
                'rxloop: loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(Trace::RxISR(isr.bits()));

                    if isr.addr().is_match_() {
                        //
                        // If we have an address match, check to see if this is
                        // change in direction; if it is, break out of our receive
                        // loop.
                        //
                        if !isr.dir().is_write() {
                            i2c.icr.write(|w| w.addrcf().set_bit());
                            break 'rxloop;
                        }

                        i2c.icr.write(|w| w.addrcf().set_bit());
                        continue 'rxloop;
                    }

                    if isr.stopf().is_stop() {
                        i2c.icr.write(|w| w.stopcf().set_bit());
                        break 'rxloop;
                    }

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| w.nackcf().set_bit());
                        break 'rxloop;
                    }

                    if isr.rxne().is_not_empty() {
                        //
                        // We have a byte; we'll read it, and continue to wait
                        // for additional bytes.
                        //
                        let rx = i2c.rxdr.read().rxdata().bits();

                        if initiated {
                            ringbuf_entry!(Trace::Rx(addr, rx));
                            rxbyte(addr, rx);
                        } else {
                            ringbuf_entry!(Trace::RxNack(addr, rx));
                        }

                        continue 'rxloop;
                    }

                    ringbuf_entry!(Trace::WaitRx);
                    (ctrl.wfi)(notification);
                    (ctrl.enable)(notification);
                }
            }

            'txloop: loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(Trace::TxISR(isr.bits()));

                if isr.addr().is_match_() {
                    //
                    // We really aren't expecting this, so kick out to the top
                    // of the loop to try to make sense of it.
                    //
                    continue 'addrloop;
                }

                if isr.txis().is_empty() {
                    //
                    // This byte is deliberately indistinguishable from no
                    // activity from the target on the bus.
                    //
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

                    continue 'txloop;
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    continue 'addrloop;
                }

                ringbuf_entry!(Trace::WaitTx);
                (ctrl.wfi)(notification);
                (ctrl.enable)(notification);
            }
        }
    }
}
