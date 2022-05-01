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
    /// Tries to generate a perfect hash, returning the hash on success and
    /// the input `Vec` on failure (so it can be reused).
    ///
    /// - `values` is data that will be owned by the resulting hash
    /// - `key` is a function which derives a key from a value
    /// - `n` is the number of slots in the resulting hash; this must be
    ///   `>= values.len()`
    /// - `rng` is a random number generator
    fn try_gen<R: rand::Rng>(
        values: Vec<(K, V)>,
        n: usize,
        rng: &mut R,
    ) -> Result<Self, Vec<(K, V)>> {
        assert!(n >= values.len());

        let m = rng.gen();

        let mut vs = values
            .iter()
            .map(|v| v.0.phash(m) as usize % n)
            .collect::<Vec<usize>>();
        vs.sort_unstable();
        vs.dedup();
        if vs.len() != values.len() {
            return Err(values);
        }

        let mut out = (0..n).map(|_| None).collect::<Vec<_>>();
        for v in values.into_iter() {
            let index = v.0.phash(m) as usize % n;
            assert!(out[index].is_none());
            out[index] = Some(v);
        }

        Ok(OwnedPerfectHashMap { m, values: out })
    }

    /// Attempt to generate a perfect hash for the given input data
    pub fn build(mut values: Vec<(K, V)>) -> Result<Self> {
        if values.iter().map(|v| &v.0).collect::<HashSet<_>>().len()
            != values.len()
        {
            bail!("Cannot build a perfect hash with duplicate keys");
        }

        const TRY_COUNT: usize = 10_000;
        let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
        for p in values.len()..(2 * values.len()) {
            for _ in 0..TRY_COUNT {
                let mut tmp_values = vec![];
                std::mem::swap(&mut values, &mut tmp_values);
                match OwnedPerfectHashMap::try_gen(tmp_values, p, &mut rng) {
                    Ok(out) => return Ok(out),
                    Err(vs) => values = vs,
                }
            }
        }

        bail!("Could not generate perfect hash");
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
    fn relative_primes() {
        let values = vec![U(5), U(7)];
        assert!(values.len() + 1 >= hash_slots(values));
    }
}
