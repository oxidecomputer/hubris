// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

/// This is a trait for things that can be reduced to a `usize` in combination
/// with another thing of the same shape. In practice, it is used to reduce
/// either an `irq: u32` or a (task_id, mask): (u32, u32)` to a single `u32`
pub trait PerfectHash {
    fn phash(&self, b: u32) -> usize;
}

////////////////////////////////////////////////////////////////////////////////

pub struct PerfectHashMap<'a, K, V> {
    pub m: u32,
    pub values: &'a [(K, V)],
}

impl<'a, K: Copy + PerfectHash + PartialEq, V> PerfectHashMap<'a, K, V> {
    /// Looks up a value in the table by key.
    ///
    /// If the value associated with the `key` argument _was not_ stored in the
    /// table, this will still return _something_. It's up to the caller
    /// to check whether the returned value matches the key.
    ///
    /// In practice, this means that the value should probably contain the
    /// key, but the specific implementation is left as an exercise for
    /// the reader.
    #[inline(always)]
    pub fn get(&self, key: K) -> Option<&V> {
        let i = key.phash(self.m) % self.values.len();
        if key == self.values[i].0 {
            Some(&self.values[i].1)
        } else {
            None
        }
    }

    /// Looks up a value in the table by key, using a linear search.
    ///
    /// This is slower than [Self::get] in most cases, but is faster for small
    /// tables on a system without hardware division.
    #[inline(always)]
    pub fn get_linear(&self, key: K) -> Option<&V> {
        self.values.iter().find(|v| v.0 == key).map(|v| &v.1)
    }
}
