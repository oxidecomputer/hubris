// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates the use of `counters!`, `count!`, and `#[derive(Count)]`
//!
//! This example is primarily intended to be used with `cargo expand` to show
//! the macro-generated code for `#[derive(ringbuf::Count)]` and friends.
use std::sync::atomic::Ordering;

use counters::*;

#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event {
    SomethingHappened,
    SayHello(#[count(children)] people::Person),
    SomeNumber(u32),
    ToBeOrNotToBe(#[count(children)] bool),
}

#[derive(Count, Debug)]
struct WrapperEvent {
    #[count(children)]
    event: Event,
    _cool_stuff: u64,
}
counters!(Event);

fn main() {
    count!(Event::SomethingHappened);
    count!(Event::SomeNumber(42));

    people::say_hello();

    let wrapper_counters = <WrapperEvent as Count>::NEW_COUNTERS;
    let event_counters = <Event as Count>::NEW_COUNTERS;
    count!(
        wrapper_counters,
        WrapperEvent {
            event: Event::SomeNumber(16),
            _cool_stuff: 100,
        }
    );
    count!(event_counters, Event::SomeNumber(16));

    assert_eq!(
        wrapper_counters.SomeNumber.load(Ordering::Relaxed),
        event_counters.SomeNumber.load(Ordering::Relaxed),
    );
}

mod people {
    use super::Event;
    use counters::*;

    #[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
    pub enum Person {
        Cliff,
        Matt,
        Laura,
        Bryan,
        John,
        Steve,
        Eliza,
    }

    pub fn say_hello() {
        use Person::*;
        for &person in &[Cliff, Matt, Laura, Bryan, John, Steve, Eliza] {
            count!(crate::__COUNTERS, Event::SayHello(person));
        }
    }
}
