// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common utility functions used in various places in the kernel.

/// Utility routine for getting `&mut` to _two_ elements of a slice, at indexes
/// `i` and `j`. `i` and `j` must be distinct, or this will panic.
#[allow(clippy::comparison_chain)]
pub fn index2_distinct<T>(
    elements: &mut [T],
    i: usize,
    j: usize,
) -> (&mut T, &mut T) {
    if i < j {
        let (prefix, suffix) = elements.split_at_mut(i + 1);
        (&mut prefix[i], &mut suffix[j - (i + 1)])
    } else if j < i {
        let (prefix, suffix) = elements.split_at_mut(j + 1);
        (&mut suffix[i - (j + 1)], &mut prefix[j])
    } else {
        panic!()
    }
}
