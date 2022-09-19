// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the DS18B20 Programmable Resolution 1-Wire Digital Thermometer
//!
//! This is a basic driver for converting and reading the temperature on
//! a DS18B20.  This driver is naive in several regards:
//!
//! - It assumes that the bus is shared with other devices.  If the bus
//!   were to only consist of a single device, `SkipROM` could be used
//!   in lieu of `MatchROM`
//!
//! - It can only have one device perform a conversion (via `MatchROM`)
//!   rather than having all devices begin a concurrent conversion
//!   (via `SkipROM`)
//!
//! - It makes no use of the alarm functionality
//!
//! - It doesn't allow the resolution to be altered (default is 12-bit)
//!
//! - It makes no attempt to assure that a read following a temperature
//!   conversion has waited sufficiently for the conversion to latch.
//!   It is up to the caller to assure this has waited sufficiently, or
//!   to otherwise understand that the read may result in stale data.
//!   Maximun conversion times for this part, per the datasheet:
//!
//!   - 9-bit resolution: 93.75 ms
//!   - 10-bit resolution: 187.5 ms
//!   - 11-bit resolution: 375 ms
//!   - 12-bit resolution: 750 ms
//!
//!   (In practice, ~650 ms conversion times for 12-bit resolution have
//!   been seen.)

use userlib::units::*;

/// A DS18B20 command
#[allow(dead_code)]
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Command {
    ConvertT = 0x44,
    WriteScratchpad = 0x4e,
    ReadScratchpad = 0xbe,
    CopyScratchpad = 0x48,
    RecallESquared = 0xb8,
    ReadPowerSupply = 0xb4,
}

/// A structure representing a single DS18B20 device
#[derive(Copy, Clone)]
pub struct Ds18b20 {
    pub id: drv_onewire::Identifier,
}

impl core::fmt::Display for Ds18b20 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ds18b20 {:#014x}", (self.id >> 8) & 0xff_ffff_ffff)
    }
}

//
// Convert as per Figure 4 in the datasheet.
//
fn convert(lsb: u8, msb: u8) -> Celsius {
    Celsius(f32::from(i16::from(msb) << 8 | i16::from(lsb)) / 16.0)
}

impl Ds18b20 {
    /// Create a new DS18B20 instance given an ID. If the family code
    /// doesn't match the DS18B20 family code, `None` is returned.
    pub fn new(id: drv_onewire::Identifier) -> Option<Self> {
        if drv_onewire::family(id) == Some(drv_onewire::Family::DS18B20) {
            Some(Self { id })
        } else {
            None
        }
    }

    /// Issues a conversion.  It is the responsibility for the caller to
    /// wait long enough for the conversion to succeed before reading it
    /// (or to understand that a stale value will be possible).
    pub fn convert_temperature<T>(
        &self,
        reset: impl Fn() -> Result<(), T>,
        write_byte: impl Fn(u8) -> Result<(), T>,
    ) -> Result<(), T> {
        reset()?;
        write_byte(drv_onewire::Command::MatchROM as u8)?;

        for i in 0..8 {
            write_byte(((self.id >> (i * 8)) & 0xff) as u8)?;
        }

        write_byte(Command::ConvertT as u8)?;

        Ok(())
    }

    /// Read the temperature.  If insufficient time has elapsed since the
    /// `convert_temperature` call, this will return whatever temperature
    /// data was latched most recently.
    pub fn read_temperature<T>(
        &self,
        reset: impl Fn() -> Result<(), T>,
        write_byte: impl Fn(u8) -> Result<(), T>,
        read_byte: impl Fn() -> Result<u8, T>,
    ) -> Result<Celsius, T> {
        reset()?;

        write_byte(drv_onewire::Command::MatchROM as u8)?;

        for i in 0..8 {
            write_byte(((self.id >> (i * 8)) & 0xff) as u8)?;
        }

        write_byte(Command::ReadScratchpad as u8)?;

        let lsb = read_byte()?;
        let msb = read_byte()?;

        Ok(convert(lsb, msb))
    }
}
