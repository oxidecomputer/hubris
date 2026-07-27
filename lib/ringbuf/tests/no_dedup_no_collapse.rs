// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Verifies that identical consecutive entries each consume a new slot rather
//! than incrementing a counter — the defining behaviour of no_dedup.
//! One static, one test function — no shared state with other files.

use ringbuf::RecordEntry;

ringbuf::ringbuf!(BUF_COLLAPSE, u32, 4, 0u32, no_dedup);

#[test]
fn identical_entries_are_not_collapsed() {
    BUF_COLLAPSE.record_entry(1, 99u32);
    BUF_COLLAPSE.record_entry(1, 99u32);

    let ring = BUF_COLLAPSE.try_borrow_mut().unwrap();
    assert_eq!(ring.last, Some(1));
    assert_eq!(ring.buffer[0].payload, 99u32);
    assert_eq!(ring.buffer[1].payload, 99u32);
}
