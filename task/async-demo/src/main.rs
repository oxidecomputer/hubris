// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demo of the `hubris-async` executor.
//!
//! Runs six async tasks inside this one Hubris task:
//!
//! - three "tickers" that each log to the ringbuf at a different period,
//! - a blinker that toggles user LED 0 via the `user_leds` task,
//! - an echo server that replies to every IPC message with its own body, and
//!   logs if it goes 5 seconds without receiving one, and
//! - a watcher that logs each time the `poke` notification is posted.
//!
//! The `async_demo_client` task drives the last two.
//!
//! Watch it with `humility ringbuf async_demo`.

#![no_std]
#![no_main]

use hubris_async::time::{Ticker, Timer, now};
use hubris_async::{Either, Executor, ipc, notification, select};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use userlib::{sys_reply, task_slot};

task_slot!(USER_LEDS, user_leds);

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
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
enum Name {
    Fast,
    Medium,
    Slow,
}

counted_ringbuf!(Trace, 32, Trace::None);

/// Longest message the echo server accepts.
const ECHO_MAX: usize = 32;

/// How long the echo server waits for a message before logging that it's
/// idle.
const ECHO_IDLE_TIMEOUT: u64 = 5_000;

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
        ringbuf_entry!(Trace::Tick {
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
            ringbuf_entry!(Trace::LedToggle { count, now: now() });
            // This is a blocking IPC, which stalls the whole executor until
            // `user_leds` replies; that's fine for a quick GPIO toggle.
            let _ = leds.led_toggle(0);
        }
    }
}

hubris_async::task! {
    /// Replies to every IPC message with the message's own body.
    async fn echo_server() {
        let mut buf = [0u8; ECHO_MAX];
        loop {
            let recv = ipc::recv(&mut buf);
            let msg = match select(recv, Timer::after(ECHO_IDLE_TIMEOUT)).await {
                Either::First(msg) => msg,
                Either::Second(()) => {
                    ringbuf_entry!(Trace::EchoIdle { now: now() });
                    continue;
                }
            };

            let len = msg.message_len;
            if len > buf.len() || len > msg.response_capacity {
                ringbuf_entry!(Trace::EchoRejected {
                    op: msg.operation,
                    len,
                    capacity: msg.response_capacity,
                });
                sys_reply(msg.sender, ECHO_TOO_LONG, &[]);
                continue;
            }

            ringbuf_entry!(Trace::Echo { op: msg.operation, len, now: now() });
            sys_reply(msg.sender, 0, &buf[..len]);
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
            ringbuf_entry!(Trace::Poked { count, now: now() });
        }
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    ringbuf_entry!(Trace::Start);

    let executor = Executor::<ECHO_MAX>::new(notifications::TIMER_MASK);
    executor.spawn(ticker_fast());
    executor.spawn(ticker_medium());
    executor.spawn(ticker_slow());
    executor.spawn(blinky(250));
    executor.spawn(echo_server());
    executor.spawn(poke_watcher());
    executor.run()
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
