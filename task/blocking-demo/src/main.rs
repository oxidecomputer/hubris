// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Blocking counterpart of `task-async-demo`, written the traditional Hubris
//! way, for side-by-side comparison.
//!
//! It behaves identically: an IPC server that echoes every message back to
//! its sender, which meanwhile also
//!
//! - logs a tick to the ringbuf at three different periods,
//! - toggles user LED 0 via the `user_leds` task,
//! - logs if the echo server goes 5 seconds without a message, and
//! - logs each time the `poke` notification is posted.
//!
//! Where the async version gives each activity its own async task, here they
//! are all events handled by one receive loop: the periodic activities share
//! the task's single kernel timer through a `Multitimer`, and the state each
//! one keeps lives in `main`'s locals rather than in its own task.
//!
//! The `demo_client` task sends the echo requests and posts `poke`.
//!
//! On hardware, watch it with `humility ringbuf blocking_demo`. Built for the
//! host (`cargo xtask host-run app/host/blocking-demo.toml`), every trace
//! event is also printed to stderr, since there's no humility there.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use enum_map::Enum;
use multitimer::{Multitimer, Repeat};
use ringbuf::counted_ringbuf;
use userlib::{TaskId, sys_get_timer, sys_recv_open, sys_reply, task_slot};

task_slot!(USER_LEDS, user_leds);

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
#[cfg_attr(not(target_os = "none"), derive(Debug))]
enum Trace {
    #[count(skip)]
    None,
    Start,
    Tick {
        #[count(children)]
        name: Name,
        count: u32,
        now: u64,
    },
    LedToggle {
        count: u32,
        now: u64,
    },
    Echo {
        op: u32,
        len: usize,
        now: u64,
    },
    EchoRejected {
        op: u32,
        len: usize,
        capacity: usize,
    },
    EchoIdle {
        now: u64,
    },
    Poked {
        count: u32,
        now: u64,
    },
}

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
#[cfg_attr(not(target_os = "none"), derive(Debug))]
enum Name {
    Fast,
    Medium,
    Slow,
}

counted_ringbuf!(Trace, 32, Trace::None);

/// Records a trace event in the ringbuf. On the host, where there's no
/// humility to read the ringbuf, also prints it to stderr. (Stdout carries a
/// host task's syscalls to the kernel, so it's not available for logging.)
macro_rules! trace {
    ($event:expr) => {{
        let event = $event;
        #[cfg(not(target_os = "none"))]
        eprintln!("[blocking_demo] {event:?}");
        ringbuf::ringbuf_entry!(event);
    }};
}

/// Longest message the echo server accepts.
const ECHO_MAX: usize = 32;

/// How long the echo server can go without a message before the idle check
/// logs it.
const ECHO_IDLE_TIMEOUT: u64 = 5_000;

/// Reply code for messages too long to echo.
const ECHO_TOO_LONG: u32 = 1;

/// How often the LED is toggled.
const BLINK_PERIOD: u64 = 250;

/// The periodic activities, each with its own timer in the `Multitimer`.
///
/// When several fire at the same tick, they're handled in declaration order.
/// That's the order the async version happens to poll its tasks in, so the
/// two produce identical traces.
#[derive(Copy, Clone, Enum)]
enum Timers {
    EchoIdleCheck,
    Blink,
    TickSlow,
    TickMedium,
    TickFast,
}

fn now() -> u64 {
    sys_get_timer().now
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    trace!(Trace::Start);

    let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());

    // Every periodic activity's schedule, multiplexed onto the one kernel
    // timer. `AfterDeadline` repeats at a fixed rate, like the async
    // version's `Ticker`; `AfterWake` re-arms relative to when it fired, like
    // its `Timer::after` in a loop.
    let mut timers = Multitimer::<Timers>::new(notifications::TIMER_BIT);
    let start = now();
    for (timer, period, repeat) in [
        (Timers::TickFast, 1_000, Repeat::AfterDeadline(1_000)),
        (Timers::TickMedium, 2_500, Repeat::AfterDeadline(2_500)),
        (Timers::TickSlow, 7_000, Repeat::AfterDeadline(7_000)),
        (
            Timers::Blink,
            BLINK_PERIOD,
            Repeat::AfterDeadline(BLINK_PERIOD),
        ),
        (
            Timers::EchoIdleCheck,
            ECHO_IDLE_TIMEOUT,
            Repeat::AfterWake(ECHO_IDLE_TIMEOUT),
        ),
    ] {
        timers.set_timer(timer, start + period, Some(repeat));
    }

    // The state each async task keeps in its own locals all lives here.
    let mut tick_counts = [0u32; 3];
    let mut led_count = 0u32;
    let mut poke_count = 0u32;
    let mut echo_count = 0u32;
    let mut echo_count_at_last_idle_check = 0u32;

    // One byte longer than the longest message we accept: the kernel reports
    // how much it copied, not how much was sent, so a message that fills the
    // buffer may have been truncated.
    let mut buf = [0u8; ECHO_MAX + 1];

    // The one receive loop, which gets every event: messages, timer
    // notifications, and pokes.
    loop {
        let msg = sys_recv_open(
            &mut buf,
            notifications::TIMER_MASK | notifications::POKE_MASK,
        );

        if msg.sender == TaskId::KERNEL {
            let bits = msg.operation;

            if bits & notifications::POKE_MASK != 0 {
                poke_count = poke_count.wrapping_add(1);
                trace!(Trace::Poked {
                    count: poke_count,
                    now: now()
                });
            }

            timers.handle_notification(bits);
            for timer in timers.iter_fired() {
                match timer {
                    Timers::EchoIdleCheck => {
                        if echo_count == echo_count_at_last_idle_check {
                            trace!(Trace::EchoIdle { now: now() });
                        }
                        echo_count_at_last_idle_check = echo_count;
                    }
                    Timers::Blink => {
                        led_count = led_count.wrapping_add(1);
                        trace!(Trace::LedToggle {
                            count: led_count,
                            now: now()
                        });
                        let _ = leds.led_toggle(0);
                    }
                    Timers::TickSlow => tick(&mut tick_counts, Name::Slow),
                    Timers::TickMedium => tick(&mut tick_counts, Name::Medium),
                    Timers::TickFast => tick(&mut tick_counts, Name::Fast),
                }
            }
            continue;
        }

        let len = msg.message_len;
        if len > ECHO_MAX || len > msg.response_capacity {
            trace!(Trace::EchoRejected {
                op: msg.operation,
                len,
                capacity: msg.response_capacity,
            });
            sys_reply(msg.sender, ECHO_TOO_LONG, &[]);
            continue;
        }

        trace!(Trace::Echo {
            op: msg.operation,
            len,
            now: now()
        });
        sys_reply(msg.sender, 0, &buf[..len]);
        echo_count = echo_count.wrapping_add(1);
    }
}

/// Logs a `Trace::Tick` for the ticker `name`.
fn tick(counts: &mut [u32; 3], name: Name) {
    let count = &mut counts[name as usize];
    *count = count.wrapping_add(1);
    trace!(Trace::Tick {
        name,
        count: *count,
        now: now()
    });
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
