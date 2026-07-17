// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Verifies that the unnamed `ringbuf!(T, N, init, no_dedup)` form delegates
//! correctly to the named arm. One static (__RINGBUF), one test function.

use ringbuf::RecordEntry;

ringbuf::ringbuf!(u32, 4, 0u32, no_dedup);

#[test]
fn unnamed_no_dedup_records_entry() {
    __RINGBUF.record_entry(1, 55u32);

    let ring = __RINGBUF.try_borrow_mut().unwrap();
    assert_eq!(ring.last, Some(0));
    assert_eq!(ring.buffer[0].payload, 55u32);
}
