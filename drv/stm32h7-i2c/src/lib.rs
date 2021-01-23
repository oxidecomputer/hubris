//! A driver for the STM32H7 I2C interface

#![no_std]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use userlib::*;
use ringbuf::*;

#[cfg(feature = "h7b3")]
pub type RegisterBlock = device::i2c3::RegisterBlock;

#[cfg(feature = "h743")]
pub type RegisterBlock = device::i2c1::RegisterBlock;

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

pub enum I2cMuxDriver {
    LTC4306,
}

pub enum I2cError {
    NoDevice,
    NoRegister
}

pub struct I2cMux {
    pub controller: drv_i2c_api::Controller,
    pub port: drv_i2c_api::Port,
    pub driver: I2cMuxDriver,
    pub enable: (
        drv_stm32h7_gpio_api::Port,
        drv_stm32h7_gpio_api::Alternate, 
        u16
    ),
    pub address: u8,
    pub segments: u8,
    pub segment: Option<u8>,
}

ringbuf!(u32, 4, 0);

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

    pub fn configure(&mut self) {
        assert!(self.registers.is_none());

        let i2c = unsafe { &*(self.getblock)() };

        // Disable PE
        i2c.cr1.write(|w| { w.pe().clear_bit() });

        self.configure_timing(i2c);

        #[rustfmt::skip]
        i2c.cr1.modify(|_, w| { w
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // must enable clock stretching
            .errie().set_bit()          // emable Error Interrupt
            .tcie().set_bit()           // enable Transfer Complete interrupt
            .stopie().set_bit()         // enable Stop Detection interrupt
            .nackie().set_bit()         // enable NACK interrupt
            .rxie().set_bit()           // enable RX interrupt
            .txie().set_bit()           // enable TX interrupt
        });

        i2c.cr1.modify(|_, w| { w.pe().set_bit() });
        self.registers = Some(i2c);
    }

    pub fn write_read(
        &self,
        addr: u8,
        wlen: usize,
        getbyte: impl Fn(usize) -> Option<u8>,
        rlen: usize,
        putbyte: impl Fn(usize, u8) -> Option<()>,
        mut enable: impl FnMut(u32),
        mut wfi: impl FnMut(u32),
    ) -> Result<(), I2cError> {
        assert!(wlen > 0 || rlen > 0);
        assert!(wlen <= 255 && rlen <= 255);

        let i2c = self.registers.unwrap();
        let notification = self.notification;

        // Before we talk to the controller, spin until it isn't busy
        loop {
            let isr = i2c.isr.read();

            if !isr.busy().is_busy() {
                break;
            }
        }

        if wlen > 0 {
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

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| { w.nackcf().set_bit() });
                        return Err(I2cError::NoDevice);
                    }

                    if isr.txis().is_empty() {
                        break;
                    }

                    wfi(notification);
                    enable(notification);
                }

                // Get a single byte. This is safe to unwrap because our
                // length has been specified as a parameter.
                let byte: u8 = getbyte(pos).unwrap();

                // And send it!
                i2c.txdr.write(|w| w.txdata().bits(byte));
                pos += 1;
            }

            // All done; now block until our transfer is complete -- or until
            // we've been NACK'd (denoting an illegal register value)
            loop {
                let isr = i2c.isr.read();

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    return Err(I2cError::NoRegister);
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

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| { w.nackcf().set_bit() });
                        return Err(I2cError::NoDevice);
                    }

                    if !isr.rxne().is_empty() {
                        break;
                    }
                }

                // Read it!
                let byte: u8 = i2c.rxdr.read().rxdata().bits();
                putbyte(pos, byte).unwrap();
                pos += 1;
            }

            // All done; now block until our transfer is complete...
            while !i2c.isr.read().tc().is_complete() {
                wfi(notification);
                enable(notification);
            }
        }

        //
        // Whether we did a write alone, a read alone, or a write followed
        // by a read, we're done now -- manually send a STOP.
        //
        i2c.cr2.modify(|_, w| { w.stop().set_bit() });

        Ok(())
    }

    fn configure_as_target(&mut self, address: u8) {
        assert!(self.registers.is_none());
        assert!(address & 0b1000_0000 == 0);

        let i2c = unsafe { &*(self.getblock)() };

        // Disable PE
        i2c.cr1.write(|w| { w.pe().clear_bit() });

        self.configure_timing(i2c);

        i2c.oar1.modify(|_, w| { w
            .oa1en().set_bit()
            .oa1mode().clear_bit()
            .oa1().bits((address << 1).into())
        });

        i2c.oar2.modify(|_, w| { w
            .oa2en().clear_bit()
        });

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

        i2c.cr1.modify(|_, w| { w.pe().set_bit() });
        self.registers = Some(i2c);
    }

    pub fn operate_as_target<'b>(
        &mut self,
        address: u8,
        mut enable: impl FnMut(u32),
        mut wfi: impl FnMut(u32),
        mut readreg: impl FnMut(Option<u8>, &mut [u8]) -> Option<usize>
    ) -> ! {
        self.configure_as_target(address);

        ringbuf_entry!(0);

        let mut wbuf = [0; 4];

        let i2c = self.registers.unwrap();
        let notification = self.notification;

        enable(notification);

        let mut register = None;

        'addrloop: loop {
            let is_write = loop {
                let isr = i2c.isr.read();
                ringbuf_entry!(isr.bits());

                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| { w.stopcf().set_bit() });
                    continue;
                }

                if isr.addr().is_match_() {
                    ringbuf_entry!(0xaaaa);
                    break isr.dir().is_write();
                }

                wfi(notification);
                enable(notification);
            };

            // Clear our Address interrupt
            i2c.icr.write(|w| { w.addrcf().set_bit() });

            if is_write {
                ringbuf_entry!(0xbbbb);
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
                            i2c.icr.write(|w| { w.addrcf().set_bit() });
                            break 'rxloop;
                        }

                        i2c.icr.write(|w| { w.addrcf().set_bit() });
                        continue 'rxloop;
                    }

                    if isr.stopf().is_stop() {
                        i2c.icr.write(|w| { w.stopcf().set_bit() });
                        break 'rxloop;
                    }

                    if isr.nackf().is_nack() {
                        i2c.icr.write(|w| { w.nackcf().set_bit() });
                        break 'rxloop;
                    }

                    if isr.rxne().is_not_empty() {
                        //
                        // We have a byte; we'll read it, and continue to wait
                        // for additional bytes.
                        //
                        register = Some(i2c.rxdr.read().rxdata().bits());
                        ringbuf_entry!(register.unwrap() as u32);
                        continue 'rxloop;
                    }

                    wfi(notification);
                    enable(notification);
                }
            }

            let wlen = match readreg(register, &mut wbuf) {
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
                Some(len) => len
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

                if isr.nackf().is_nack() {
                    ringbuf_entry!(0xeeee);
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    continue 'addrloop;
                }

                if isr.txis().is_empty() {
                    if pos < wlen {
                        i2c.txdr.write(|w| { w.txdata().bits(wbuf[pos]) });
                        ringbuf_entry!(wbuf[pos] as u32);
                        pos += 1;
                        continue 'txloop;
                    } else {
                        //
                        // Nothing more to send -- flush TXDR.  (This bogus
                        // byte will only be seen on the wire if we haven't
                        // sent anything at all.)
                        //
                        ringbuf_entry!(0xcccc);
                        i2c.txdr.write(|w| { w.txdata().bits(0x1d) });
                        i2c.isr.modify(|_, w| { w.txe().set_bit() });
                        continue 'txloop;
                    }
                }

                wfi(notification);
                enable(notification);
            }
        }
    }
}
