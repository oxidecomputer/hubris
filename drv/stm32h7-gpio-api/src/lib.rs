// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the STM32H7 GPIO server.

#![no_std]

use userlib::*;

pub use drv_stm32xx_gpio_common::{
    Alternate, Mode, OutputType, PinSet, Port, Pull, Speed,
};

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum GpioError {
    BadArg = 2,
}

impl From<GpioError> for u32 {
    fn from(rc: GpioError) -> Self {
        rc as u32
    }
}

impl From<u32> for GpioError {
    fn from(x: u32) -> Self {
        match x {
            2 => GpioError::BadArg,
            _ => panic!(),
        }
    }
}

impl Gpio {
    /// Configures a subset of pins in a GPIO port.
    ///
    /// This is the raw operation, which can be useful if you're doing something
    /// unusual, but see `configure_output`, `configure_input`, and
    /// `configure_alternate` for the common cases.
    pub fn configure(
        &self,
        port: Port,
        pins: u16,
        mode: Mode,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        let packed_attributes = mode as u16
            | (output_type as u16) << 2
            | (speed as u16) << 3
            | (pull as u16) << 5
            | (af as u16) << 7;

        self.configure_raw(port, pins, packed_attributes)
    }

    /// Configures the pins in `PinSet` as high-impedance digital inputs, with
    /// optional pull resistors.
    pub fn configure_input(
        &self,
        pinset: PinSet,
        pull: Pull,
    ) -> Result<(), GpioError> {
        self.configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Input,
            OutputType::PushPull, // doesn't matter
            Speed::High,          // doesn't matter
            pull,
            Alternate::AF0, // doesn't matter
        )
    }

    /// Configures the pins in `PinSet` as digital GPIO outputs, either
    /// push-pull or open-drain, with adjustable slew rate filtering and pull
    /// resistors.
    pub fn configure_output(
        &self,
        pinset: PinSet,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
    ) -> Result<(), GpioError> {
        self.configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Output,
            output_type,
            speed,
            pull,
            Alternate::AF0, // doesn't matter
        )
    }

    /// Configures the pins in `PinSet` in the given alternate function.
    ///
    /// If the alternate function is an output, the `OutputType` and `Speed`
    /// settings apply. If it's an input, they don't matter; consider using
    /// `configure_alternate_input` in that case.
    pub fn configure_alternate(
        &self,
        pinset: PinSet,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        self.configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Alternate,
            output_type,
            speed,
            pull,
            af,
        )
    }

    /// Configures the pins in `PinSet` in the given alternate function, which
    /// should be an input.
    ///
    /// This calls `configure_alternate` passing arbitrary values for
    /// `OutputType` and `Speed`. This is appropriate for inputs, but not for
    /// outputs or bidirectional signals.
    pub fn configure_alternate_input(
        &self,
        pinset: PinSet,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        self.configure_alternate(
            pinset,
            OutputType::OpenDrain,
            Speed::High,
            pull,
            af,
        )
    }

    /// Sets some pins high.
    pub fn set(&self, pinset: PinSet) -> Result<(), GpioError> {
        self.set_reset(pinset.port, pinset.pin_mask, 0)
    }

    /// Resets some pins low.
    pub fn reset(&self, pinset: PinSet) -> Result<(), GpioError> {
        self.set_reset(pinset.port, 0, pinset.pin_mask)
    }

    /// Sets some pins based on `flag` -- high if `true`, low if `false`.
    pub fn set_to(&self, pinset: PinSet, flag: bool) -> Result<(), GpioError> {
        self.set_reset(
            pinset.port,
            if flag { pinset.pin_mask } else { 0 },
            if flag { 0 } else { pinset.pin_mask },
        )
    }

    pub fn read(&self, pinset: PinSet) -> Result<u16, GpioError> {
        Ok(self.read_input(pinset.port)? & pinset.pin_mask)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
