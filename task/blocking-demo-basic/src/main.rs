// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Most basic blocking counterpart of `task-async-demo`, for side-by-side
//! comparison: a single fixed-rate tick, with every slower activity counted
//! off it, in the style of tasks like `power` and `thermal`.
//!
//! It behaves identically: an IPC server that echoes every message back to
//! its sender, which meanwhile also
//!
//! - logs a tick to the ringbuf at three different periods,
//! - toggles user LED 0 via the `user_leds` task,
//! - logs if the echo server goes 5 seconds without a message, and
//! - logs each time the `poke` notification is posted.
//!
//! Where `task-async-demo` gives each activity its own async task, and
//! `task-blocking-demo` its own timer in a `Multitimer`, here the kernel timer
//! fires every `TICK_INTERVAL`, and each activity runs every so many ticks.
//! That only works because every period is a multiple of the tick.
//!
//! The `demo_client` task sends the echo requests and posts `poke`.
//!
//! On hardware, watch it with `humility ringbuf blocking_demo_basic`. Built
//! for the host (`cargo xtask host-run app/host/blocking-demo-basic.toml`),
//! every trace event is also printed to stderr, since there's no humility
//! there.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use drv_user_leds_api::UserLeds;
use ringbuf::counted_ringbuf;
use userlib::{
    NotificationBits, RecvMessage, TaskId, sys_get_timer, sys_recv_open,
    sys_reply, sys_set_timer, task_slot,
};

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
        eprintln!("[blocking_demo_basic] {event:?}");
        ringbuf::ringbuf_entry!(event);
    }};
}

/// Longest message the echo server accepts.
const ECHO_MAX: usize = 32;

/// Reply code for messages too long to echo.
const ECHO_TOO_LONG: u32 = 1;

/// The fundamental tick. Every period below is a whole number of these.
const TICK_INTERVAL: u64 = 250;

/// How often the LED is toggled: every tick.
const BLINK_TICKS: u32 = 1;
/// The three tickers' periods: 1s, 2.5s, and 7s.
const FAST_TICKS: u32 = 4;
const MEDIUM_TICKS: u32 = 10;
const SLOW_TICKS: u32 = 28;
/// How long the echo server can go without a message before we log it: 5s.
const ECHO_IDLE_TICKS: u32 = 20;

struct ServerImpl {
    leds: UserLeds,

    /// When the timer is next due. We step this by exactly `TICK_INTERVAL`
    /// rather than setting it relative to "now", so ticks don't drift.
    deadline: u64,
    /// How many times the timer has fired.
    fired: u32,

    tick_counts: [u32; 3],
    led_count: u32,
    poke_count: u32,
    echo_count: u32,
    echo_count_at_last_idle_check: u32,
}

impl ServerImpl {
    fn handle_notification(&mut self, bits: NotificationBits) {
        if bits.check_notification_mask(notifications::POKE_MASK) {
            self.poke_count = self.poke_count.wrapping_add(1);
            trace!(Trace::Poked {
                count: self.poke_count,
                now: now()
            });
        }

        if bits.has_timer_fired(notifications::TIMER_MASK) {
            self.handle_timer_fired();
            self.deadline += TICK_INTERVAL;
            sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);
        }
    }

    /// Runs every `TICK_INTERVAL`; each activity checks whether this is one
    /// of its ticks.
    ///
    /// When several are due on the same tick, they run in the order below.
    /// That's the order the async version happens to poll its tasks in, so
    /// the two produce identical traces.
    fn handle_timer_fired(&mut self) {
        self.fired = self.fired.wrapping_add(1);

        if self.fired.is_multiple_of(ECHO_IDLE_TICKS) {
            if self.echo_count == self.echo_count_at_last_idle_check {
                trace!(Trace::EchoIdle { now: now() });
            }
            self.echo_count_at_last_idle_check = self.echo_count;
        }

        if self.fired.is_multiple_of(BLINK_TICKS) {
            self.led_count = self.led_count.wrapping_add(1);
            trace!(Trace::LedToggle {
                count: self.led_count,
                now: now()
            });
            let _ = self.leds.led_toggle(0);
        }

        if self.fired.is_multiple_of(SLOW_TICKS) {
            self.tick(Name::Slow);
        }
        if self.fired.is_multiple_of(MEDIUM_TICKS) {
            self.tick(Name::Medium);
        }
        if self.fired.is_multiple_of(FAST_TICKS) {
            self.tick(Name::Fast);
        }
    }

    /// Logs a `Trace::Tick` for the ticker `name`.
    fn tick(&mut self, name: Name) {
        let count = &mut self.tick_counts[name as usize];
        *count = count.wrapping_add(1);
        trace!(Trace::Tick {
            name,
            count: *count,
            now: now()
        });
    }

    /// Echoes `msg`, whose body is `buf[..msg.message_len]`.
    fn handle_message(&mut self, msg: &RecvMessage, buf: &[u8]) {
        let len = msg.message_len;
        if len > ECHO_MAX || len > msg.response_capacity {
            trace!(Trace::EchoRejected {
                op: msg.operation,
                len,
                capacity: msg.response_capacity,
            });
            sys_reply(msg.sender, ECHO_TOO_LONG, &[]);
            return;
        }

        trace!(Trace::Echo {
            op: msg.operation,
            len,
            now: now()
        });
        sys_reply(msg.sender, 0, &buf[..len]);
        self.echo_count = self.echo_count.wrapping_add(1);
    }
}

fn now() -> u64 {
    sys_get_timer().now
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    trace!(Trace::Start);

    let deadline = now() + TICK_INTERVAL;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    let mut server = ServerImpl {
        leds: UserLeds::from(USER_LEDS.get_task_id()),
        deadline,
        fired: 0,
        tick_counts: [0; 3],
        led_count: 0,
        poke_count: 0,
        echo_count: 0,
        echo_count_at_last_idle_check: 0,
    };

    // One byte longer than the longest message we accept: the kernel reports
    // how much it copied, not how much was sent, so a message that fills the
    // buffer may have been truncated.
    let mut buf = [0u8; ECHO_MAX + 1];

    loop {
        let msg = sys_recv_open(
            &mut buf,
            notifications::TIMER_MASK | notifications::POKE_MASK,
        );
        if msg.sender == TaskId::KERNEL {
            server.handle_notification(NotificationBits::new(msg.operation));
        } else {
            server.handle_message(&msg, &buf);
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
