// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! GPIO-related things needed by all STM32 parts.

#![no_std]

use userlib::FromPrimitive;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

#[cfg(feature = "server-support")]
pub mod server;

/// Enumerates the GPIO ports available on this chip, from the perspective of
/// driver software. This does not mean the GPIO port is physically available on
/// pins of the package -- we don't model package differences.
#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum Port {
    A = 0,
    B,
    C,
    D,
    #[cfg(feature = "has-port-gpioe")]
    E,
    #[cfg(feature = "has-port-gpiof")]
    F,
    #[cfg(feature = "has-port-gpiog")]
    G,
    #[cfg(feature = "has-port-gpioh")]
    H,
    #[cfg(feature = "has-port-gpioi")]
    I,
    #[cfg(feature = "has-port-gpioj")]
    J,
    #[cfg(feature = "has-port-gpiok")]
    K,
}

impl Port {
    /// Turns a `Port` into a `PinSet` containing one pin, number `index`.
    #[inline(always)]
    pub const fn pin(self, index: usize) -> PinSet {
        PinSet {
            port: self,
            pin_mask: 1 << index,
        }
    }

    /// Convenience operation for creating a `PinSet` from a `Port` with _many_
    /// pins included.
    #[inline(always)]
    pub const fn pins<const N: usize>(self, indexes: [usize; N]) -> PinSet {
        let mut pin_mask = 0;
        // Using a manual for loop because const fn limitations
        let mut i = 0;
        while i < N {
            pin_mask |= 1 << indexes[i];
            i += 1;
        }
        PinSet {
            port: self,
            pin_mask,
        }
    }
}

/// The STM32xx GPIO hardware lets us configure up to 16 pins on the same port
/// at a time, and we expose this in the API. A `PinSet` describes the target of
/// a configuration operation.
///
/// A `PinSet` can technically be empty (`pin_mask` of zero) but that's rarely
/// useful.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PinSet {
    /// Port we're talking about.
    pub port: Port,
    /// Mask with 1s in affected positions, 0s in others.
    pub pin_mask: u16,
}

impl PinSet {
    /// Derives a `PinSet` by setting mask bit `index`.
    #[inline(always)]
    pub const fn and_pin(self, index: usize) -> Self {
        Self {
            pin_mask: self.pin_mask | 1 << index,
            ..self
        }
    }
}

/// Possible modes for a GPIO pin.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Mode {
    /// Digital input. This activates a Schmitt trigger on the pin, which is
    /// great for receiving digital signals, but can burn a lot of current if
    /// faced with signals intermediate between 1 and 0. As a result, to treat a
    /// pin as unused, set it to `Analog`.
    Input = 0b00,
    /// Software-controlled output. Values written to the corresponding bit of
    /// the ODR register will control the pin's driver.
    Output = 0b01,
    /// Alternate function. This disconnects the direct GPIO driver from the pin
    /// and instead connects it to the function mux, which in turn connects it
    /// to a peripheral signal chosen by one of the `AFx` values written to
    /// AFRL/AFRH.
    Alternate = 0b10,
    /// Analog input. This disconnects the output driver, input Schmitt trigger,
    /// and function mux from the pin, and is the highest-impedance state. It is
    /// _also_ useful for analog if the pin has an ADC channel attached.
    Analog = 0b11,
}

/// Drive modes for a GPIO pin.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum OutputType {
    /// The pin will be driven both high and low in `Output` and `Alternate`
    /// modes.
    PushPull = 0,
    /// Turns off the pin's high side driver in `Output` and `Alternate` modes,
    /// so that setting the pin to 0 pulls low, but 1 enters a high impedance
    /// state.
    OpenDrain = 1,
}

/// Drive speeds / slew rate limits for GPIO pins.
///
/// When in doubt, use `Low`. It's fast enough for most things and is less prone
/// to generating reflections and EMI. Note that you need to check the datasheet
/// for the specific part you're targeting to get the actual speeds of these
/// drive settings. The notes below are thus vague.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Speed {
    /// Slowest and generally correct drive speed (up to, say, 10MHz or so).
    Low = 0b00,
    /// Somewhat faster (say, 50MHz).
    Medium = 0b01,
    /// Somewhat faster-er (idk like 80MHz? Go read the datasheet)
    High = 0b10,
    /// Go read the datasheet.
    VeryHigh = 0b11,
}

/// Settings for the switchable weak pull resistors on GPIO pins.
///
/// Note that the pull resistors apply in all modes, so, you can apply these to
/// an input, and you will want to turn them off for `Analog`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Pull {
    /// Both resistors off.
    None = 0b00,
    /// Weak pull up.
    Up = 0b01,
    /// Weak pull down.
    Down = 0b10,
}

/// Enumeration of alternate functions that can be stuffed into the AFRL/AFRH
/// registers to change pin muxes. These only apply when the pin is in
/// `Alternate` mode.
///
/// These are numbers and not, like, convenient human-readable peripheral names
/// because the mapping from pin + AF to signal is very complex. See the
/// datasheet.
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Alternate {
    AF0 = 0,
    AF1 = 1,
    AF2 = 2,
    AF3 = 3,
    AF4 = 4,
    AF5 = 5,
    AF6 = 6,
    AF7 = 7,
    #[cfg(feature = "has-af8-thru-af15")]
    AF8 = 8,
    #[cfg(feature = "has-af8-thru-af15")]
    AF9 = 9,
    #[cfg(feature = "has-af8-thru-af15")]
    AF10 = 10,
    #[cfg(feature = "has-af8-thru-af15")]
    AF11 = 11,
    #[cfg(feature = "has-af8-thru-af15")]
    AF12 = 12,
    #[cfg(feature = "has-af8-thru-af15")]
    AF13 = 13,
    #[cfg(feature = "has-af8-thru-af15")]
    AF14 = 14,
    #[cfg(feature = "has-af8-thru-af15")]
    AF15 = 15,
}
