// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{collections::HashSet, hash::Hash};

use anyhow::{bail, Result};
use rand::prelude::*;
use rand_chacha::ChaCha20Rng;

use phash::PerfectHash;

////////////////////////////////////////////////////////////////////////////////

/// A owned perfect hash from keys to values. This `struct` is intended for
/// use in codegen, so it doesn't actually expose a way to retrieve items
/// from the table; `phash::PerfectHash` is intended for use at runtime.
pub struct OwnedPerfectHashMap<K, V> {
    pub m: u32,
    pub values: Vec<Option<(K, V)>>,
}

impl<K, V> OwnedPerfectHashMap<K, V>
where
    K: PerfectHash + Hash + Eq,
{
    /// Checks if `m` creates a valid perfect hash with some number of slots
    fn check(values: &[(K, V)], slots: usize, m: u32) -> bool {
        assert!(slots >= values.len());

        let mut vs = values
            .iter()
            .map(|v| v.0.phash(m) as usize % slots)
            .collect::<Vec<usize>>();
        vs.sort_unstable();
        vs.dedup();
        vs.len() == values.len()
    }

    /// Attempt to generate a perfect hash for the given input data
    pub fn build(values: Vec<(K, V)>) -> Result<Self> {
        if values.iter().map(|v| &v.0).collect::<HashSet<_>>().len()
            != values.len()
        {
            bail!("Cannot build a perfect hash with duplicate keys");
        }

        const TRY_COUNT: usize = 10_000;
        let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
        for slots in values.len()..(2 * values.len() + 1) {
            for _ in 0..TRY_COUNT {
                let m = rng.gen();
                if Self::check(&values, slots, m) {
                    let mut out = (0..slots).map(|_| None).collect::<Vec<_>>();
                    for v in values.into_iter() {
                        let index = v.0.phash(m) as usize % slots;
                        assert!(out[index].is_none());
                        out[index] = Some(v);
                    }
                    return Ok(Self { m, values: out });
                }
            }
        }

        bail!("Could not generate perfect hash");
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct OwnedNestedPerfectHashMap<K, V> {
    pub m: u32,
    pub g: Vec<u32>,
    pub values: Vec<Vec<(K, V)>>,
}

impl<K, V> OwnedNestedPerfectHashMap<K, V>
where
    K: PerfectHash + Hash + Eq,
{
    fn id(key: &K, m: u32, g: &[u32]) -> (usize, usize) {
        let i = key.phash(m) as usize % g.len();
        let j = key.phash(g[i]) as usize;
        (i, j)
    }

    /// Checks if `m` and `g` create a valid perfect hash
    ///
    /// If they work, returns a `Vec` of sub-table sizes (for each value
    /// in `g`).  Otherwise, returns `None`
    fn check(values: &[(K, V)], m: u32, g: &[u32]) -> Option<Vec<usize>> {
        // Accumulate un-modded values
        let mut seen: Vec<HashSet<usize>> = vec![HashSet::default(); g.len()];
        for (i, j) in values.iter().map(|(k, _v)| Self::id(k, m, g)) {
            if !seen[i].insert(j) {
                return None;
            }
        }
        // Every entry in the secondary table must be used
        if seen.iter().any(|h| h.is_empty()) {
            return None;
        }
        let mut out = vec![];
        for h in &mut seen {
            let mut vs = h.iter().map(|v| v % h.len()).collect::<Vec<usize>>();
            vs.sort_unstable();
            vs.dedup();
            if vs.len() != h.len() {
                return None;
            }
            out.push(h.len());
        }
        Some(out)
    }

    /// Attempt to generate a perfect hash for the given input data
    pub fn build(values: Vec<(K, V)>) -> Result<Self> {
        if values.iter().map(|v| &v.0).collect::<HashSet<_>>().len()
            != values.len()
        {
            bail!("Cannot build a perfect hash with duplicate keys");
        }

        const TRY_COUNT: usize = 10_000;
        let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
        for slots in 2..16 {
            for _ in 0..TRY_COUNT {
                let m: u32 = rng.gen();
                let mut g = vec![0u32; slots];
                for g in g.iter_mut() {
                    *g = rng.gen();
                }
                if let Some(sizes) = Self::check(&values, m, &g) {
                    let mut out = vec![];
                    for s in &sizes {
                        out.push((0..*s).map(|_| None).collect::<Vec<_>>());
                    }
                    for (k, v) in values.into_iter() {
                        let (i, j) = Self::id(&k, m, &g);
                        let j = j % sizes[i];
                        assert!(out[i][j].is_none());
                        out[i][j] = Some((k, v));
                    }
                    let out = out
                        .into_iter()
                        .map(|o| o.into_iter().map(|v| v.unwrap()).collect())
                        .collect();
                    return Ok(Self { g, m, values: out });
                }
            }
        }

        bail!("Could not generate perfect hash");
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct OwnedSortedList<K, V> {
    pub values: Vec<(K, V)>,
}

impl<K, V> OwnedSortedList<K, V>
where
    K: Eq + Ord,
{
    /// Attempt to generate a perfect hash for the given input data
    pub fn build(mut values: Vec<(K, V)>) -> Result<Self> {
        values.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(Self { values })
    }
}

////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Hash, Eq, PartialEq)]
    struct U(u32);
    impl PerfectHash for U {
        fn phash(&self, b: u32) -> usize {
            self.0.wrapping_mul(b) as usize
        }
    }

    #[derive(Hash, Eq, PartialEq)]
    struct U2(u32, u32);
    impl PerfectHash for U2 {
        fn phash(&self, b: u32) -> usize {
            self.0.wrapping_mul(b).wrapping_add(self.1.wrapping_mul(!b))
                as usize
        }
    }

    fn hash_slots<K: PerfectHash + Hash + Eq>(values: Vec<K>) -> usize {
        let values = values.into_iter().map(|v| (v, ())).collect();
        OwnedPerfectHashMap::build(values).unwrap().values.len()
    }

    fn nested_hash<K: PerfectHash + Hash + Eq>(values: Vec<K>) -> usize {
        let values = values.into_iter().map(|v| (v, ())).collect();
        OwnedNestedPerfectHashMap::build(values).unwrap().g.len()
    }

    #[test]
    fn small_hash() {
        let values: Vec<U> = vec![36, 51, 13, 14].into_iter().map(U).collect();
        assert_eq!(values.len(), hash_slots(values));
    }

    #[test]
    fn medium_hash() {
        let values: Vec<U> =
            vec![36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96]
                .into_iter()
                .map(U)
                .collect();
        assert!(values.len() + 1 >= hash_slots(values));
    }

    #[test]
    fn medium_hash_nested() {
        let values: Vec<U> =
            vec![36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96]
                .into_iter()
                .map(U)
                .collect();
        assert_eq!(nested_hash(values), 2);
    }

    #[test]
    fn tuple_hash() {
        let values = vec![
            U2(2, 0b1),
            U2(3, 0b1),
            U2(4, 0b1),
            U2(5, 0b1),
            U2(5, 0b11),
            U2(8, 0b0),
            U2(9, 0b1),
            U2(9, 0b10),
            U2(9, 0b100),
            U2(9, 0b1000),
        ];
        assert!(values.len() + 1 >= hash_slots(values));
    }

    #[test]
    fn tuple_hash_nested_smol() {
        let values = vec![U2(2, 0b1), U2(3, 0b1)];
        assert_eq!(nested_hash(values), 2);
    }

    #[test]
    fn tuple_hash_nested() {
        let values = vec![
            U2(2, 0b1),
            U2(3, 0b1),
            U2(4, 0b1),
            U2(5, 0b1),
            U2(5, 0b11),
            U2(8, 0b0),
            U2(9, 0b1),
            U2(9, 0b10),
            U2(9, 0b100),
            U2(9, 0b1000),
        ];
        assert_eq!(nested_hash(values), 2);
    }

    #[test]
    fn relative_primes() {
        let values = vec![U(5), U(7)];
        assert!(values.len() + 1 >= hash_slots(values));
    }
}
