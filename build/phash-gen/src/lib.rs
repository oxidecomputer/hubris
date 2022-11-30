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
            .map(|v| v.0.phash(m) % slots)
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

        const TRY_COUNT: usize = 1_000;
        let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
        for slots in values.len()..(2 * values.len() + 1) {
            for _ in 0..TRY_COUNT {
                let m = rng.gen();
                if Self::check(&values, slots, m) {
                    let mut out = (0..slots).map(|_| None).collect::<Vec<_>>();
                    for v in values.into_iter() {
                        let index = v.0.phash(m) % slots;
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
    pub values: Vec<Vec<Option<(K, V)>>>,
}

impl<K, V> OwnedNestedPerfectHashMap<K, V>
where
    K: PerfectHash + Hash + Eq,
{
    fn id(key: &K, m: u32, g: &[u32]) -> (usize, usize) {
        let i = key.phash(m) % g.len();
        let j = key.phash(g[i]);
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
        let mut out = vec![];
        for h in &mut seen {
            let mut found = false;
            for slots in h.len()..(h.len() * 2 + 1) {
                let mut vs =
                    h.iter().map(|v| v % slots).collect::<Vec<usize>>();
                vs.sort_unstable();
                vs.dedup();
                if vs.len() == h.len() {
                    found = true;
                    out.push(slots);
                    break;
                }
            }
            if !found {
                return None;
            }
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

        const TRY_COUNT: usize = 1_000;
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

    #[derive(Copy, Clone, Hash, Eq, PartialEq)]
    struct U(u32);
    impl PerfectHash for U {
        fn phash(&self, b: u32) -> usize {
            self.0.wrapping_mul(b) as usize
        }
    }

    #[derive(Copy, Clone, Hash, Eq, PartialEq)]
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
        let values: Vec<(U, ())> =
            vec![36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96]
                .into_iter()
                .map(|i| (U(i), ()))
                .collect();
        assert!(OwnedNestedPerfectHashMap::build(values).is_ok());
    }

    #[test]
    fn large_hash() {
        let values: Vec<(U, ())> = vec![
            0, 3, 6, 9, 13, 19, 22, 29, 37, 40, 42, 49, 53, 58, 59, 69, 70, 73,
            77, 79, 85, 86, 92, 94, 100, 104, 115, 117, 123, 130, 138, 142,
            143, 145, 147, 148, 151, 155, 165, 168, 171, 176, 186, 187, 198,
            204, 205, 210, 218, 219, 222, 227, 228, 229, 236, 244, 247, 248,
            249, 250, 255,
        ]
        .into_iter()
        .map(|i| (U(i), ()))
        .collect();
        assert!(OwnedNestedPerfectHashMap::build(values).is_ok());
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
        let values = vec![(2, 0b1), (3, 0b1)]
            .into_iter()
            .map(|(a, b)| (U2(a, b), ()))
            .collect();
        assert!(OwnedNestedPerfectHashMap::build(values).is_ok());
    }

    #[test]
    fn tuple_hash_nested() {
        let values = vec![
            (2, 0b1),
            (3, 0b1),
            (4, 0b1),
            (5, 0b1),
            (5, 0b11),
            (8, 0b0),
            (9, 0b1),
            (9, 0b10),
            (9, 0b100),
            (9, 0b1000),
        ]
        .into_iter()
        .map(|(a, b)| (U2(a, b), ()))
        .collect();
        assert!(OwnedNestedPerfectHashMap::build(values).is_ok());
    }

    #[test]
    fn relative_primes() {
        let values = vec![U(5), U(7)];
        assert!(values.len() + 1 >= hash_slots(values));
    }
}
