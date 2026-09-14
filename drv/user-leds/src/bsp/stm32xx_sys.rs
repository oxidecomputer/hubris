// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32 devices that have a `sys` task that manages GPIOs. This includes
//! the STM32G0 and STM32H7 families.

use crate::bsp::Bsp;
use drv_stm32xx_sys_api::{OutputType, PinSet, Pull, Speed, Sys};
use userlib::{UnwrapLite, task_slot};

task_slot!(SYS, sys);

#[derive(Clone, Copy)]
pub struct PinInfo {
    pub info: PinSet,
    pub active_low: bool,
}

pub struct BspImpl {
    sys: Sys,
}

impl Bsp for BspImpl {
    type Led = board::Led;

    fn new() -> Self {
        Self {
            sys: Sys::from(SYS.get_task_id()),
        }
    }

    fn enable_led_pins(&self) {
        for PinInfo { info, active_low } in board::LEDS {
            // Make sure LEDs are initially off.
            self.sys.gpio_set_to(*info, *active_low);
            // Make them outputs.
            self.sys.gpio_configure_output(
                *info,
                OutputType::PushPull,
                Speed::High,
                Pull::None,
            );
        }
    }

    fn led_on(&self, led: Self::Led) {
        let PinInfo { info, active_low } = board::led_info(led);
        self.sys.gpio_set_to(info, !active_low);
    }

    fn led_off(&self, led: Self::Led) {
        let PinInfo { info, active_low } = board::led_info(led);

        self.sys.gpio_set_to(info, active_low);
    }

    fn led_toggle(&self, led: Self::Led) {
        let PinInfo {
            info,
            active_low: _,
        } = board::led_info(led);
        self.sys.gpio_toggle(info.port, info.pin_mask).unwrap_lite();
    }
}

mod board {
    use super::PinInfo;
    use drv_stm32xx_sys_api::{PinSet, Port};

    #[allow(dead_code)]
    const fn act_low(pinset: PinSet) -> PinInfo {
        PinInfo {
            info: pinset,
            active_low: true,
        }
    }

    #[allow(dead_code)]
    const fn act_hi(pinset: PinSet) -> PinInfo {
        PinInfo {
            info: pinset,
            active_low: false,
        }
    }

    pub(super) fn led_info(led: Led) -> PinInfo {
        const _: () = assert!(
            LEDS.len() == <Led as enum_map::Enum>::LENGTH,
            "LED count mismatch length vs enum"
        );
        LEDS[led as usize]
    }

    // Target boards with 4 leds
    #[cfg(any(
        target_board = "gemini-bu-1",
        target_board = "gimletlet-1",
        target_board = "gimletlet-2"
    ))]
    pub type Led = crate::bsp::Led4;

    // Target boards with 3 leds
    #[cfg(any(
        target_board = "nucleo-h753zi",
        target_board = "nucleo-h743zi2"
    ))]
    pub type Led = crate::bsp::Led3;

    // Target boards with 1 led
    #[cfg(any(
        target_board = "stm32g031-nucleo",
        target_board = "stm32g070-nucleo",
        target_board = "stm32g0b1-nucleo",
        target_board = "donglet-g030",
        target_board = "donglet-g031",
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
        target_board = "psc-b",
        target_board = "psc-c",
        target_board = "observer-a",
        target_board = "oxcon2023g0",
        target_board = "grapefruit-a",
        target_board = "grapefruit-b",
        target_board = "cosmo-a",
        target_board = "cosmo-b",
        target_board = "metro-a",
    ))]
    pub type Led = crate::bsp::Led1;

    // G0 Zone

    #[cfg(target_board = "stm32g031-nucleo")]
    pub const LEDS: &[PinInfo] = &[act_low(Port::C.pin(6))];

    #[cfg(any(target_board = "donglet-g030", target_board = "donglet-g031"))]
    pub const LEDS: &[PinInfo] = &[act_low(Port::A.pin(12))];

    #[cfg(target_board = "oxcon2023g0")]
    pub const LEDS: &[PinInfo] = &[act_low(Port::B.pin(7))];

    #[cfg(target_board = "stm32g070-nucleo")]
    pub const LEDS: &[PinInfo] = &[act_low(Port::A.pin(5))];

    // H7 Zone

    #[cfg(any(
        target_board = "nucleo-h743zi2",
        target_board = "nucleo-h753zi"
    ))]
    /// Nucleo boards: LEDs are on PB0, PB14 and PE1.
    pub const LEDS: &[PinInfo] = &[
        act_hi(Port::B.pin(0)),
        act_hi(Port::B.pin(14)),
        act_hi(Port::E.pin(1)),
    ];

    #[cfg(any(target_board = "gemini-bu-1", target_board = "gimletlet-1"))]
    /// Gemini bringup SP: LEDs are on PI8, PI9, PI10 and PI11.
    /// Original Gimletlet: LEDs are on PI8-11
    pub const LEDS: &[PinInfo] = &[
        act_hi(Port::I.pin(8)),
        act_hi(Port::I.pin(9)),
        act_hi(Port::I.pin(10)),
        act_hi(Port::I.pin(11)),
    ];

    #[cfg(target_board = "gimletlet-2")]
    /// Glorified gimletlet SP: LEDs are on PG2-5
    pub const LEDS: &[PinInfo] = &[
        act_hi(Port::G.pin(2)),
        act_hi(Port::G.pin(3)),
        act_hi(Port::G.pin(4)),
        act_hi(Port::G.pin(5)),
    ];

    #[cfg(any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
        target_board = "psc-b",
        target_board = "psc-c",
        target_board = "observer-a",
    ))]
    pub const LEDS: &[PinInfo] = &[act_hi(Port::A.pin(3))];

    #[cfg(any(target_board = "grapefruit-a", target_board = "grapefruit-b"))]
    pub const LEDS: &[PinInfo] = &[act_hi(Port::C.pin(6))];

    #[cfg(any(
        target_board = "cosmo-a",
        target_board = "cosmo-b",
        target_board = "metro-a",
    ))]
    pub const LEDS: &[PinInfo] = &[act_hi(Port::H.pin(6))];
}
