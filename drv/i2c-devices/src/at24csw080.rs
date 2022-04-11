// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the AT24CSW080/4 I2C EEPROM

use drv_i2c_api::*;
use userlib::hl::sleep_for;
use zerocopy::{AsBytes, FromBytes};

/// The AT23CSW080/4 is an I2C EEPROM used as the FRU ID. It includes 8-Kbit of
/// memory (arranged as 1024 x 8), software write protection, a 256-bit
/// Security Register, and various other useful features.
///
/// Write functions are conservative with respect to timing, waiting the
/// entire 5 ms (maximum write cycle time) before returning. If this proves
/// limiting, it may be possible to use Acknowledge Polling (section 7.3 of the
/// datasheet). This would use NAK to indicate that the device is not present,
/// which has more room for confusion.
pub struct At24csw080 {
    /// We store a `DeviceHandle` instead of an `I2cDevice` to force users
    /// of this API to call either the `eeprom()` or `registers()`, since
    /// the I2C address must be dynamically generated.
    device: handle::DeviceHandle,
}

/// Word address for the write-protect register
///
/// According to the datasheet, this is `11xx_xxxx`; we're filling the ignored
/// bits with zeros.
const WPR_WORD_ADDR: u8 = 0b1100_0000;

const WPR_WRITE: u8 = 0b0100_0000;
const WPR_ENABLE: u8 = 0b0000_1000;
const WPR_PERMANENTLY_LOCK: u8 = 0b0010_0001;

/// Word address for the security register.
///
/// According to the datasheet, this is `0110_xxxx`; we're filling the ignored
/// bits with all zeros.
const SECURITY_REGISTER_WORD_ADDR: u8 = 0b0110_0000;

impl core::fmt::Display for At24csw080 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "at24csw080: {}", &self.device)
    }
}

impl At24csw080 {
    pub fn new(dev: I2cDevice) -> Self {
        Self {
            device: handle::DeviceHandle::new(dev),
        }
    }

    /// Reads a single value of type `V` from the EEPROM.
    ///
    /// `addr` and `addr + sizeof(V)` must be below 1024; otherwise this
    /// function will panic.
    pub fn read<V: Default + AsBytes + FromBytes>(
        &self,
        addr: u16,
    ) -> Result<V, ResponseCode> {
        assert!(addr as usize + core::mem::size_of::<V>() < 1024);
        self.device.eeprom(addr).read_reg(addr as u8)
    }

    /// Writes a single byte to the EEPROM at the given address
    ///
    /// On success, sleeps for 5 ms (the EEPROM's write cycle time) before
    /// returning `Ok(())`
    pub fn write_byte(&self, addr: u16, val: u8) -> Result<(), ResponseCode> {
        // Write the low byte of the address followed by the actual value
        let buffer = [addr as u8, val];
        self.device.eeprom(addr).write(&buffer)?;
        sleep_for(5);
        Ok(())
    }

    /// Writes up to 16 bytes to a page.
    ///
    /// `addr` must be 16-byte aligned (i.e. the four lowest bits must be 0)
    /// and less than 1024.
    ///
    /// `buf` must be 16 bytes or less.
    ///
    /// This function will panic if either of those conditions is violated
    ///
    /// On success, sleeps for 5 ms (the EEPROM's write cycle time) before
    /// returning `Ok(())`
    fn write_page(&self, addr: u16, buf: &[u8]) -> Result<(), ResponseCode> {
        assert!(addr & 0b1111 == 0);
        assert!(buf.len() <= 16);
        let mut out: [u8; 17] = [0; 17];

        // Write the low byte of the address followed by up to 16 bytes of
        // buffer data.
        out[0] = addr as u8;
        out[1..=buf.len()].copy_from_slice(buf);
        self.device.eeprom(addr).write(&out)?;
        sleep_for(5);
        Ok(())
    }

    /// Writes a buffer to the EEPROM at the specified address, taking
    /// advantage of page writes when possible.
    ///
    /// `addr` and `addr + buf.len()` must be < 1024; otherwise, this function
    /// panics.
    fn write_buffer(
        &self,
        mut addr: u16,
        mut buf: &[u8],
    ) -> Result<(), ResponseCode> {
        assert!(addr as usize + buf.len() < 1024);

        // Write single bytes until we reach an aligned address or run out
        // of buffer data to write. Note that the datasheet says we need
        // address bits A9-A3 to be the same for the write, but that doesn't
        // make sense: if we can write 16 bytes, then A3 is by definition
        // going to change. Instead, we look for an address that is aligned
        // to a 16-byte boundary.
        while (addr & 0b1111) != 0 && !buf.is_empty() {
            self.write_byte(addr, buf[0])?;
            buf = &buf[1..];
            addr += 1;
        }
        for chunk in buf.chunks(16) {
            self.write_page(addr, chunk)?;
            addr += chunk.len() as u16;
        }
        Ok(())
    }

    /// Serializes the given value to bytes then writes it to the given
    /// address.
    ///
    /// `addr` and `addr + sizeof(V)` must be < 1024; otherwise this function
    /// panics.
    pub fn write<V: Default + AsBytes + FromBytes>(
        &self,
        addr: u16,
        val: V,
    ) -> Result<(), ResponseCode> {
        self.write_buffer(addr, val.as_bytes())
    }

    /// Reads a byte from the 32-byte security register.
    ///
    /// The security register has 16 read-only bytes (addresses 0-15), followed
    /// by 16 user-programmable bytes.
    ///
    /// Panics if `addr >= 32`
    pub fn read_security_register_byte(
        &self,
        addr: u8,
    ) -> Result<u8, ResponseCode> {
        assert!(addr < 32);
        let reg_addr = 0b1000_0000 | addr;
        self.device.registers().read_reg(reg_addr)
    }
    /// Writes a byte to the user-programmable region of the the 32-byte
    /// security register.
    ///
    /// Panics if `addr < 16` (the read-only region) or `addr >= 32`
    ///
    /// On success, sleeps for 5 ms (the EEPROM's write cycle time) before
    /// returning `Ok(())`
    pub fn write_security_register_byte(
        &self,
        addr: u8,
        val: u8,
    ) -> Result<(), ResponseCode> {
        assert!(addr >= 16);
        assert!(addr < 32);
        let reg_addr = 0b1000_0000 | addr;
        let cmd = [reg_addr, val];
        self.device.registers().write(&cmd)?;

        // It's unclear if this is needed for the registers (vs the EEPROM),
        // but better safe than sorry.
        sleep_for(5);
        Ok(())
    }

    /// Checks whether the security register is locked. Returns `true` if
    /// the security register is locked and `false` otherwise.
    ///
    /// This may incorrectly return `true` if the chip is not present.
    pub fn is_security_register_locked(&self) -> Result<bool, ResponseCode> {
        // Write a single byte (after the device address)
        let cmd = [SECURITY_REGISTER_WORD_ADDR];
        let out = self.device.registers().write(&cmd);

        // The device NAKs at the end of the word address byte if the
        // security lock is already set. Unfortunately, in the I2C driver,
        // it's hard to tell whether the address byte went through or not...
        match out {
            Ok(()) => Ok(false),
            Err(ResponseCode::NoDevice) => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Locks the security register. *THIS CANNOT BE UNDONE.*
    pub fn permanently_lock_security_register(
        &self,
    ) -> Result<(), ResponseCode> {
        let cmd = [SECURITY_REGISTER_WORD_ADDR, 0];
        self.device.registers().write(&cmd)
    }

    /// Enables EEPROM write protection. This can be undone by calling
    /// `disable_write_protection`.
    pub fn enable_eeprom_write_protection(
        &self,
        b: WriteProtectBlock,
    ) -> Result<(), ResponseCode> {
        let cmd = [WPR_WORD_ADDR, (b.wpb() << 1) | WPR_WRITE | WPR_ENABLE];
        self.device.registers().write(&cmd)
    }

    /// Disables EEPROM write protection (assuming it wasn't set permanently)
    pub fn disable_eeprom_write_protection(&self) -> Result<(), ResponseCode> {
        let cmd = [WPR_WORD_ADDR, WPR_WRITE];
        self.device.registers().write(&cmd)
    }

    /// Enables EEPROM write protection. *THIS CANNOT BE UNDONE.*
    pub fn permanently_enable_eeprom_write_protection(
        &self,
        b: WriteProtectBlock,
    ) -> Result<(), ResponseCode> {
        let cmd = [
            WPR_WORD_ADDR,
            (b.wpb() << 1) | WPR_WRITE | WPR_ENABLE | WPR_PERMANENTLY_LOCK,
        ];
        self.device.registers().write(&cmd)
    }
}

/// Represents a range of the EEPROM that can be write-protected.
pub enum WriteProtectBlock {
    Upper256Bytes,
    Upper512Bytes,
    Upper768Bytes,
    AllMemory,
}

impl WriteProtectBlock {
    /// Returns the two-bit WPB value corresponding to a particular region
    fn wpb(&self) -> u8 {
        match self {
            Self::Upper256Bytes => 0b00,
            Self::Upper512Bytes => 0b01,
            Self::Upper768Bytes => 0b10,
            Self::AllMemory => 0b11,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Small module to encapsulate the `DeviceHandle` and prevent users from
/// accessing its inner `I2cDevice`.
mod handle {
    use super::*;

    /// The AT24CSW080 uses bits 0 and 1 of the 7-bit I2C device address as
    /// high bits for the EEPROM address.  In addition, it uses bit 3 to
    /// indicate whether we are addressing the EEPROM or security and write
    /// protection registers.
    ///
    /// This means we can't have a single address and be done with it; we must
    /// generate the address on a per-operation basis.
    ///
    /// The `DeviceHandle` forces users to explicitly build an `I2cDevice`
    /// based on EEPROM address and EEPROM vs registers.
    ///
    /// The address stored in the inner `I2cDevice` should have all those bits
    /// cleared, i.e. it must be 1010_000 for the AT23CSW080 or 1010_100
    /// for the AT23CSW084.
    pub(super) struct DeviceHandle(I2cDevice);
    impl DeviceHandle {
        pub(super) fn new(dev: I2cDevice) -> Self {
            Self(dev)
        }

        /// Returns an `I2cDevice` to read or write the EEPROM at the given
        /// address.  This device has to be dynamically generated because the
        /// I2C device address includes two EEPROM address bits.
        pub(super) fn eeprom(&self, addr: u16) -> I2cDevice {
            assert!(addr < 1024);
            let a_9_8 = ((addr >> 8) & 0b11) as u8;
            I2cDevice {
                address: self.0.address | a_9_8,
                ..self.0
            }
        }

        /// Returns an `I2cDevice` to read and write the security registers
        /// and write protection registers.
        pub(super) fn registers(&self) -> I2cDevice {
            I2cDevice {
                address: self.0.address | (1 << 3),
                ..self.0
            }
        }
    }
    impl core::fmt::Display for DeviceHandle {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            self.0.fmt(f)
        }
    }
}
