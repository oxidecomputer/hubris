// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the PCA9538 GPIO expander

use crate::Validate;
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::FromPrimitive;

/// `PinSet` is a bit vector indicating on which pins/ports a given operation is
/// applied.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub struct PinSet(u8);

impl PinSet {
    /// Returns a `PinSet` with the mask bit `index` set.
    #[inline(always)]
    pub const fn pin(index: usize) -> Self {
        Self(1 << index)
    }

    /// Derives a `PinSet` by setting mask bit `index` in addition to the
    /// already set bits.
    #[inline(always)]
    pub const fn and_pin(self, index: usize) -> Self {
        Self(self.0 | 1 << index)
    }
}

/// Derive the union on two `PinSet`s.
impl core::ops::BitOr for PinSet {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Pins in a `PinSet` can be configured as either `Input` or `Output`. Note
/// that even when configured as output, the status of the pin will be reflected
/// in the result of `read(..)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum Mode {
    Input = 0,
    Output = 1,
}

/// Pins in a `PinSet` can be configured with `Normal` or `Inverted` polarity.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum Polarity {
    Normal = 0,
    Inverted = 1,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
enum Register {
    InputPort = 0x00,
    OutputPort = 0x01,
    PolarityInversion = 0x02,
    Configuration = 0x03,
}

pub struct Pca9538 {
    device: I2cDevice,
}

impl Pca9538 {
    pub fn new(device: I2cDevice) -> Self {
        Self { device }
    }

    fn read_reg(&self, register: Register) -> Result<u8, ResponseCode> {
        self.device.read_reg(register as u8)
    }

    fn write_reg(
        &self,
        register: Register,
        value: u8,
    ) -> Result<(), ResponseCode> {
        self.device.write(&[register as u8, value])
    }

    /// Read the state of pins in the `PinSet`. Note that this results reflects
    /// the polarity configuration of the pins.
    pub fn read(&self, pins: PinSet) -> Result<u8, ResponseCode> {
        Ok(self.read_reg(Register::InputPort)? & pins.0)
    }

    /// Set the pins in the `PinSet` to low/high based on the given bool value
    /// of `set`.
    pub fn set_to(&self, pins: PinSet, set: bool) -> Result<(), ResponseCode> {
        let outputs = self.read_reg(Register::OutputPort)?;
        self.write_reg(
            Register::OutputPort,
            if set {
                outputs | pins.0
            } else {
                outputs & !pins.0
            },
        )
    }

    /// Set the pins in the `PinSet`.
    pub fn set(&self, pins: PinSet) -> Result<(), ResponseCode> {
        self.set_to(pins, true)
    }

    /// Reset the pins
    pub fn reset(&self, pins: PinSet) -> Result<(), ResponseCode> {
        self.set_to(pins, false)
    }

    /// Configure the pins in the `PinSet` with the given `Mode` and `Polarity`.
    pub fn set_mode(
        &self,
        pins: PinSet,
        mode: Mode,
        polarity: Polarity,
    ) -> Result<(), ResponseCode> {
        let output_pins = self.read_reg(Register::Configuration)?;
        let inverted_pins = self.read_reg(Register::PolarityInversion)?;

        self.write_reg(
            Register::PolarityInversion,
            match polarity {
                Polarity::Normal => inverted_pins & !pins.0,
                Polarity::Inverted => inverted_pins | pins.0,
            },
        )?;
        self.write_reg(
            Register::Configuration,
            match mode {
                Mode::Input => output_pins | pins.0,
                Mode::Output => output_pins & !pins.0,
            },
        )
    }

    /// Return the polarity of the pins in the `PinSet`.
    pub fn polarity(&self, pins: PinSet) -> Result<u8, ResponseCode> {
        Ok(self.read_reg(Register::PolarityInversion)? & pins.0)
    }
}

impl Validate<ResponseCode> for Pca9538 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        // The device does not carry any identification. Simply performing a
        // read of the Configuration register to determine if the device is
        // present is the best we can do here.
        Pca9538::new(*device)
            .read_reg(Register::Configuration)
            .map(|_| true)
    }
}
