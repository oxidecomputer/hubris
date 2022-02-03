// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

use drv_spi_api::{SpiDevice, SpiError};
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use ringbuf::*;
use userlib::{hl::sleep_for, task_slot};

task_slot!(GPIO, gpio_driver);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Read(Register, u16),
    Write(Register, u16),
    Id(u16),
}
ringbuf!(Trace, 16, Trace::None);

const fn register_offset(address: u16) -> u16 {
    let addr10_2 = address >> 2;
    let mask_shift = 2 /* turn around bits */ + (2 * ((address >> 1) & 0x1));
    (addr10_2 << 6) | ((0x3 as u16) << mask_shift)
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum Register {
    CIDER = register_offset(0x000),
    SGCR1 = register_offset(0x002),
    SGCR2 = register_offset(0x004),
    SGCR3 = register_offset(0x006),
    SGCR6 = register_offset(0x00c),
    SGCR7 = register_offset(0x00e),
    MACAR1 = register_offset(0x010),
    MACAR2 = register_offset(0x012),
    MACAR3 = register_offset(0x014),

    P1MBCR = register_offset(0x04c),
    P1MBSR = register_offset(0x04e),

    P2MBCR = register_offset(0x058),
    P2MBSR = register_offset(0x05a),

    CFGR = register_offset(0x0d8),
    DSP_CNTRL_6 = register_offset(0x734),
}

pub struct Ksz8463 {
    spi: SpiDevice,
    nrst: PinSet,
    slow_reset: bool,
}

impl Ksz8463 {
    pub fn new(spi: SpiDevice, nrst: PinSet, slow_reset: bool) -> Self {
        Self {
            spi,
            nrst,
            slow_reset,
        }
    }

    pub fn read(&self, r: Register) -> Result<u16, SpiError> {
        let cmd = (r as u16).to_be_bytes();
        let request = [cmd[0], cmd[1]];
        let mut response = [0; 4];

        self.spi.exchange(&request, &mut response)?;
        let v = u16::from_le_bytes(response[2..].try_into().unwrap());
        ringbuf_entry!(Trace::Read(r, v));

        Ok(v)
    }

    pub fn write(&self, r: Register, v: u16) -> Result<(), SpiError> {
        let cmd = (r as u16 | 0x8000).to_be_bytes(); // Set MSB to indicate write.
        let data = v.to_le_bytes();
        let request = [cmd[0], cmd[1], data[0], data[1]];

        ringbuf_entry!(Trace::Write(r, v));
        self.spi.write(&request[..])?;
        Ok(())
    }

    pub fn write_masked(
        &self,
        r: Register,
        v: u16,
        mask: u16,
    ) -> Result<(), SpiError> {
        let v = (self.read(r)? & !mask) | (v & mask);
        self.write(r, v)
    }

    pub fn enabled(&self) -> Result<bool, SpiError> {
        Ok(self.read(Register::CIDER)? & 0x1 != 0)
    }

    pub fn enable(&self) -> Result<(), SpiError> {
        self.write(Register::CIDER, 1)
    }

    pub fn disable(&self) -> Result<(), SpiError> {
        self.write(Register::CIDER, 0)
    }

    /// Configures the KSZ8463 switch in 100BASE-FX mode.
    pub fn configure(&self, sys: &Sys) {
        sys.gpio_reset(self.nrst).unwrap();
        sys.gpio_configure_output(
            self.nrst,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
        .unwrap();

        // Toggle the reset line
        sleep_for(10); // Reset must be held low for 10 ms after power up
        sys.gpio_set(self.nrst).unwrap();

        // The datasheet recommends a particular combination of diodes and
        // capacitors which dramatically slow down the rise of the reset
        // line, meaning you have to wait for extra long here.
        //
        // Otherwise, the minimum wait time is 1 µs, so 1 ms is fine.
        sleep_for(if self.slow_reset { 150 } else { 1 });

        let id = self.read(Register::CIDER).unwrap();
        assert_eq!(id & !1, 0x8452);
        ringbuf_entry!(Trace::Id(id));

        // Configure for 100BASE-FX operation
        self.enable().unwrap();
        self.write_masked(Register::CFGR, 0x0, 0xc0).unwrap();
        self.write_masked(Register::DSP_CNTRL_6, 0, 0x2000).unwrap();
    }
}
