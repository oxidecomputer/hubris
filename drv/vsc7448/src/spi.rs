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
pub struct Vsc7448Spi(pub SpiDevice);
impl Vsc7448Spi {
    /// Reads from a VSC7448 register
    pub fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);
        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let data: [u8; 3] = [
            ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
        ];

        // We read back 8 bytes in total:
        // - 3 bytes of address
        // - 1 byte of padding
        // - 4 bytes of data
        let mut out = [0; 8];
        self.0.exchange(&data[..], &mut out[..])?;
        let value = (out[7] as u32)
            | ((out[6] as u32) << 8)
            | ((out[5] as u32) << 16)
            | ((out[4] as u32) << 24);

        ringbuf_entry_masked!(
            DEBUG_TRACE_SPI,
            Trace::Read {
                addr: reg.addr,
                value
            }
        );
        if value == 0x88888888 {
            panic!("suspicious read");
        }
        Ok(value.into())
    }

    /// Writes to a VSC7448 register.  This will overwrite the entire register;
    /// if you want to modify it, then use [Self::modify] instead.
    pub fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);

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
    pub fn serdes6g_read(&self, instance: u32) -> Result<(), VscError> {
        let mut reg: vsc7448_pac::hsio::mcb_serdes6g_cfg::MCB_SERDES6G_ADDR_CFG =
            0.into();
        reg.set_serdes6g_rd_one_shot(1);
        reg.set_serdes6g_addr(1 << instance);
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.write(addr, reg)?;
        for _ in 0..32 {
            if self.read(addr)?.serdes6g_rd_one_shot() != 1 {
                return Ok(());
            }
        }
        return Err(VscError::Serdes6gReadTimeout { instance });
    }

    /// Reads from a specific SERDES6G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    pub fn serdes6g_write(&self, instance: u32) -> Result<(), VscError> {
        let mut reg: vsc7448_pac::hsio::mcb_serdes6g_cfg::MCB_SERDES6G_ADDR_CFG =
            0.into();
        reg.set_serdes6g_wr_one_shot(1);
        reg.set_serdes6g_addr(1 << instance);
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.write(addr, reg)?;
        for _ in 0..32 {
            if self.read(addr)?.serdes6g_wr_one_shot() != 1 {
                return Ok(());
            }
        }
        return Err(VscError::Serdes6gWriteTimeout { instance });
    }
}
