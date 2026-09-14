// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use enum_map::EnumArray;
use userlib::FromPrimitive;

// Select an impl and re-export based on coarse target features
cfg_if::cfg_if! {
    if #[cfg(any(feature = "stm32f3", feature = "stm32f4"))] {
        mod stm32fx;
        pub use stm32fx::BspImpl;
    } else if #[cfg(any(feature = "stm32g0", feature = "stm32h7"))] {
        mod stm32xx_sys;
        pub use stm32xx_sys::BspImpl;
    } else if #[cfg(feature = "lpc55")] {
        mod lpc55;
        pub use lpc55::BspImpl;
    } else {
        compile_error!("Unknown board for user-leds!");
    }
}

/// Board-specific user LED interface trait
pub trait Bsp {
    /// Type used to refer to a specific LED. Usually an enum.
    type Led: Copy + FromPrimitive + EnumArray<bool>;

    /// Create a new instance of the bsp.
    fn new() -> Self;

    /// Configure all LED pins as outputs and set to initial state
    /// of "off".
    fn enable_led_pins(&self);

    /// Set the LED "on"/illuminated.
    fn led_on(&self, led: Self::Led);

    /// Set the LED "off"/dark.
    fn led_off(&self, led: Self::Led);

    /// Toggle the current state (on->off or off->on).
    fn led_toggle(&self, led: Self::Led);
}

// The following are common LED types that are used for various boards.
//
// The level of variance here is somewhat unfortunate, but seems to
// work in practice.

#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
#[allow(clippy::enum_variant_names)]
pub enum Led4Color {
    // chassis LED is controlled by cosmo-seq
    DebugWhite = 0,
    DebugRed = 1,
    DebugGreen = 2,
    DebugBlue = 3,
}

/// Target boards with 4 leds
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led4 {
    Zero = 0,
    One = 1,
    Two = 2,
    Three = 3,
}

/// Target boards with 3 leds
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led3 {
    Zero = 0,
    One = 1,
    Two = 2,
}

/// Target boards with 1 led
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led1 {
    Zero = 0,
}

/// Target boards with 2 leds -> the rest
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led2 {
    Zero = 0,
    One = 1,
}
