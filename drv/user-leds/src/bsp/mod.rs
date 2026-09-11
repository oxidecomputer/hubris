// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use enum_map::EnumArray;
use userlib::FromPrimitive;

#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
mod stm32fx;
#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
pub use stm32fx::BspImpl;

#[cfg(any(feature = "stm32g0", feature = "stm32h7"))]
mod stm32xx_sys;
#[cfg(any(feature = "stm32g0", feature = "stm32h7"))]
pub use stm32xx_sys::BspImpl;

pub trait Bsp {
    type Led: Copy + FromPrimitive + EnumArray<bool>;
    fn enable_led_pins();
    fn led_on(led: Self::Led);
    fn led_off(led: Self::Led);
    fn led_toggle(led: Self::Led);
}

#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
#[allow(clippy::enum_variant_names)]
pub enum Led4Color {
    // chassis LED is controlled by cosmo-seq
    DebugWhite = 0,
    DebugRed = 1,
    DebugGreen = 2,
    DebugBlue = 3,
}

// Target boards with 4 leds
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led4 {
    Zero = 0,
    One = 1,
    Two = 2,
    Three = 3,
}

// Target boards with 3 leds
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led3 {
    Zero = 0,
    One = 1,
    Two = 2,
}

// Target boards with 1 led
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led1 {
    Zero = 0,
}

// Target boards with 2 leds -> the rest
#[derive(enum_map::Enum, Copy, Clone, FromPrimitive)]
pub enum Led2 {
    Zero = 0,
    One = 1,
}
