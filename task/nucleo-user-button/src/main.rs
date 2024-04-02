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

use drv_stm32xx_sys_api::{Edge, PinSet, Port, Pull};
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
    /// Whether to enable rising-edge interrupts
    rising: bool,
    /// Whether to enable falling-edge interrupts
    falling: bool,
}

// In real life, we might not actually want to trace all of these events, but
// for demo purposes, it's nice to be able to watch everything that happens in
// Humility.
#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    /// Config should enable either the rising or falling edge. We defaulted to
    /// "rising" because you didn't enable either edge sensitivity.
    ConfigDidntEnableAnyEdgeSensitivity,

    /// We called the `Sys.gpio_irq_configure` IPC with these arguments.
    GpioIrqConfigure { mask: u32, sensitivity: Edge },

    /// We called the `Sys.gpio_irq_control` IPC with these arguments.
    GpioIrqControl { enable_mask: u32, disable_mask: u32 },

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

    let (led, sensitivity) = if let Some(Config {
        rising,
        falling,
        led,
    }) = TASK_CONFIG
    {
        let sensitivity = match (rising, falling) {
            (true, true) => Edge::Both,
            (false, true) => Edge::Rising,
            (true, false) => Edge::Falling,
            (false, false) => {
                ringbuf_entry!(Trace::ConfigDidntEnableAnyEdgeSensitivity);
                // Just picking something arbitrarily seems nicer than panicking
                // endlessly in a loop...
                Edge::Rising
            }
        };
        (led, sensitivity)
    } else {
        (0, Edge::Rising)
    };

    sys.gpio_configure_input(
        PinSet {
            port: Port::C,
            pin_mask: (1 << 13),
        },
        Pull::None,
    );

    ringbuf_entry!(Trace::GpioIrqConfigure {
        mask: notifications::BUTTON_MASK,
        sensitivity,
    });
    sys.gpio_irq_configure(notifications::BUTTON_MASK, sensitivity);

    loop {
        // The first argument to `gpio_irq_control` is the mask of interrupts to
        // disable, while the second is the mask to enable. So, enable the
        // button notification.
        let disable_mask = 0;
        ringbuf_entry!(Trace::GpioIrqControl {
            enable_mask: notifications::BUTTON_MASK,
            disable_mask,
        });
        sys.gpio_irq_control(disable_mask, notifications::BUTTON_MASK);

        // Wait for the user button to be pressed.
        //
        // We only care about notifications, so we can pass a zero-sized recv
        // buffer, and the kernel's task ID.
        let recvmsg = sys_recv_closed(
            &mut [],
            notifications::BUTTON_MASK,
            TaskId::KERNEL,
        )
        // Recv from the kernel never returns an error.
        .unwrap_lite();

        let notif = recvmsg.operation;
        ringbuf_entry!(Trace::Notification(notif));

        // If the notification is for the button, toggle the LED.
        if notif == notifications::BUTTON_MASK {
            ringbuf_entry!(Trace::LedToggle { led });
            user_leds.led_toggle(led).unwrap_lite();
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
