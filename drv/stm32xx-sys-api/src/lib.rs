// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the STM32xx SYS server.

#![no_std]

cfg_if::cfg_if! {
    if #[cfg(feature = "family-stm32g0")] {
        mod g0;
        pub use self::g0::*;
    } else if #[cfg(feature = "family-stm32h7")] {
        mod h7;
        pub use self::h7::*;
    } else {
        compile_error!("unsupported SoC family");
    }
}

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

pub use drv_stm32xx_gpio_common::{
    Alternate, Mode, OutputType, PinSet, Port, Pull, Speed,
};

#[derive(Copy, Clone, Debug, FromPrimitive, IdolError)]
#[repr(u32)]
#[derive(counters::Count)]
pub enum RccError {
    NoSuchPeripheral = 1,
}

/// Configures edge sensitivity for a GPIO interrupt
#[derive(
    Copy,
    Clone,
    FromPrimitive,
    PartialEq,
    Eq,
    IntoBytes,
    Immutable,
    KnownLayout,
    serde::Deserialize,
)]
// NOTE: This `repr` attribute is *not* necessary for
// serialization/deserialization, but it is used to allow casting to `u8` in the
// `Edge::{is_rising, is_falling}` methods. The current implementation of those
// methods with bit-and tests generates substantially fewer instructions than
// using `matches!` (see: https://godbolt.org/z/j5fdPfz3c).
#[repr(u8)]
pub enum Edge {
    /// The interrupt will trigger on the rising edge only.
    Rising = 0b01,
    /// The interrupt will trigger on the falling edge only.
    Falling = 0b10,
    /// The interrupt will trigger on both teh rising and falling edge.
    Both = 0b11,
}

/// Describes which operation is performed by the [`Sys::gpio_irq_control`] IPC.
#[derive(
    Copy,
    Clone,
    FromPrimitive,
    PartialEq,
    Eq,
    IntoBytes,
    Immutable,
    KnownLayout,
    serde::Deserialize,
)]
// repr attribute is required for the derived `IntoBytes` implementation
#[repr(u8)]
pub enum IrqControl {
    /// Disable any interrupts mapped to the provided notification mask.
    Disable = 0,
    /// Enable any interrupts mapped to the provided notification mask.
    Enable,
    /// Check if any interrupts mapped to the provided notification mask have
    /// been triggered, *without* enabling or disabling the interrupt.
    ///
    /// If an interrupt is currently enabled, it will remain enabled, while if
    /// it is currently disabled, it will remain disabled.
    Check,
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

/// Assign peripheral numbers that are unique by group.
const fn periph(g: Group, bit_number: u8) -> u32 {
    // Note: this will accept bit numbers higher than 31, and they'll wrap
    // around to zero. Asserting here would be nice, but asserts in const fns
    // are not yet stable. In practice, you are likely to get a compile error if
    // you make a mistake here, because it will cause enum variants to alias to
    // the same number which is not permitted.
    (g as u32) << 5 | (bit_number & 0x1F) as u32
}

impl Peripheral {
    #[inline(always)]
    pub fn group(self) -> Group {
        let index = (self as u32 >> 5) as u8;
        // Safety: this is unsafe because it can turn any arbitrary bit pattern
        // into a `Group`, potentially resulting in undefined behavior. However,
        // `self` is a valid `Peripheral`, and we make sure (above) that
        // `Peripheral` has valid values in its `Group` bits by only
        // constructing it _from_ a `Group`. So this is safe.
        //
        // The reason this is using unsafe code in the _first_ place is to
        // ensure that we don't generate an unnecessary panic here. We don't
        // need the panic because we already checked user input on the way into
        // the `Peripheral` type.
        unsafe { core::mem::transmute(index) }
    }

    #[inline(always)]
    pub fn bit_index(self) -> u8 {
        self as u8 & 0x1F
    }
}

impl Sys {
    /// Configures a subset of pins in a GPIO port.
    ///
    /// This is the raw operation, which can be useful if you're doing something
    /// unusual, but see `gpio_configure_output`, `gpio_configure_input`, and
    /// `gpio_configure_alternate` for the common cases.
    pub fn gpio_configure(
        &self,
        port: Port,
        pins: u16,
        mode: Mode,
        output_type: OutputType,
        speed: Speed,
        pull: Pull,
        af: Alternate,
    ) {
        let packed_attributes = mode as u16
            | (output_type as u16) << 2
            | (speed as u16) << 3
            | (pull as u16) << 5
            | (af as u16) << 7;

        self.gpio_configure_raw(port, pins, packed_attributes);
    }

    /// Configures the pins in `PinSet` as high-impedance digital inputs, with
    /// optional pull resistors.
    ///
    /// This chooses arbitrary settings for all the other fields (output type,
    /// slew rate, alternate function), and so it is not suitable for
    /// pins that need to be repeatedly reconfigured between high-impedance and
    /// alternate without intermediate glitching. In such cases you probably
    /// want to use the raw `gpio_configure`.
    pub fn gpio_configure_input(&self, pinset: PinSet, pull: Pull) {
        self.gpio_configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Input,
            OutputType::PushPull, // doesn't matter
            Speed::High,          // doesn't matter
            pull,
            Alternate::AF0, // doesn't matter
        );
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
    ) {
        self.gpio_configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Output,
            output_type,
            speed,
            pull,
            Alternate::AF0, // doesn't matter
        );
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
    ) {
        self.gpio_configure(
            pinset.port,
            pinset.pin_mask,
            Mode::Alternate,
            output_type,
            speed,
            pull,
            af,
        );
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
    ) {
        self.gpio_configure_alternate(
            pinset,
            OutputType::OpenDrain,
            Speed::High,
            pull,
            af,
        );
    }

    /// Sets some pins high.
    pub fn gpio_set(&self, pinset: PinSet) {
        self.gpio_set_reset(pinset.port, pinset.pin_mask, 0);
    }

    /// Resets some pins low.
    pub fn gpio_reset(&self, pinset: PinSet) {
        self.gpio_set_reset(pinset.port, 0, pinset.pin_mask);
    }

    /// Sets some pins based on `flag` -- high if `true`, low if `false`.
    #[inline]
    pub fn gpio_set_to(&self, pinset: PinSet, flag: bool) {
        self.gpio_set_reset(
            pinset.port,
            if flag { pinset.pin_mask } else { 0 },
            if flag { 0 } else { pinset.pin_mask },
        );
    }

    pub fn gpio_read(&self, pinset: PinSet) -> u16 {
        self.gpio_read_input(pinset.port) & pinset.pin_mask
    }

    /// Combines a common sequence of operations to initialize a reset line
    /// tied to a microcontroller GPIO pin:
    /// - Set the given GPIO pin(s) as low, to avoid glitches when setting it
    ///   as an output.
    /// - Configures the given GPIO pin as a push-pull output.  At this point,
    ///   the pin is actively driven low.
    /// - Holds it low for `low_time_ms`
    /// - Brings it high, then waits for `wait_time_ms`
    pub fn gpio_init_reset_pulse(
        &self,
        pinset: PinSet,
        low_time_ms: u32,
        wait_time_ms: u32,
    ) {
        self.gpio_reset(pinset);
        self.gpio_configure_output(
            pinset,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        );
        userlib::hl::sleep_for(low_time_ms as u64);
        self.gpio_set(pinset);
        userlib::hl::sleep_for(wait_time_ms as u64);
    }
}

impl Edge {
    /// Returns `true` if this edge sensitivity should trigger on the rising
    /// edge.
    pub fn is_rising(&self) -> bool {
        *self as u8 & Self::Rising as u8 != 0
    }

    /// Returns `true` if this edge sensitivity should trigger on the falling
    /// edge.
    pub fn is_falling(&self) -> bool {
        *self as u8 & Self::Falling as u8 != 0
    }
}

impl core::ops::BitOr for Edge {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Edge::Rising, Edge::Rising) => Edge::Rising,
            (Edge::Falling, Edge::Falling) => Edge::Falling,
            _ => Edge::Both,
        }
    }
}

impl From<bool> for IrqControl {
    fn from(value: bool) -> Self {
        if value {
            IrqControl::Enable
        } else {
            IrqControl::Disable
        }
    }
}

impl From<Option<bool>> for IrqControl {
    fn from(value: Option<bool>) -> Self {
        value.map(Self::from).unwrap_or(Self::Check)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
