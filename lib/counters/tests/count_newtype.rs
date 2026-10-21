// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Verifies deriving `counters::Count` for a newtype around an unsigned
//! integer with a bounded range, via the `#[count(max = N)]` attribute.
//!
//! A raw `u8` cannot derive `Count` (it would generate a 256-entry counter
//! table); a bounded newtype generates one counter per value in `0..=N`,
//! plus a single `_other` bucket for out-of-range values.
//!
//! This is primarily intended to be inspected with `cargo expand` to show
//! the macro-generated code.
use core::sync::atomic::Ordering;
use counters::*;

/// A PSU number, always in `0..=5` at runtime even though it's stored as a
/// `u8`. This is the same shape as the `Slot` newtype in
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

#[test]
fn counts_newtype_and_nested_children() {
    count!(SLOT_COUNTERS, Slot(0));
    count!(SLOT_COUNTERS, Slot(0));
    count!(SLOT_COUNTERS, Slot(3));

    count!(EVENT_COUNTERS, Event::SomeNumber(42));
    count!(EVENT_COUNTERS, Event::Psu(Slot(0)));
    count!(EVENT_COUNTERS, Event::Psu(Slot(4)));

    let relaxed = Ordering::Relaxed;
    assert_eq!(SLOT_COUNTERS._0.load(relaxed), 2);
    assert_eq!(SLOT_COUNTERS._1.load(relaxed), 0);
    assert_eq!(SLOT_COUNTERS._3.load(relaxed), 1);
    assert_eq!(SLOT_COUNTERS._5.load(relaxed), 0);
    assert_eq!(SLOT_COUNTERS._other.load(relaxed), 0);

    assert_eq!(EVENT_COUNTERS.SomeNumber.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._0.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._4.load(relaxed), 1);
    assert_eq!(EVENT_COUNTERS.Psu._5.load(relaxed), 0);
    assert_eq!(EVENT_COUNTERS.Psu._other.load(relaxed), 0);
}

/// A second bounded newtype, used so the out-of-range test exercises its own
/// counter set (the test harness runs `#[test]`s in parallel, so two tests
/// must not mutate the same statics).
#[derive(Count, Debug, Copy, Clone, PartialEq, Eq)]
#[count(max = 7)]
pub struct Byte(u8);

counters!(BYTE_COUNTERS, Byte);

#[test]
fn buckets_out_of_range_values() {
    count!(BYTE_COUNTERS, Byte(7));
    count!(BYTE_COUNTERS, Byte(8));
    count!(BYTE_COUNTERS, Byte(255));

    let relaxed = Ordering::Relaxed;
    assert_eq!(BYTE_COUNTERS._7.load(relaxed), 1);
    assert_eq!(BYTE_COUNTERS._other.load(relaxed), 2);
    assert_eq!(BYTE_COUNTERS._0.load(relaxed), 0);
}
