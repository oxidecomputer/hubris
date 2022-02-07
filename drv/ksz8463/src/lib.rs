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

/// Data from a management information base (MIB) counter on the chip,
/// used to monitor port activity for network management.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MIBCounter {
    Invalid,
    Count(u32),
    CountOverflow(u32),
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(non_camel_case_types)]
pub enum Register {
    /// Chip ID and enable register
    CIDER = 0x0,
    /// Switch global control register 1
    SGCR1 = 0x2,
    /// Switch global control register 2
    SGCR2 = 0x4,
    /// Switch global control register 3
    SGCR3 = 0x6,
    /// Switch global control register 6
    SGCR6 = 0xc,
    /// Switch global control register 7
    SGCR7 = 0xe,
    /// MAC address register 1
    MACAR1 = 0x10,
    /// MAC address register 2
    MACAR2 = 0x12,
    /// MAC address register 3
    MACAR3 = 0x14,

    /// Indirect access data register 4
    IADR4 = 0x02c,
    /// Indirect access data register 5
    IADR5 = 0x02e,
    /// Indirect access control register
    IACR = 0x030,

    /// PHY 1 and MII basic control register
    P1MBCR = 0x4c,
    /// PHY 1 and MII basic status register
    P1MBSR = 0x4e,

    /// PHY 2 and MII basic control register
    P2MBCR = 0x58,
    /// PHY 2 and MII basic status register
    P2MBSR = 0x5a,

    /// PHY 1 special control and status register
    P1PHYCTRL = 0x066,
    /// PHY 2 special control and status register
    P2PHYCTRL = 0x06a,

    /// Configuration status and serial bus mode register
    CFGR = 0xd8,

    /// DSP control 1 register
    DSP_CNTRL_6 = 0x734,
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

    fn pack_addr(address: u16) -> u16 {
        // This chip has a bizarre addressing scheme where you specify the
        // address with 4-byte resolution (i.e. masking off the lower two bits
        // of the address), then use four flags to indicate which bytes within
        // that region you actually want.
        let b = match address & 0b11 {
            0 => 0b0011,
            2 => 0b1100,
            _ => panic!("Address must be 2-byte aligned"),
        };
        ((address & 0b1111111100) << 4) | (b << 2)
    }

    pub fn read(&self, r: Register) -> Result<u16, SpiError> {
        let cmd = Self::pack_addr(r as u16).to_be_bytes();
        let mut response = [0; 4];

        self.spi.exchange(&cmd, &mut response)?;
        let v = u16::from_le_bytes(response[2..].try_into().unwrap());
        ringbuf_entry!(Trace::Read(r, v));

        Ok(v)
    }

    pub fn write(&self, r: Register, v: u16) -> Result<(), SpiError> {
        // Yes, the address is big-endian while the data is little-endian.
        //
        // I don't make the rules.
        let mut request: [u8; 4] = [0; 4];
        request[..2].copy_from_slice(&Self::pack_addr(r as u16).to_be_bytes());
        request[2..].copy_from_slice(&v.to_le_bytes());
        request[0] |= 0x80; // Set MSB to indicate write.

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

    /// Reads a management information base (MIB) counter
    pub fn read_mib_counter(&self, offset: u8) -> Result<MIBCounter, SpiError> {
        // Request counter with given offset.
        self.write(Register::IACR, 0x1c00 | offset as u16)?;

        // Read counter data.
        let hi = self.read(Register::IADR5)?;
        let lo = self.read(Register::IADR4)?;

        // Determine state of the counter, see p. 184 of datasheet.
        let valid = ((1 << 14) & hi) == 0;
        let overflow = ((1 << 15) & hi) != 0;
        let value: u32 = (((hi as u32) << 16) | lo as u32) & (3 << 30);

        if !valid {
            Ok(MIBCounter::Invalid)
        } else if !overflow {
            Ok(MIBCounter::Count(value))
        } else {
            Ok(MIBCounter::CountOverflow(value))
        }
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
        self.write_masked(Register::CFGR, 0x0, 0xc0).unwrap();
        self.write_masked(Register::DSP_CNTRL_6, 0, 0x2000).unwrap();

        // Enable port 1 near-end loopback (XXX delete this before connecting
        // to the rest of the management network)
        self.write_masked(Register::P1PHYCTRL, 1 << 1, 1 << 1)
            .unwrap();

        self.enable().unwrap();
    }
}
