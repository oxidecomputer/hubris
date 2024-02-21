// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates the use of `counted_ringbuf!` and friends.
//!
//! This example is primarily intended to be used with `cargo expand` to show
//! the macro-generated code for `#[derive(ringbuf::Count)]` and friends.
use ringbuf::*;

#[derive(ringbuf::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event {
    NothingHappened,
    SomethingHappened,
    SomethingElse(u32),
    SecretThirdThing { secret: () },
}

counted_ringbuf!(Event, 16, Event::NothingHappened);
counted_ringbuf!(MY_NAMED_RINGBUF, Event, 16, Event::NothingHappened);

ringbuf!(NON_COUNTED_RINGBUF, Event, 16, Event::NothingHappened);

pub fn example() {
    ringbuf_entry!(Event::SomethingHappened);
    ringbuf_entry!(NON_COUNTED_RINGBUF, Event::SomethingHappened);
}

pub fn example_named() {
    ringbuf_entry!(MY_NAMED_RINGBUF, Event::SomethingElse(420));
}

pub mod nested {
    use super::Event;

    ringbuf::counted_ringbuf!(Event, 16, Event::NothingHappened);

    pub fn example() {
        ringbuf::ringbuf_entry!(Event::SomethingHappened);
        ringbuf::ringbuf_entry_root!(Event::SomethingElse(666));
        ringbuf::ringbuf_entry_root!(
            MY_NAMED_RINGBUF,
            Event::SecretThirdThing { secret: () }
        );
    }
}

fn main() {}
