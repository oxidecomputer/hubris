// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Verifies that the named no_dedup ringbuf arm records its first entry into
//! slot 0. One static, one test function — no shared state with other files.

use ringbuf::RecordEntry;

ringbuf::ringbuf!(BUF_FIRST, u32, 4, 0u32, no_dedup);

#[test]
fn records_first_entry() {
    BUF_FIRST.record_entry(1, 7u32);

    let ring = BUF_FIRST.try_borrow_mut().unwrap();
    assert_eq!(ring.last, Some(0));
    assert_eq!(ring.buffer[0].payload, 7u32);
    assert_eq!(ring.buffer[0].line, 1);
}
