//! A driver for the STM32H7 I2C interface

#![no_std]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h7b3")]
pub type RegisterBlock = device::i2c3::RegisterBlock;

#[cfg(feature = "h743")]
pub type RegisterBlock = device::i2c1::RegisterBlock;

mod ltc4306;
use ltc4306::Ltc4306;

mod max7358;
use max7358::Max7358;

use num_traits::cast::FromPrimitive;
use ringbuf::*;

ringbuf!(u32, 8, 0);

pub struct I2cPin {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::Port,
    pub gpio_port: drv_stm32h7_gpio_api::Port,
    pub function: drv_stm32h7_gpio_api::Alternate,
    pub mask: u16,
}

pub struct I2cController<'a> {
    pub controller: drv_i2c_api::Controller,
    pub peripheral: drv_stm32h7_rcc_api::Peripheral,
    pub getblock: fn() -> *const RegisterBlock,
    pub notification: u32,
    pub port: Option<drv_i2c_api::Port>,
    pub registers: Option<&'a RegisterBlock>,
}

pub enum I2cSpecial {
    Read,
    Write,
}

///
/// I2C mux devices
///
pub enum I2cMuxDevice {
    Ltc4306,
    Max7358,
}

///
/// A trait to express an I2C mux driver.  We can't use static dispatch here
/// because we want to be able to declare an array of [`I2cMux`] where each
/// mux has (potentially) a different driver.  But we also can't use dynamic
/// dispatch because our various functions here take closures to both enable
/// notifications and wait for interrupts (and are therefore not suitable for
/// an object-safe trait because they are a generic type parameter).  So we
/// choose arguably the worst of all worlds, and statically dispatch based on
/// a single type ([`I2cMuxDevice`]) that manually effects dynamic dispatch by
/// matching on its device.  The downside to this is that we can't expect the
/// compiler to figure out when one of these [`I2cMuxDevice`] types can't, in
/// fact, be used -- and therefore none of the implementations can be elided.
/// More elegant solutions to this very much welcome!
///
pub trait I2cMuxDriver {
    /// Configure the mux, specifying the mux and controller, but also an
    /// instance to a [`Gpio`] task.
    fn configure(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        gpio: &drv_stm32h7_gpio_api::Gpio,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Reset the mux
    fn reset(
        &self,
        mux: &I2cMux,
        gpio: &drv_stm32h7_gpio_api::Gpio,
    ) -> Result<(), drv_i2c_api::ResponseCode>;

    /// Enable the specified segment on the specified mux
    fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: drv_i2c_api::Segment,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), drv_i2c_api::ResponseCode>;
}

impl I2cMuxDriver for I2cMuxDevice {
    fn configure(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        gpio: &drv_stm32h7_gpio_api::Gpio,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        match self {
            I2cMuxDevice::Max7358 => {
                Max7358.configure(mux, controller, enable, wfi)
            }
            _ => {
                if let Some(pin) = &mux.enable {
                    gpio.configure(
                        pin.gpio_port,
                        pin.mask,
                        drv_stm32h7_gpio_api::Mode::Output,
                        drv_stm32h7_gpio_api::OutputType::PushPull,
                        drv_stm32h7_gpio_api::Speed::High,
                        drv_stm32h7_gpio_api::Pull::None,
                        pin.function,
                    )
                    .unwrap();

                    gpio.set_reset(pin.gpio_port, pin.mask, 0).unwrap();
                }
                Ok(())
            }
        }
    }

    fn reset(
        &self,
        mux: &I2cMux,
        gpio: &drv_stm32h7_gpio_api::Gpio,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        match self {
            I2cMuxDevice::Max7358 => {
                panic!("not yet supported");
            }
            _ => {
                if let Some(pin) = &mux.enable {
                    gpio.set_reset(pin.gpio_port, 0, pin.mask).unwrap();
                    gpio.set_reset(pin.gpio_port, pin.mask, 0).unwrap();
                }
            }
        }

        Ok(())
    }

    fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: drv_i2c_api::Segment,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        match self {
            I2cMuxDevice::Ltc4306 => {
                Ltc4306.enable_segment(mux, controller, segment, enable, wfi)
            }
            I2cMuxDevice::Max7358 => {
                Max7358.enable_segment(mux, controller, segment, enable, wfi)
            }
        }
    }
}

pub struct I2cMux {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::Port,
    pub id: drv_i2c_api::Mux,
    pub driver: I2cMuxDevice,
    pub enable: Option<I2cPin>,
    pub address: u8,
    pub segment: Option<drv_i2c_api::Segment>,
}

impl<'a> I2cController<'a> {
    pub fn enable(&self, rcc_driver: &drv_stm32h7_rcc_api::Rcc) {
        rcc_driver.enable_clock(self.peripheral);
        rcc_driver.leave_reset(self.peripheral);
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
            } else if #[cfg(feature = "h743")] {
                // Here our APB1 peripheral clock is 100MHz, yielding the
                // following:
                //
                // - A PRESC of 1, yielding a t_presc of 20 ns
                // - An SCLH of 236 (0xec), yielding a t_sclh of 4740 ns
                // - An SCLL of 255 (0xff), yielding a t_scll of 5120 ns
                //
                // Taken together, this yields a t_scl of 9860 ns, which (as
                // above) when added to t_sync1 and t_sync2 will be close to
                // our target of 10000 ns.  Finally, we set SCLDEL to 12 and
                // SDADEL to 0 -- values that come from from the STM32CubeMX
                // tool (as advised by 47.4.5).
                i2c.timingr.write(|w| { w
                    .presc().bits(1)
                    .sclh().bits(236)
                    .scll().bits(255)
                    .scldel().bits(12)
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
            if #[cfg(feature = "h743")] {
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

    pub fn configure(&mut self) {
        assert!(self.registers.is_none());

        let i2c = unsafe { &*(self.getblock)() };
        self.registers = Some(i2c);

        // Disable PE
        i2c.cr1.write(|w| w.pe().clear_bit());

        self.configure_timing(i2c);
        self.configure_timeouts(i2c);

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .smbhen().set_bit()         // enable SMBus host mode
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // must enable clock stretching
            .errie().set_bit()          // emable Error Interrupt
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
        let i2c = self.registers.unwrap();

        // Disable PE
        i2c.cr1.modify(|_, w| w.pe().clear_bit());

        loop {
            let cr1 = i2c.cr1.read();
            ringbuf_entry!(cr1.bits());
            if cr1.pe().is_disabled() {
                break;
            }
        }

        i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    pub fn write_read(
        &self,
        addr: u8,
        wlen: usize,
        getbyte: impl Fn(usize) -> u8,
        rlen: usize,
        mut putbyte: impl FnMut(usize, u8),
        mut enable: impl FnMut(u32),
        mut wfi: impl FnMut(u32),
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        assert!(wlen > 0 || rlen > 0);
        assert!(wlen <= 255 && rlen <= 255);

        let i2c = self.registers.unwrap();
        let notification = self.notification;

        // Before we talk to the controller, spin until it isn't busy
        loop {
            let isr = i2c.isr.read();
            ringbuf_entry!(isr.bits());

            if !isr.busy().is_busy() {
                break;
            }

            if isr.timeout().is_timeout() {
                i2c.icr.write(|w| w.timoutcf().set_bit());
                return Err(drv_i2c_api::ResponseCode::BusLocked);
            }
        }

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
                    ringbuf_entry!(isr.bits());

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

                    wfi(notification);
                    enable(notification);
                }

                // Get a single byte.
                let byte: u8 = getbyte(pos);

                // And send it!
                i2c.txdr.write(|w| w.txdata().bits(byte));
                pos += 1;
            }

            // All done; now block until our transfer is complete -- or until
            // we've been NACK'd (denoting an illegal register value)
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(isr.bits());

                if isr.timeout().is_timeout() {
                    i2c.icr.write(|w| w.timoutcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::BusLocked);
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    return Err(drv_i2c_api::ResponseCode::NoRegister);
                }

                if isr.tc().is_complete() {
                    break;
                }

                wfi(notification);
                enable(notification);
            }
        }

        if rlen > 0 {
            //
            // If we have both a write and a read, we deliberately do not send
            // a STOP between them to force the RESTART (many devices do not
            // permit a STOP between a register address write and a subsequent
            // read).
            //
            #[rustfmt::skip]
            i2c.cr2.modify(|_, w| { w
                .nbytes().bits(rlen as u8)
                .autoend().clear_bit()
                .add10().clear_bit()
                .sadd().bits((addr << 1).into())
                .rd_wrn().set_bit()
                .start().set_bit()
            });

            let mut pos = 0;

            while pos < rlen {
                loop {
                    wfi(notification);
                    enable(notification);

                    let isr = i2c.isr.read();
                    ringbuf_entry!(isr.bits());

                    if isr.timeout().is_timeout() {
                        i2c.icr.write(|w| w.timoutcf().set_bit());
                        return Err(drv_i2c_api::ResponseCode::BusLocked);
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
                putbyte(pos, byte);
                pos += 1;
            }

            // All done; now block until our transfer is complete...
            while !i2c.isr.read().tc().is_complete() {
                ringbuf_entry!(i2c.isr.read().bits());
                wfi(notification);
                enable(notification);
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
    /// unlock functionality.  Of course, there are only two real I2C
    /// operations (namely, read and write), so we assume that special
    /// sequences that don't involve *actual* reads and *actual* writes are
    /// sequence of zero-byte read and zero-byte write operations, expressed
    /// as a slice of [`I2cSpecial`].
    ///
    pub fn special(
        &self,
        addr: u8,
        ops: &[I2cSpecial],
        mut enable: impl FnMut(u32),
        mut wfi: impl FnMut(u32),
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        let i2c = self.registers.unwrap();
        let notification = self.notification;

        // Before we talk to the controller, spin until it isn't busy
        loop {
            let isr = i2c.isr.read();
            ringbuf_entry!(isr.bits());

            if !isr.busy().is_busy() {
                break;
            }

            if isr.timeout().is_timeout() {
                i2c.icr.write(|w| w.timoutcf().set_bit());
                return Err(drv_i2c_api::ResponseCode::BusLocked);
            }
        }

        for op in ops {
            let opval = match *op {
                I2cSpecial::Write => false,
                I2cSpecial::Read => true,
            };

            ringbuf_entry!(if opval { 1 } else { 0 });

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
            // at our special sequence).
            loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(isr.bits());

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
                    break;
                }

                if isr.tc().is_complete() {
                    break;
                }

                wfi(notification);
                enable(notification);
            }
        }

        //
        // We have sent the special sequence -- manually send a STOP.
        //
        i2c.cr2.modify(|_, w| w.stop().set_bit());

        Ok(())
    }

    fn configure_as_target(&mut self, address: u8, secondary: Option<u8>) {
        assert!(self.registers.is_none());
        assert!(address & 0b1000_0000 == 0);

        let i2c = unsafe { &*(self.getblock)() };

        // Disable PE
        i2c.cr1.write(|w| w.pe().clear_bit());

        self.configure_timing(i2c);

        #[rustfmt::skip]
        i2c.oar1.modify(|_, w| { w
            .oa1en().set_bit()                      // own-address enable
            .oa1mode().clear_bit()                  // 7-bit address
            .oa1().bits((address << 1).into())      // address bits
        });

        if let Some(address) = secondary {
            #[rustfmt::skip]
            i2c.oar2.modify(|_, w| { w
                .oa2en().set_bit()                  // own-address-2 enable
                .oa2().bits(address.into())         // address bits
                .oa2msk().bits(0)                   // mask 0 == exact match
            });
        } else {
            #[rustfmt::skip]
            i2c.oar2.modify(|_, w| { w
                .oa2en().clear_bit()                // own-address 2 disable
            });
        }

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // enable clock stretching
            .sbc().clear_bit()          // disable byte control 
            .errie().set_bit()          // emable Error Interrupt
            .tcie().set_bit()           // enable Transfer Complete interrupt
            .stopie().set_bit()         // enable Stop Detection interrupt
            .nackie().set_bit()         // enable NACK interrupt
            .addrie().set_bit()         // enable Address interrupt
            .rxie().set_bit()           // enable RX interrupt
            .txie().set_bit()           // enable TX interrupt
        });

        i2c.cr1.modify(|_, w| w.pe().set_bit());
        self.registers = Some(i2c);
    }

    pub fn operate_as_target<'b>(
        &mut self,
        address: u8,
        secondary: Option<u8>,
        mut enable: impl FnMut(u32),
        mut wfi: impl FnMut(u32),
        mut readreg: impl FnMut(u8, Option<u8>, &mut [u8]) -> Option<usize>,
    ) -> ! {
        self.configure_as_target(address, secondary);

        let mut wbuf = [0; 4];

        let i2c = self.registers.unwrap();
        let notification = self.notification;

        enable(notification);

        let mut register = None;

        'addrloop: loop {
            let (is_write, addr) = loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(isr.bits());

                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| w.stopcf().set_bit());
                    continue;
                }

                if isr.addr().is_match_() {
                    ringbuf_entry!(1);
                    break (isr.dir().is_write(), isr.addcode().bits());
                }

                wfi(notification);
                enable(notification);
            };

            // Clear our Address interrupt
            i2c.icr.write(|w| w.addrcf().set_bit());

            if is_write {
                'rxloop: loop {
                    let isr = i2c.isr.read();
                    ringbuf_entry!(isr.bits());

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
                        register = Some(i2c.rxdr.read().rxdata().bits());
                        continue 'rxloop;
                    }

                    wfi(notification);
                    enable(notification);
                }
            }

            let wlen = match readreg(addr, register, &mut wbuf) {
                None => {
                    //
                    // We have read from an invalid register, but we don't
                    // have a way of immediately NACK'ing it -- so we will
                    // instead indicate that we have zero bytes to send,
                    // which will in fact send one byte when we flush TXDR
                    // below (upshot being that we won't actually NACK invalid
                    // registers at all -- but many I2C targets can't/don't).
                    //
                    0
                }
                Some(len) => len,
            };

            let mut pos = 0;

            'txloop: loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(isr.bits());

                if isr.addr().is_match_() {
                    //
                    // We really aren't expecting this, so kick out to the top
                    // of the loop to try to make sense of it.
                    //
                    continue 'addrloop;
                }

                if isr.txis().is_empty() {
                    if pos < wlen {
                        ringbuf_entry!(wbuf[pos] as u32);
                        i2c.txdr.write(|w| w.txdata().bits(wbuf[pos]));
                        pos += 1;
                        continue 'txloop;
                    } else {
                        //
                        // Nothing more to send -- flush TXDR.  (This bogus
                        // byte will only be seen on the wire if we haven't
                        // sent anything at all.)
                        //
                        i2c.txdr.write(|w| w.txdata().bits(0x1d));
                        i2c.isr.modify(|_, w| w.txe().set_bit());
                        continue 'txloop;
                    }
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| w.nackcf().set_bit());
                    continue 'addrloop;
                }

                wfi(notification);
                enable(notification);
            }
        }
    }
}
