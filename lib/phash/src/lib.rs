// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

/// This is a trait for things that can be reduced to a `usize` in combination
/// with a `u32`. In practice, it is used to reduce either an `irq: u32` or a
/// (task_id, mask): (u32, u32)` to a single `u32`
pub trait PerfectHash {
    fn phash(&self, b: u32) -> usize;
}

////////////////////////////////////////////////////////////////////////////////

pub struct PerfectHashMap<'a, K, V> {
    pub m: u32,
    pub values: &'a [(K, V)],
}

impl<'a, K: Copy + PerfectHash + PartialEq, V> PerfectHashMap<'a, K, V> {
    /// Looks up a value in the table by key, returning `None` if the key was
    /// not stored in the table.
    #[inline(always)]
    pub fn get(&self, key: K) -> Option<&V> {
        let i = key.phash(self.m) % self.values.len();
        if key == self.values[i].0 {
            Some(&self.values[i].1)
        } else {
            None
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct NestedPerfectHashMap<'a, K, V> {
    pub m: u32,
    pub g: &'a [u32],
    pub values: &'a [&'a [(K, V)]],
}

impl<'a, K: Copy + PerfectHash + PartialEq, V> NestedPerfectHashMap<'a, K, V> {
    /// Looks up a value in the table by key, returning `None` if the key was
    /// not stored in the table.
    #[inline(always)]
    pub fn get(&self, key: K) -> Option<&V> {
        let i = key.phash(self.m) % self.g.len();
        let j = key.phash(self.g[i]) % self.values[i].len();
        if key == self.values[i][j].0 {
            Some(&self.values[i][j].1)
        } else {
            None
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct SortedList<'a, K, V> {
    pub values: &'a [(K, V)],
}

impl<'a, K: Copy + PerfectHash + PartialEq + Ord, V> SortedList<'a, K, V> {
    /// Looks up a value in the table by key, returning `None` if the key was
    /// not stored in the table.
    #[inline(always)]
    pub fn get(&self, key: K) -> Option<&V> {
        self.values
            .binary_search_by_key(&key, |v| v.0)
            .ok()
            .map(|i| &self.values[i].1)
    }
}
