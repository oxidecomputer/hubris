// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the STM32L0 SYS server.

#![no_std]

use unwrap_lite::UnwrapLite;
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum RccError {
    NoSuchPeripheral = 1,
}

impl From<u32> for RccError {
    fn from(x: u32) -> Self {
        match x {
            1 => RccError::NoSuchPeripheral,
            _ => panic!(),
        }
    }
}

impl From<RccError> for u16 {
    fn from(x: RccError) -> Self {
        x as u16
    }
}

impl Sys {
    /// Requests that the clock to a peripheral be turned on.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn enable_clock(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.enable_clock_raw(peripheral as u32).unwrap_lite()
    }

    /// Requests that the clock to a peripheral be turned off.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn disable_clock(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.disable_clock_raw(peripheral as u32).unwrap_lite()
    }

    /// Requests that the reset line to a peripheral be asserted.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn enter_reset(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.enter_reset_raw(peripheral as u32).unwrap_lite()
    }

    /// Requests that the reset line to a peripheral be deasserted.
    ///
    /// This operation is idempotent and will be retried automatically should
    /// the RCC server crash while processing it.
    ///
    /// # Panics
    ///
    /// If the RCC server has died.
    pub fn leave_reset(&self, peripheral: Peripheral) {
        // We are unwrapping here because the RCC server should not return
        // NoSuchPeripheral for a valid member of the Peripheral enum.
        self.leave_reset_raw(peripheral as u32).unwrap_lite()
    }
}

//
// A few macros for purposes of defining the Peripheral enum in terms that our
// driver is expecting:
//
// - RCC_IOPENR[31:0] and RCC_IOPRSTR[31:0] are indices 31-0.
// - RCC_AHBENR[31:0] and RCC_AHBRSTR[31:0] are indices 63-32.
// - RCC_APBENR1[31:0] and RCC_APBRSTR1[31:0] are indices 95-64.
// - RCC_APBENR2[31:0] and RCC_APBRSTR2[31:0] are indices 127-96.
//
macro_rules! iop {
    ($bit:literal) => {
        (0 * 32) + $bit
    };
}

macro_rules! ahb {
    ($bit:literal) => {
        (1 * 32) + $bit
    };
}

macro_rules! apb1 {
    ($bit:literal) => {
        (2 * 32) + $bit
    };
}

macro_rules! apb2 {
    ($bit:literal) => {
        (3 * 32) + $bit
    };
}

/// Peripheral numbering.
///
/// Peripheral bit numbers per the STM32G0 documentation, starting at section:
///
///    STM32L0 PART     MANUAL      SECTION
///    L0x3             RM0367      7.3.8 (RCC_IOPRSTR)
///
/// These are in the order that they appear in the documentation.   This is
/// the union of all STM32L0 peripherals; not all peripherals will exist on
/// all variants!
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u32)]
pub enum Peripheral {
    GpioH = iop!(7),
    GpioE = iop!(4),
    GpioD = iop!(3),
    GpioC = iop!(2),
    GpioB = iop!(1),
    GpioA = iop!(0),

    Cryp = ahb!(24),
    Rng = ahb!(20),
    Tsc = ahb!(16),
    Crc = ahb!(12),
    Mif = ahb!(8),
    Dma = ahb!(0),

    Dbg = apb2!(22),
    Usart1 = apb2!(14),
    Spi1 = apb2!(12),
    Adc = apb2!(9),
    Fw = apb2!(7),
    Tim22 = apb2!(5),
    Tim21 = apb2!(2),
    Syscfg = apb2!(0),

    LpTim1 = apb1!(31),
    I2c3 = apb1!(30),
    Dac = apb1!(29),
    Pwr = apb1!(28),
    Crs = apb1!(27),
    Usb = apb1!(23),
    I2c2 = apb1!(22),
    I2c1 = apb1!(21),
    Usart5 = apb1!(20),
    Usart4 = apb1!(19),
    LpUart1 = apb1!(18),
    Usart2 = apb1!(17),
    Spi2 = apb1!(14),
    Wwdg = apb1!(11),
    Lcd = apb1!(9),
    Tim7 = apb1!(5),
    Tim6 = apb1!(4),
    Tim3 = apb1!(1),
    Tim2 = apb1!(0),
}

/// Enumeration of all GPIO ports on the STM32L0 series. Note that not all these
/// ports may be externally exposed on your device/package. We do not check this
/// at compile time.
#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum Port {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    H = 5,
}

impl Port {
    /// Turns a `Port` into a `PinSet` containing one pin, number `index`.
    pub const fn pin(self, index: usize) -> PinSet {
        PinSet {
            port: self,
            pin_mask: 1 << index,
        }
    }
}

/// The STM32L0 GPIO hardware lets us configure up to 16 pins on the same port
/// at a time, and we expose this in the IPC API. A `PinSet` describes the
/// target of a configuration operation.
///
/// A `PinSet` can technically be empty (`pin_mask` of zero) but that's rarely
/// useful.
#[derive(Copy, Clone, Debug)]
pub struct PinSet {
    /// Port we're talking about.
    pub port: Port,
    /// Mask with 1s in affected positions, 0s in others.
    pub pin_mask: u16,
}

impl PinSet {
    /// Derives a `PinSet` by setting mask bit `index`.
    pub const fn and_pin(self, index: usize) -> Self {
        Self {
            pin_mask: self.pin_mask | 1 << index,
            ..self
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Mode {
    Input = 0b00,
    Output = 0b01,
    Alternate = 0b10,
    Analog = 0b11,
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum OutputType {
    PushPull = 0,
    OpenDrain = 1,
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Speed {
    Low = 0b00,
    Medium = 0b01,
    High = 0b10,
    VeryHigh = 0b11,
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Pull {
    None = 0b00,
    Up = 0b01,
    Down = 0b10,
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Alternate {
    AF0 = 0,
    AF1 = 1,
    AF2 = 2,
    AF3 = 3,
    AF4 = 4,
    AF5 = 5,
    AF6 = 6,
    AF7 = 7,
    AF8 = 8,
    AF9 = 9,
    AF10 = 10,
    AF11 = 11,
    AF12 = 12,
    AF13 = 13,
    AF14 = 14,
    AF15 = 15,
}

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

impl From<GpioError> for u16 {
    fn from(rc: GpioError) -> Self {
        rc as u16
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

impl Sys {
    /// Configures a subset of pins in a GPIO port.
    ///
    /// This is the raw operation, which can be useful if you're doing something
    /// unusual, but see `configure_output`, `configure_input`, and
    /// `configure_alternate` for the common cases.
    pub fn gpio_configure(
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

        self.gpio_configure_raw(port, pins, packed_attributes)
    }

    /// Configures the pins in `PinSet` as high-impedance digital inputs, with
    /// optional pull resistors.
    pub fn gpio_configure_input(
        &self,
        pinset: PinSet,
        pull: Pull,
    ) -> Result<(), GpioError> {
        self.gpio_configure(
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
    pub fn gpio_configure_output(
        &self,
        pinset: PinSet,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
    ) -> Result<(), GpioError> {
        self.gpio_configure(
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
    pub fn gpio_configure_alternate(
        &self,
        pinset: PinSet,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        self.gpio_configure(
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
    pub fn gpio_configure_alternate_input(
        &self,
        pinset: PinSet,
        pull: Pull,
        af: Alternate,
    ) -> Result<(), GpioError> {
        self.gpio_configure_alternate(
            pinset,
            OutputType::OpenDrain,
            Speed::High,
            pull,
            af,
        )
    }

    /// Sets some pins high.
    pub fn gpio_set(&self, pinset: PinSet) -> Result<(), GpioError> {
        self.gpio_set_reset(pinset.port, pinset.pin_mask, 0)
    }

    /// Resets some pins low.
    pub fn gpio_reset(&self, pinset: PinSet) -> Result<(), GpioError> {
        self.gpio_set_reset(pinset.port, 0, pinset.pin_mask)
    }

    /// Sets some pins based on `flag` -- high if `true`, low if `false`.
    #[inline]
    pub fn gpio_set_to(
        &self,
        pinset: PinSet,
        flag: bool,
    ) -> Result<(), GpioError> {
        self.gpio_set_reset(
            pinset.port,
            if flag { pinset.pin_mask } else { 0 },
            if flag { 0 } else { pinset.pin_mask },
        )
    }

    pub fn gpio_read(&self, pinset: PinSet) -> Result<u16, GpioError> {
        Ok(self.gpio_read_input(pinset.port)? & pinset.pin_mask)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
