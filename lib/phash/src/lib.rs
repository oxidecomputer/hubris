// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

/// This is a trait for things that can be reduced to a `u32` in combination
/// with another thing of the same shape. In practice, it is used to reduce
/// either an `irq: u32` or a (task_id, mask): (u32, u32)` to a single `u32`
pub trait Reduce {
    fn reduce(&self, b: Self) -> u32;
}

impl Reduce for u32 {
    #[inline(always)]
    fn reduce(&self, b: Self) -> u32 {
        self.wrapping_mul(b)
    }
}

impl Reduce for (u32, u32) {
    #[inline(always)]
    fn reduce(&self, b: Self) -> u32 {
        self.0
            .wrapping_mul(b.0)
            .wrapping_add(self.1.wrapping_mul(b.1))
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct PerfectHash<'a, K, V> {
    pub m: K,
    pub values: &'a [V],
}

impl<'a, K: Copy + Reduce, V> PerfectHash<'a, K, V> {
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
    pub fn get(&self, key: K) -> &V {
        let i = key.reduce(self.m) as usize % self.values.len();
        &self.values[i]
    }
}
