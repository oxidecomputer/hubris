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

        i2c.cr1.modify(|_, w| { w
            .gcen().clear_bit()         // disable General Call
            .nostretch().clear_bit()    // enable clock stretching
            .sbc().set_bit()            // enable byte control 
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

        let mut wbuf = [0; 4];

        let i2c = self.registers.unwrap();
        let notification = self.notification;

        enable(notification);

        let mut register = None;

        'addrloop: loop {
            let is_write = loop {
                let isr = i2c.isr.read();

                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| { w.stopcf().set_bit() });
                    continue;
                }

                if isr.addr().is_match_() {
                    break isr.dir().is_write();
                }

                wfi(notification);
                enable(notification);
            };

            // Clear our Address interrupt
            i2c.icr.write(|w| { w.addrcf().set_bit() });

            if is_write {
                i2c.cr2.modify(|_, w| { w.nbytes().bits(1) });
                'rxloop: loop {
                    let isr = i2c.isr.read();

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
                        continue 'rxloop;
                    }

                    wfi(notification);
                    enable(notification);
                }
            }

            let wlen = match readreg(register, &mut wbuf) {
                None => {
                    //
                    // We have read from an invalid register; NACK it
                    //
                    i2c.cr2.modify(|_, w| { w
                        .nbytes().bits(0)
                        .nack().set_bit()
                    });
                    continue 'addrloop;
                }
                Some(len) => len
            };

            // This is a read from the controller.  Because SBC is set, we must
            // indicate the number of bytes that we will send.
            i2c.cr2.modify(|_, w| { w.nbytes().bits(wlen as u8) });
            let mut pos = 0;

            'txloop: loop {
                let isr = i2c.isr.read();

                if isr.tc().is_complete() {
                    //
                    // We're done -- write the stop bit, and kick out to our
                    // address loop.
                    //
                    i2c.cr2.modify(|_, w| { w.stop().set_bit() });
                    continue 'addrloop;
                }

                if isr.addr().is_match_() {
                    //
                    // We really aren't expecting this, so kick out to the top
                    // of the loop to try to make sense of it.
                    //
                    continue 'addrloop;
                }

                if isr.txis().is_empty() {
                    if pos < wlen {
                        i2c.txdr.write(|w| { w.txdata().bits(wbuf[pos]) });
                        pos += 1;
                        continue 'txloop;
                    } else {
                        //
                        // We're not really expecting this -- NACK and kick
                        // out.
                        //
                        i2c.cr2.modify(|_, w| { w.nack().set_bit() });
                        continue 'addrloop;
                    }
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    continue 'addrloop;
                }

                wfi(notification);
                enable(notification);
            }
        }
    }
}
