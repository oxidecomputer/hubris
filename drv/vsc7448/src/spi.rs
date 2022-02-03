// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::VscError;
use drv_spi_api::SpiDevice;
use ringbuf::*;
use vsc7448_pac::{types::RegisterAddress, Vsc7448};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Read { addr: u32, value: u32 },
    Write { addr: u32, value: u32 },
}

/// This indicates how many bytes we pad between (writing) the address bytes
/// and (reading) data back, during SPI transactions to the VSC7448.  See
/// section 5.5.2 for details.  1 padding byte should be good up to 6.5 MHz
/// SPI clock.
pub const SPI_NUM_PAD_BYTES: usize = 1;

// Flags to tune ringbuf output while developing
const DEBUG_TRACE_SPI: u8 = 1 << 0;
const DEBUG_MASK: u8 = 0;

/// Writes the given value to the ringbuf if allowed by the global `DEBUG_MASK`
macro_rules! ringbuf_entry_masked {
    ($mask:ident, $value:expr) => {
        if (DEBUG_MASK & $mask) != 0 {
            ringbuf_entry!($value);
        }
    };
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// Helper struct to read and write from the VSC7448 over SPI
pub struct Vsc7448Spi(SpiDevice);
impl Vsc7448Spi {
    pub fn new(spi: SpiDevice) -> Self {
        Self(spi)
    }
    /// Reads from a VSC7448 register.  The register must be in the switch
    /// core register block, i.e. having an address in the range
    /// 0x71000000-0x72000000.
    pub fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        if reg.addr < 0x71000000 || reg.addr > 0x72000000 {
            return Err(VscError::BadRegAddr(reg.addr));
        }
        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let data: [u8; 3] = [
            ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
        ];

        // We read back 7 + padding bytes in total:
        // - 3 bytes of address
        // - Some number of padding bytes
        // - 4 bytes of data
        const SIZE: usize = 7 + SPI_NUM_PAD_BYTES as usize;
        let mut out = [0; SIZE];
        self.0.exchange(&data[..], &mut out[..])?;
        let value = (out[SIZE - 1] as u32)
            | ((out[SIZE - 2] as u32) << 8)
            | ((out[SIZE - 3] as u32) << 16)
            | ((out[SIZE - 4] as u32) << 24);

        ringbuf_entry_masked!(
            DEBUG_TRACE_SPI,
            Trace::Read {
                addr: reg.addr,
                value
            }
        );
        // The VSC7448 is configured to return 0x88888888 if a register is
        // read too fast.  Reading takes place over SPI: we write a 3-byte
        // address, then read 4 bytes of data; based on SPI speed, we may
        // need to configure padding bytes in between the address and
        // returning data.
        //
        // This is controlled by setting DEVCPU_ORG:IF_CFGSTAT.IF_CFG in
        // Vsc7448::init(), then by padding bytes in the `out` arrays in
        // [read] and [write].
        //
        // Therefore, we should only read "too fast" if someone has modified
        // the SPI speed without updating the padding byte, which should
        // never happen in well-behaved code.
        //
        // If we see this sentinel value, then we check
        // DEVCPU_ORG:IF_CFGSTAT.IF_STAT.  If that bit is set, then the sentinel
        // value _actually_ indicates an error (and not just an unfortunate
        // coincidence).
        if value == 0x88888888 {
            // Return immediately if we got an invalid read sentinel while
            // reading IF_CFGSTAT itself.  This check also protects us from a
            // stack overflow.
            let if_cfgstat = Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT();
            if reg.addr == if_cfgstat.addr {
                return Err(VscError::InvalidRegisterReadNested);
            }
            // This read should _never_ fail for timing reasons because the
            // DEVCPU_ORG register block can be accessed faster than all other
            // registers (section 5.3.2 of the datasheet).
            let v = self.read(if_cfgstat)?;
            if v.if_stat() == 1 {
                return Err(VscError::InvalidRegisterRead(reg.addr));
            }
        }
        Ok(value.into())
    }

    /// Writes to a VSC7448 register.  This will overwrite the entire register;
    /// if you want to modify it, then use [Self::modify] instead.
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000.
    pub fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        if reg.addr < 0x71000000 || reg.addr > 0x72000000 {
            return Err(VscError::BadRegAddr(reg.addr));
        }

        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let value: u32 = value.into();
        let data: [u8; 7] = [
            0x80 | ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            ((value >> 24) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ];

        ringbuf_entry_masked!(
            DEBUG_TRACE_SPI,
            Trace::Write {
                addr: reg.addr,
                value: value.into()
            }
        );
        self.0.write(&data[..])?;
        Ok(())
    }

    /// Writes to a port mask, which is assumed to be a pair of adjacent
    /// registers representing all 53 ports.
    pub fn write_port_mask<T>(
        &self,
        mut reg: RegisterAddress<T>,
        value: u64,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
    {
        self.write(reg, ((value & 0xFFFFFFFF) as u32).into())?;
        reg.addr += 4; // Good luck!
        self.write(reg, (((value >> 32) as u32) & 0x1FFFFF).into())
    }

    /// Performs a write operation on the given register, where the value is
    /// calculated by calling f(0).  This is helpful as a way to reduce manual
    /// type information.
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000.
    pub fn write_with<T, F>(
        &self,
        reg: RegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
        F: Fn(&mut T),
    {
        let mut data = 0.into();
        f(&mut data);
        self.write(reg, data)
    }

    /// Performs a read-modify-write operation on a VSC7448 register
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000.
    pub fn modify<T, F>(
        &self,
        reg: RegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
    }

    /// Reads from a specific SERDES6G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    pub fn serdes6g_read(&self, instance: u8) -> Result<(), VscError> {
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.write_with(addr, |r| {
            r.set_serdes6g_rd_one_shot(1);
            r.set_serdes6g_addr(1 << instance);
        })?;
        for _ in 0..32 {
            if self.read(addr)?.serdes6g_rd_one_shot() != 1 {
                return Ok(());
            }
        }
        Err(VscError::Serdes6gReadTimeout { instance })
    }

    /// Writes to a specific SERDES6G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    pub fn serdes6g_write(&self, instance: u8) -> Result<(), VscError> {
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.write_with(addr, |r| {
            r.set_serdes6g_wr_one_shot(1);
            r.set_serdes6g_addr(1 << instance);
        })?;
        for _ in 0..32 {
            if self.read(addr)?.serdes6g_wr_one_shot() != 1 {
                return Ok(());
            }
        }
        Err(VscError::Serdes6gWriteTimeout { instance })
    }

    /// Writes to a specific SERDES1G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    pub fn serdes1g_read(&self, instance: u8) -> Result<(), VscError> {
        let addr = Vsc7448::HSIO().MCB_SERDES1G_CFG().MCB_SERDES1G_ADDR_CFG();
        self.write_with(addr, |r| {
            r.set_serdes1g_rd_one_shot(1);
            r.set_serdes1g_addr(1 << instance);
        })?;
        for _ in 0..32 {
            if self.read(addr)?.serdes1g_rd_one_shot() != 1 {
                return Ok(());
            }
        }
        Err(VscError::Serdes1gReadTimeout { instance })
    }

    /// Reads from a specific SERDES1G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    pub fn serdes1g_write(&self, instance: u8) -> Result<(), VscError> {
        let addr = Vsc7448::HSIO().MCB_SERDES1G_CFG().MCB_SERDES1G_ADDR_CFG();
        self.write_with(addr, |r| {
            r.set_serdes1g_wr_one_shot(1);
            r.set_serdes1g_addr(1 << instance);
        })?;
        for _ in 0..32 {
            if self.read(addr)?.serdes1g_wr_one_shot() != 1 {
                return Ok(());
            }
        }
        Err(VscError::Serdes1gWriteTimeout { instance })
    }
}
