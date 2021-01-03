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

    pub fn configure_as_target(&mut self, address: u8) {
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
}
