// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Fixed map
//!
//! This contains a very simple implementation of a fixed-sized map, with
//! keys of type `K` and values of type `V`.  Keys and values are both stored
//! by value: both must implement `Copy`, and keys must implement `PartialEq`.
//! It is up to callers to assure that the map doesn't overflow; an attempt
//! to [`FixedMap::insert`] when the map is full will result in a `panic!`.

#![no_std]

///
/// A fixed-size map of size `N`, mapping keys of type `K` to values of
/// type `V`.
///
#[derive(Debug)]
pub struct FixedMap<K, V, const N: usize> {
    contents: [Option<(K, V)>; N],
}

impl<K: Copy, V: Copy, const N: usize> Default for FixedMap<K, V, { N }> {
    /// Create an empty `FixedMap`.
    fn default() -> Self {
        Self {
            contents: [None; N],
        }
    }
}

impl<K: Copy + PartialEq, V: Copy, const N: usize> FixedMap<K, V, { N }> {
    ///
    /// Gets the value that corresponds to `key`, returning `None` if no
    /// such key is in the map.
    ///
    pub fn get(&self, key: K) -> Option<V> {
        for i in 0..self.contents.len() {
            match self.contents[i] {
                None => {
                    break;
                }
                Some((k, v)) => {
                    if k == key {
                        return Some(v);
                    }
                }
            }
        }

        None
    }

    ///
    /// Inserts the `value` into the map for the specified `key`.  If the
    /// specified key already exists in the map, its value will be overwritten
    /// with the specified value.  It is up to the caller to assure that there
    /// is room in the map; if the map is full, this code will panic.
    ///
    pub fn insert(&mut self, key: K, value: V) {
        for i in 0..self.contents.len() {
            match self.contents[i] {
                None => {
                    self.contents[i] = Some((key, value));
                    return;
                }

                Some((k, _)) => {
                    if k == key {
                        self.contents[i] = Some((key, value));
                        return;
                    }
                }
            }
        }

        panic!();
    }

    ///
    /// Removes the specified key from the map.
    ///
    pub fn remove(&mut self, key: K) {
        let mut found = None;
        let mut swap = None;

        for i in 0..self.contents.len() {
            match self.contents[i] {
                None => {
                    break;
                }
                Some((k, _)) => {
                    if k == key {
                        found = Some(i);
                    } else if found.is_some() {
                        swap = Some(i);
                    }
                }
            }
        }

        match (found, swap) {
            (Some(found), Some(swap)) => {
                self.contents[found] = self.contents[swap];
                self.contents[swap] = None;
            }

            (Some(found), None) => {
                self.contents[found] = None;
            }

            (_, _) => {}
        }
    }
}
