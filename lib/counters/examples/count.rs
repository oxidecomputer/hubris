// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates the use of `counters!`, `count!`, and `#[derive(Count)]`
//!
//! This example is primarily intended to be used with `cargo expand` to show
//! the macro-generated code for `#[derive(ringbuf::Count)]` and friends.
use counters::*;

#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event {
    SomethingHappened,
    SayHello(#[count(children)] people::Person),
    SomeNumber(u32),
    ToBeOrNotToBe(#[count(children)] bool),
}

counters!(Event);

fn main() {
    count!(Event::SomethingHappened);
    count!(Event::SomeNumber(42));

    people::say_hello();
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
