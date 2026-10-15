// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demo of the `hubris-async` executor.
//!
//! The task's main loop is an ordinary synchronous IPC server that echoes
//! every message back to its sender. Alongside it, the executor runs six
//! async tasks:
//!
//! - three "tickers" that each log to the ringbuf at a different period,
//! - a blinker that toggles user LED 0 via the `user_leds` task,
//! - an idle watcher that logs if the echo server goes 5 seconds without a
//!   message, and
//! - a watcher that logs each time the `poke` notification is posted.
//!
//! The `async_demo_client` task sends the echo requests and posts `poke`.
//!
//! On hardware, watch it with `humility ringbuf async_demo`. Built for the
//! host (`cargo xtask host-run app/host/async-demo.toml`), every trace event
//! is also printed to stderr, since there's no humility there.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use core::sync::atomic::{AtomicU32, Ordering};

use hubris_async::sys::task_slot;
use hubris_async::time::{Ticker, Timer, now};
use hubris_async::{Executor, notification};
use ringbuf::counted_ringbuf;

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
        eprintln!("[async_demo] {event:?}");
        ringbuf::ringbuf_entry!(event);
    }};
}

/// Longest message the echo server accepts.
const ECHO_MAX: usize = 32;

/// How long the echo server can go without a message before the idle
/// watcher logs it.
const ECHO_IDLE_TIMEOUT: u64 = 5_000;

/// Number of messages the echo server has handled. This is how the
/// synchronous main loop talks to the idle watcher.
static ECHO_COUNT: AtomicU32 = AtomicU32::new(0);

/// Reply code for messages too long to echo.
const ECHO_TOO_LONG: u32 = 1;

/// Logs a `Trace::Tick` every `period` ticks, forever.
///
/// This is an ordinary `async fn`, shared by the three ticker tasks below.
/// Each `task!` declares exactly one static task, so running this three times
/// takes three thin wrappers.
async fn tick_forever(name: Name, period: u64) {
    let mut ticker = Ticker::every(period);
    let mut count = 0u32;
    loop {
        ticker.next().await;
        count = count.wrapping_add(1);
        trace!(Trace::Tick {
            name,
            count,
            now: now()
        });
    }
}

hubris_async::task! {
    async fn ticker_fast() {
        tick_forever(Name::Fast, 1_000).await
    }
}

hubris_async::task! {
    async fn ticker_medium() {
        tick_forever(Name::Medium, 2_500).await
    }
}

hubris_async::task! {
    async fn ticker_slow() {
        tick_forever(Name::Slow, 7_000).await
    }
}

hubris_async::task! {
    /// Toggles user LED 0 every `period` ticks.
    async fn blinky(period: u64) {
        let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
        let mut ticker = Ticker::every(period);
        let mut count = 0u32;
        loop {
            ticker.next().await;
            count = count.wrapping_add(1);
            trace!(Trace::LedToggle { count, now: now() });
            // This is a blocking IPC, which stalls the whole executor until
            // `user_leds` replies; that's fine for a quick GPIO toggle.
            let _ = leds.led_toggle(0);
        }
    }
}

hubris_async::task! {
    /// Logs whenever a whole `ECHO_IDLE_TIMEOUT` passes with no messages
    /// echoed.
    async fn echo_idle_watcher() {
        loop {
            let before = ECHO_COUNT.load(Ordering::Relaxed);
            Timer::after(ECHO_IDLE_TIMEOUT).await;
            if ECHO_COUNT.load(Ordering::Relaxed) == before {
                trace!(Trace::EchoIdle { now: now() });
            }
        }
    }
}

hubris_async::task! {
    /// Logs every time the `poke` notification is posted to us.
    async fn poke_watcher() {
        let mut count = 0u32;
        loop {
            notification::wait(notifications::POKE_MASK).await;
            count = count.wrapping_add(1);
            trace!(Trace::Poked { count, now: now() });
        }
    }
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    trace!(Trace::Start);

    let executor = Executor::new();
    executor.spawn(ticker_fast());
    executor.spawn(ticker_medium());
    executor.spawn(ticker_slow());
    executor.spawn(blinky(250));
    executor.spawn(echo_idle_watcher());
    executor.spawn(poke_watcher());

    // The echo server. `executor.recv` runs the async tasks until they're
    // all waiting, then blocks until a message arrives (`Some`) or something
    // an async task is waiting on happens (`None`, and already handled).
    // One byte longer than the longest message we accept: the kernel reports
    // how much it copied, not how much was sent, so a message that fills the
    // buffer may have been truncated.
    let mut buf = [0u8; ECHO_MAX + 1];
    loop {
        let Some(msg) = executor.recv(&mut buf) else {
            continue;
        };

        let len = msg.message_len();
        if len > ECHO_MAX || len > msg.response_capacity() {
            trace!(Trace::EchoRejected {
                op: msg.operation(),
                len,
                capacity: msg.response_capacity(),
            });
            msg.reply(ECHO_TOO_LONG, &[]);
            continue;
        }

        trace!(Trace::Echo {
            op: msg.operation(),
            len,
            now: now()
        });
        msg.reply(0, &buf[..len]);
        ECHO_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
