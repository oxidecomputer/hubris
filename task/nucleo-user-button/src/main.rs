// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A demo task showing the use of EXTI (GPIO interrupts) on the
//! STM32H7-NUCLEO-H753ZI2 board.
//!
//! This task listens for interrupts on the user button (PC13) and toggles a LED
//! when the button is pressed, using the `user-leds` IPC interface.

#![no_std]
#![no_main]

use drv_stm32xx_sys_api::{Edge, IrqControl, Pull};
use ringbuf::ringbuf_entry;
use userlib::*;

#[cfg(not(any(
    target_board = "nucleo-h753zi",
    target_board = "nucleo-h743zi2",
)))]
compile_error!(
    "the `nucleo-user-button` task is only supported on the Nucleo H753ZI and H743ZI2 boards"
);

task_slot!(USER_LEDS, user_leds);
task_slot!(SYS, sys);

task_config::optional_task_config! {
    /// The index of the user LED to toggle
    led: usize,
    /// Edge sensitivity for the button interrupt
    edge: Edge,
}

// In real life, we might not actually want to trace all of these events, but
// for demo purposes, it's nice to be able to watch everything that happens in
// Humility.
#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    /// We called the `Sys.gpio_irq_configure` IPC with these arguments.
    GpioIrqConfigure { mask: u32, edge: Edge },

    /// We called the `Sys.gpio_irq_control` IPC with these arguments, and it
    /// returned whether the interrupt had fired or not.
    GpioIrqControl {
        mask: u32,
        op: IrqControl,
        #[count(children)]
        fired: bool,
    },

    /// We received a notification with these bits set.
    Notification(u32),

    /// We called the `UserLeds.led_toggle` IPC.
    LedToggle { led: usize },
}

ringbuf::counted_ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
pub fn main() -> ! {
    let user_leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
    let sys = drv_stm32xx_sys_api::Sys::from(SYS.get_task_id());

    let Config { led, edge } = TASK_CONFIG.unwrap_or(Config {
        led: 0,
        edge: Edge::Rising,
    });

    sys.gpio_configure_input(gpio_irq_pins::BUTTON, Pull::None);

    ringbuf_entry!(Trace::GpioIrqConfigure {
        mask: notifications::BUTTON_MASK,
        edge,
    });
    sys.gpio_irq_configure(notifications::BUTTON_MASK, edge);

    loop {
        // Call `Sys.gpio_irq_control` to enable our interrupt, returning
        // whether it has fired.
        let fired = match sys
            .gpio_irq_control(notifications::BUTTON_MASK, IrqControl::Enable)
        {
            Ok(fired) => fired,
            // If the sys task panicked, okay, let's just try to enable the IRQ
            // again.
            Err(_) => continue,
        };
        ringbuf_entry!(Trace::GpioIrqControl {
            mask: notifications::BUTTON_MASK,
            op: IrqControl::Enable,
            fired
        });

        // If the button has changed state, toggle the LED.
        if fired {
            ringbuf_entry!(Trace::LedToggle { led });
            user_leds.led_toggle(led).unwrap_lite();
        }

        // Wait for the user button to be pressed.
        let notif = sys_recv_notification(notifications::BUTTON_MASK);
        ringbuf_entry!(Trace::Notification(notif.get_raw_bits()));
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/gpio_irq_pins.rs"));
