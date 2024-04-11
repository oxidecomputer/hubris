// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Allow std-y things to be used in test. Note that this attribute is a bit of a
// trap for the programmer, because rust-analyzer by default seems to build
// things with test set. This means it's easy to introduce code incompatible
// with no_std without your editor hassling you about it. Beware.
#![cfg_attr(not(test), no_std)]

/// Describes types that act as "slices" (in the very abstract sense) referenced
/// by tasks in syscalls.
///
/// This is not the same thing as a Rust slice in the kernel, because this is
/// just a base-length pair --- taken by itself, it doesn't let you actually
/// _access_ the memory.
///
/// # Invariants
///
/// `s.is_empty()` implies `s.base_addr() == s.end_addr()`, and vice versa.
///
/// `s.base_addr() <= s.end_addr()` must hold.
pub trait UserSlice {
    /// Checks whether the slice spans zero bytes. Empty slices are opted out of
    /// access checking to allow task code to use literals like `&[]`.
    ///
    /// This must be consistent with the base/end addr implementations, such
    /// that `is_empty <==> base_addr == end_addr`.
    fn is_empty(&self) -> bool;

    /// The address of the first byte included in this slice.
    ///
    /// The value returned by `base_addr` must be less than or equal to that
    /// returned by `end_addr`.
    fn base_addr(&self) -> usize;

    /// The address of the first byte _not_ included in this slice, past the
    /// end.
    ///
    /// Note that this prevents a slice from touching the end of the address
    /// space. This is also prevented, in practice, by the definition of several
    /// Rust core types, so we accept it.
    ///
    /// The return value must be greater than or equal to the result of
    /// `base_addr`.
    fn end_addr(&self) -> usize;
}

impl<T: UserSlice> UserSlice for &T {
    #[inline(always)]
    fn is_empty(&self) -> bool {
        (**self).is_empty()
    }

    #[inline(always)]
    fn base_addr(&self) -> usize {
        (**self).base_addr()
    }

    #[inline(always)]
    fn end_addr(&self) -> usize {
        (**self).end_addr()
    }
}

/// Describes types that indicate region permissions.
///
/// This type is _almost_ identical to `UserSlice` but has slightly different
/// operations defined on it. Those operations _do not_ include region
/// attributes, which might be surprising, but we handle those generically using
/// a predicate when required.
///
/// # Invariants
///
/// `r.contains(x)` implies `r.base_addr() <= x < r.end_addr()` and vice versa.
///
/// By extension, `r.base_addr() <= r.end_addr()` must hold.
///
/// An empty region is weird but not impossible.
pub trait MemoryRegion {
    fn contains(&self, addr: usize) -> bool;
    fn base_addr(&self) -> usize;
    fn end_addr(&self) -> usize;
}

impl<T: MemoryRegion> MemoryRegion for &T {
    #[inline(always)]
    fn contains(&self, addr: usize) -> bool {
        (**self).contains(addr)
    }

    #[inline(always)]
    fn base_addr(&self) -> usize {
        (**self).base_addr()
    }

    #[inline(always)]
    fn end_addr(&self) -> usize {
        (**self).end_addr()
    }
}

/// Generic version of the kernel slice access checking code.
///
/// The purpose of this routine is to determine whether a task can access some
/// memory. The memory is described by `slice` and consists of a single
/// contiguous region. The task's memory access permissions are described by
/// `table`, which is an array of region descriptors.
///
/// The exact implementation of both the slice type `S` and the region type `R`
/// are left unspecified here, to avoid needing to rely on kernel-internal
/// types. The requirements for each type are specified by [`UserSlice`] and
/// [`MemoryRegion`], respectively.
///
/// Generally speaking, not all task region descriptors confer the same rights
/// --- some are read-only, some may represent an address space hole that cannot
/// be accessed, etc. To reflect this, this routine takes a `region_ok`
/// predicate over the `R` type. Provide a `region_ok` function to do any access
/// permission checking that you require.
///
/// # Preconditions
///
/// `table` must be sorted by region base address, and the regions in the table
/// must not overlap.
///
/// Both `slice` and each element of `table` must meet the properties described
/// on [`UserSlice`] and [`MemoryRegion`], respectively.
///
/// # Returns
///
/// `true` if `slice` is completely covered by one or more regions in `table`
/// that meet the `region_ok` condition.
///
/// `false` otherwise.
pub fn can_access<S, R>(
    slice: S,
    table: &[R],
    region_ok: impl Fn(&R) -> bool,
) -> bool
where
    S: UserSlice,
    R: MemoryRegion,
{
    if slice.is_empty() {
        // We deliberately omit tests for empty slices, as they confer no
        // authority as far as the kernel is concerned. This is pretty
        // important because a literal like `&[]` tends to produce a base
        // address of `0 + sizeof::<T>()`, which is almost certainly invalid
        // according to the task's region map... but fine with us.
        return true;
    }

    // We need to be convinced that this slice is _entirely covered_ by regions
    // with the right attributes. It _may_ span multiple regions, which is
    // important since the build system can insert region boundaries in
    // unpredictable locations in otherwise innocent programs.  But the regions
    // that are spanned must be perfectly contiguous.

    // Per the function's preconditions, the region table is sorted in ascending
    // order of base address, and the regions within it do not overlap. This
    // lets us use a one-pass algorithm.
    let start_addr = slice.base_addr();
    let end_addr = slice.end_addr();

    let mut i = 0;
    for region in table {
        if region.contains(start_addr) {
            // Make sure it's permissible!
            if !region_ok(region) {
                // The start region wasn't permissible, error out.
                return false;
            }

            if end_addr <= region.end_addr() {
                // We've exhausted the slice in this region, we don't have
                // to continue processing.
                return true;
            }

            // We found our start region, it is permissible and does not
            // contain the end address. We should look for the end region
            // starting from the next region over.
            i += 1;
            break;
        }
        if region.base_addr() > end_addr {
            // We've passed our target address without finding regions that
            // work!
            return false;
        }
        // Continue scanning from the next region
        i += 1;
    }

    // Start region was found; now to find the end region.
    for region in &table[i..] {
        if !region_ok(region) {
            // End or intermediate region wasn't permissible, skip to error
            // handling code at the end.
            break;
        }

        if end_addr <= region.end_addr() {
            // The slice ends in this region, the slice was okay.
            return true;
        }
    }

    // We reach this point by exhausting the region table, or finding a
    // region at a higher address than the slice.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSlice {
        base: usize,
        size: usize,
    }

    impl UserSlice for TestSlice {
        fn is_empty(&self) -> bool {
            self.size == 0
        }

        fn base_addr(&self) -> usize {
            self.base
        }

        fn end_addr(&self) -> usize {
            self.base + self.size
        }
    }

    struct TestRegion {
        base: usize,
        size: usize,
        label: String,
    }

    impl MemoryRegion for TestRegion {
        fn contains(&self, addr: usize) -> bool {
            addr >= self.base && addr < self.end_addr()
        }

        fn base_addr(&self) -> usize {
            self.base
        }

        fn end_addr(&self) -> usize {
            self.base + self.size
        }
    }

    fn make_fake_region_table() -> Vec<TestRegion> {
        vec![
            // Two adjacent good ranges lower in the address space. It's
            // important to some of the tests that there be no mapped regions to
            // either side of this, because we assume that adjacent areas should
            // be inaccessible.
            TestRegion {
                base: 0x0099_0000,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
            TestRegion {
                base: 0x009A_0000,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
            TestRegion {
                base: 0x1234_5678,
                size: 0x0001_0000,
                label: "bad".to_string(),
            },
            TestRegion {
                base: 0x1235_5678,
                size: 0x0001_0000,
                label: "bad".to_string(),
            },
            TestRegion {
                base: 0x1236_5678,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
            TestRegion {
                base: 0x1237_5678,
                size: 0x0001_0000,
                label: "bad".to_string(),
            },
            TestRegion {
                base: 0x1238_5678,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
        ]
    }
    const GOOD_REGION_0_IDX: usize = 0;
    const GOOD_REGION_1_IDX: usize = 1;
    const BAD_REGION_0_IDX: usize = 2;
    const BAD_REGION_1_IDX: usize = 3;
    const GOOD_REGION_2_IDX: usize = 4;
    const GOOD_REGION_3_IDX: usize = 6;

    // Predicate to use when matching _any_ region would be interesting, such as
    // if a slice is expected to be outside all regions.
    fn accept_any_region_wheee(_: &TestRegion) -> bool {
        true
    }

    // Predicate to use when simulating normal access control operations.
    fn accept_only_good_regions(r: &TestRegion) -> bool {
        r.label == "good"
    }

    #[test]
    fn can_access_single_good_region() {
        let region_table = make_fake_region_table();
        for i in [GOOD_REGION_0_IDX, GOOD_REGION_1_IDX] {
            assert!(
                can_access(
                    TestSlice {
                        base: region_table[i].base + 10,
                        size: region_table[i].size - 20,
                    },
                    &region_table,
                    accept_only_good_regions,
                ),
                "should be able to access good region {i} but cannot",
            );
        }
    }

    #[test]
    fn cannot_access_single_bad_region() {
        let region_table = make_fake_region_table();
        for i in [BAD_REGION_0_IDX, BAD_REGION_1_IDX] {
            assert!(
                // load-bearing tiny punctuation character:
                !can_access(
                    TestSlice {
                        base: region_table[i].base + 10,
                        size: region_table[i].size - 20,
                    },
                    &region_table,
                    accept_only_good_regions,
                ),
                "should NOT be able to access bad region {i} but can",
            );
        }
    }

    #[test]
    fn cannot_access_uncontained_memory() {
        let mut last = 0;
        let region_table = make_fake_region_table();
        for region in &region_table {
            if last != region.base_addr() {
                // Fabricate a slice that's between this region and the previous
                // one.
                let slice = TestSlice {
                    base: last,
                    size: region.base_addr() - last,
                };
                assert!(
                    // load-bearing tiny punctuation character:
                    !can_access(
                        slice,
                        &region_table,
                        // We don't want to match _anything._
                        accept_any_region_wheee,
                    ),
                    "should NOT be able to access range {last:#x} - {:#x} but can",
                    region.base_addr(),
                );
            }

            last = region.end_addr();
        }
    }

    #[test]
    fn can_access_overlapping_adjacent_good_regions() {
        let region_table = make_fake_region_table();

        let base = region_table[GOOD_REGION_0_IDX].base + 10;
        let end = region_table[GOOD_REGION_1_IDX].end_addr() - 10;
        let slice = TestSlice {
            base,
            size: end - base,
        };

        assert!(
            can_access(slice, &region_table, accept_only_good_regions,),
            "should be able to access slice that spans adjacent ranges, cannot",
        );
    }

    #[test]
    fn cannot_access_overlapping_adjacent_bad_regions() {
        let region_table = make_fake_region_table();

        let base = region_table[BAD_REGION_0_IDX].base + 10;
        let end = region_table[BAD_REGION_1_IDX].end_addr() - 10;
        let slice = TestSlice {
            base,
            size: end - base,
        };

        assert!(
            // Load-bearing tiny punctuation character:
            !can_access(
                slice,
                &region_table,
                accept_only_good_regions,
            ),
            "should NOT be able to access slice that spans adjacent bad ranges, but can",
        );
    }

    #[test]
    fn cannot_access_contiguous_regions_with_bad_region_interleaved() {
        let region_table = make_fake_region_table();

        let base = region_table[GOOD_REGION_2_IDX].base + 10;
        let end = region_table[GOOD_REGION_3_IDX].end_addr() - 10;
        let slice = TestSlice {
            base,
            size: end - base,
        };

        assert!(
            // Load-bearing tiny punctuation character:
            !can_access(
                slice,
                &region_table,
                accept_only_good_regions,
            ),
            "should NOT be able to access slice that starts and ends in good ranges but passes through bad one, but can",
        );
    }

    #[test]
    fn cannot_access_slice_spanning_over_uncontained_memory() {
        // Using a custom region table to not cause cannot_access_uncontained_memory
        // to spuriously fail.
        let region_table = vec![
            TestRegion {
                base: 0x1238_5678,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
            TestRegion {
                // 65kb separated from previous region
                base: 0x123A_5678,
                size: 0x0001_0000,
                label: "good".to_string(),
            },
        ];

        let base = region_table[GOOD_REGION_0_IDX].base + 10;
        let end = region_table[GOOD_REGION_1_IDX].end_addr() - 10;
        let slice = TestSlice {
            base,
            size: end - base,
        };

        assert!(
            // Load-bearing tiny punctuation character:
            !can_access(
                slice,
                &region_table,
                accept_only_good_regions,
            ),
            "should NOT be able to access slice that starts and ends in good ranges but passes through uncontained memory, but can",
        );
    }
}
