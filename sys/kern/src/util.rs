// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common utility functions used in various places in the kernel.

/// Utility routine for getting `&mut` to _two_ elements of a slice, at indexes
/// `i` and `j`. `i` and `j` must be distinct, or this will panic.
#[inline(always)]
pub fn index2_distinct<T>(
    elements: &mut [T],
    i: usize,
    j: usize,
) -> (&mut T, &mut T) {
    if i < elements.len() && j < elements.len() && i != j {
        let base = elements.as_mut_ptr();
        // Safety:
        // - i is a valid offset for elements (checked above), base.add(i) is ok
        // - j is a valid offset for elements (checked above), base.add(j) is ok
        // - i and j do not alias (checked above), so we can dereference both
        // - The &muts are returned with the same lifetime as elements,
        //   preventing the caller from producing further aliasing.
        unsafe {
            let iptr = base.add(i);
            let jptr = base.add(j);
            (&mut *iptr, &mut *jptr)
        }
    } else {
        panic!()
    }
}
