// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates the use of `counters!`, `count!`, and `#[derive(Count)]`
//!
//! This example is primarily intended to be used with `cargo expand` to show
//! the macro-generated code for `#[derive(ringbuf::Count)]` and friends.
use counters::*;

#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event<E> {
    SomethingHappened(#[count(children)] E),
    SomeNumber(u32),
}

/// A generic type can derive `Count` even if some of its type parameters don't
/// implement `Count`, provided those fields don't have `#[count(children)]`
/// annotations.
#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum SaySomething<P, T> {
    Hello(#[count(children)] P),
    Value(T),
}

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

counters!(Event<SaySomething<Person, &'static str>>);

fn main() {
    count!(Event::SomethingHappened(SaySomething::Hello(Person::Matt)));
    count!(Event::SomethingHappened(SaySomething::Value(
        "Hello, world!"
    )));
    count!(Event::SomethingHappened(SaySomething::Value(
        "Hello, world!"
    )));
}
