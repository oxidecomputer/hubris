// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

use drv_spi_api::{SpiDevice, SpiError};
use ringbuf::*;

mod registers;
pub use registers::{MIBCounter, Register};

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
pub enum MIBCounterValue {
    None,
    Count(u32),
    CountOverflow(u32),
}

impl Default for MIBCounterValue {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SourcePort {
    Port1,
    Port2,
    Port3,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MacTableEntry {
    /// Specifies that there are no valid entries in the table
    empty: bool,

    /// Number of valid entries in the table, minus 1 (check `empty` as well)
    count: u32,

    /// Two-bit counter for internal aging
    timestamp: u8,

    /// Source port where the FID + MAC is learned
    source: SourcePort,

    /// Filter ID
    fid: u8,

    /// MAC address from the table
    addr: [u8; 6],
}

pub struct Ksz8463 {
    spi: SpiDevice,
}

impl Ksz8463 {
    pub fn new(spi: SpiDevice) -> Self {
        Self { spi }
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

    /// Performs a read-modify-write operation on a PHY register
    #[inline(always)]
    pub fn modify<F>(&self, reg: Register, f: F) -> Result<(), SpiError>
    where
        F: Fn(&mut u16),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
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
    ///
    /// `port` must be 1 or 2 to select the relevant port; otherwise, this
    /// function will panic.
    pub fn read_mib_counter(
        &self,
        port: u8,
        offset: MIBCounter,
    ) -> Result<MIBCounterValue, SpiError> {
        let b = match port {
            1 => 0x0,
            2 => 0x20,
            _ => panic!("Invalid port {}", port),
        };
        // Request counter with given offset.
        self.write(
            Register::IACR,
            (1 << 12) |        // Read
            (0b11 << 10) |     // MIB counter
            offset as u16 + b, // Offset
        )?;

        // Read counter data, looping until the 'valid' bit is 1
        let hi = loop {
            let hi = self.read(Register::IADR5)?;
            if hi & (1 << 14) != 0 {
                break hi;
            }
        };

        let lo = self.read(Register::IADR4)?;
        let value = u32::from(hi) << 16 | u32::from(lo);

        // Determine state of the counter, see p. 184 of datasheet.
        let overflow = ((1 << 31) & value) != 0;
        let value: u32 = value & 0x3fffffff;

        if overflow {
            Ok(MIBCounterValue::CountOverflow(value))
        } else {
            Ok(MIBCounterValue::Count(value))
        }
    }

    /// Reads an entry from the dynamic MAC address table.
    /// `addr` must be < 1024, otherwise this will panic.
    pub fn read_dynamic_mac_table(
        &self,
        addr: u16,
    ) -> Result<MacTableEntry, SpiError> {
        assert!(addr < 1024);
        self.write(Register::IACR, 0x1800 | addr)?;
        // Wait for the "not ready" bit to be cleared
        let d_71_64 = loop {
            let d = self.read(Register::IADR1)?;
            if d & (1 << 15) == 0 {
                break d;
            }
        };
        // This ordering of IADR reads is straight out of the datasheet;
        // heaven forbid they be in a sensible order.
        let d_63_48 = self.read(Register::IADR3)?;
        let d_47_32 = self.read(Register::IADR2)?;
        let d_31_16 = self.read(Register::IADR5)?;
        let d_15_0 = self.read(Register::IADR4)?;

        let empty = (d_71_64 & 4) != 0;

        // Awkwardly stradling the line between two words...
        let count = (d_71_64 as u32 & 0b11) << 8 | (d_63_48 as u32 & 0xF0) >> 8;

        let timestamp = (d_63_48 >> 6) as u8 & 0b11;
        let source = match (d_63_48 >> 4) & 0b11 {
            0 => SourcePort::Port1,
            1 => SourcePort::Port2,
            2 => SourcePort::Port3,
            _ => panic!("Invalid port"),
        };
        let fid = (d_63_48 & 0b1111) as u8;

        let addr = [
            (d_47_32 >> 8) as u8,
            d_47_32 as u8,
            (d_31_16 >> 8) as u8,
            d_31_16 as u8,
            (d_15_0 >> 8) as u8,
            d_15_0 as u8,
        ];

        Ok(MacTableEntry {
            empty,
            count,
            timestamp,
            source,
            fid,
            addr,
        })
    }

    /// Configures the KSZ8463 switch in 100BASE-FX mode.
    pub fn configure(&self) -> Result<(), SpiError> {
        let id = self.read(Register::CIDER)?;
        assert_eq!(id & !1, 0x8452);
        ringbuf_entry!(Trace::Id(id));

        // Configure for 100BASE-FX operation
        self.modify(Register::CFGR, |r| *r &= !0xc0)?;
        self.modify(Register::DSP_CNTRL_6, |r| *r &= !0x2000)?;

        self.enable()
    }
}
