// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates the use of `counted_ringbuf!` and friends.
//!
//! This example is primarily intended to be used with `cargo expand` to show
//! the macro-generated code for `#[derive(ringbuf::Count)]` and friends.`
#![no_std]
#![no_main]

#[derive(ringbuf::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event {
    NothingHappened,
    SomethingHappened,
    SomethingElse(u32),
    SecretThirdThing { secret: () },
}

ringbuf::counted_ringbuf!(Event, 16, Event::NothingHappened);
ringbuf::counted_ringbuf!(MY_NAMED_RINGBUF, Event, 16, Event::NothingHappened);

pub fn example() {
    ringbuf::count_entry!(Event::SomethingHappened);
}

pub fn example_named() {
    ringbuf::count_entry!(MY_NAMED_RINGBUF, Event::SomethingElse(420));
}

// This is just necessary to make the example compile.
#[panic_handler]
fn _die() -> ! {
    loop {}
}
