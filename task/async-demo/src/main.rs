// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demo of the `hubris-async` executor.
//!
//! Runs four async tasks inside this one Hubris task:
//!
//! - three "tickers" that each log to the ringbuf at a different period, and
//! - a blinker that toggles user LED 0 via the `user_leds` task.
//!
//! Watch it with `humility ringbuf async_demo`.

#![no_std]
#![no_main]

use hubris_async::Executor;
use hubris_async::time::{Ticker, now};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use userlib::task_slot;

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
}

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Name {
    Fast,
    Medium,
    Slow,
}

counted_ringbuf!(Trace, 32, Trace::None);

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

#[unsafe(export_name = "main")]
fn main() -> ! {
    ringbuf_entry!(Trace::Start);

    let executor = Executor::new(notifications::TIMER_MASK);
    executor.spawn(ticker_fast());
    executor.spawn(ticker_medium());
    executor.spawn(ticker_slow());
    executor.spawn(blinky(250));
    executor.run()
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
