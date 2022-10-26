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

impl PerfectHash for u32 {
    fn phash(&self, b: u32) -> usize {
        self.wrapping_mul(b) as usize
    }
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
        if self.values.is_empty() {
            return None;
        }
        let i = key.phash(self.m) % self.values.len();
        if key == self.values[i].0 {
            Some(&self.values[i].1)
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.values.iter()
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
        if self.g.is_empty() {
            return None;
        }
        let i = key.phash(self.m) % self.g.len();
        if self.values[i].is_empty() {
            return None;
        }
        let j = key.phash(self.g[i]) % self.values[i].len();
        if key == self.values[i][j].0 {
            Some(&self.values[i][j].1)
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.values.iter().flat_map(|s| s.iter())
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

    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.values.iter()
    }
}
