// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Demonstrates deriving `counters::Count` for a newtype around an integer
//! with a bounded range, via the `#[count(max = N)]` attribute.
//!
//! A raw `u8` cannot derive `Count` (it would generate a 256-entry counter
//! table); a bounded newtype generates one counter per value in `0..=N`.
//! This example verifies the generated counters behave correctly, and is
//! primarily intended to be used with `cargo expand` to show the
//! macro-generated code.
use core::sync::atomic::Ordering;
use counters::*;

/// A PSU number, always in `0..=5` at runtime even though it's stored as a
/// `u8`. This is the same shape as the `Slot` enum in
/// `drv/psc-seq-server/src/main.rs`, without the `#[repr(u8)]` enum
/// boilerplate.
#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
#[count(max = 5)]
pub struct Slot(u8);

#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Event {
    SomeNumber(u32),
    Psu(#[count(children)] Slot),
}

counters!(SLOT_COUNTERS, Slot);
counters!(EVENT_COUNTERS, Event);

fn main() {
    count!(SLOT_COUNTERS, Slot(0));
    count!(SLOT_COUNTERS, Slot(0));
    count!(SLOT_COUNTERS, Slot(3));
    // Values outside `0..=max` are not counted (see the `_ => {}` arm in the
    // generated `count` implementation).
    count!(SLOT_COUNTERS, Slot(255));

    count!(EVENT_COUNTERS, Event::SomeNumber(42));
    count!(EVENT_COUNTERS, Event::Psu(Slot(0)));
    count!(EVENT_COUNTERS, Event::Psu(Slot(4)));

    let relaxed = Ordering::Relaxed;
    assert_eq!(SLOT_COUNTERS._0.load(relaxed), 2);
    assert_eq!(SLOT_COUNTERS._1.load(relaxed), 0);
    assert_eq!(SLOT_COUNTERS._3.load(relaxed), 1);
    assert_eq!(SLOT_COUNTERS._5.load(relaxed), 0);

    assert_eq!(EVENT_COUNTERS.SomeNumber.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._0.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._4.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._5.load(relaxed), 0);

    println!(
        "Slot:     _0={}, _1={}, _2={}, _3={}, _4={}, _5={}",
        SLOT_COUNTERS._0.load(relaxed),
        SLOT_COUNTERS._1.load(relaxed),
        SLOT_COUNTERS._2.load(relaxed),
        SLOT_COUNTERS._3.load(relaxed),
        SLOT_COUNTERS._4.load(relaxed),
        SLOT_COUNTERS._5.load(relaxed),
    );
    println!(
        "Event:    SomeNumber={}, Psu._0={}, Psu._4={}",
        EVENT_COUNTERS.SomeNumber.load(relaxed),
        EVENT_COUNTERS.Psu._0.load(relaxed),
        EVENT_COUNTERS.Psu._4.load(relaxed),
    );
    println!("ok");
}
