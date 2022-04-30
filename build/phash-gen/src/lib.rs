// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{collections::HashSet, hash::Hash};

use anyhow::{bail, Result};
use rand::prelude::*;
use rand_chacha::ChaCha20Rng;

use phash::Reduce;

////////////////////////////////////////////////////////////////////////////////

/// A owned perfect hash from keys to values. This `struct` is intended for
/// use in codegen, so it doesn't actually expose a way to retrieve items
/// from the table; `phash::PerfectHash` is intended for use at runtime.
#[derive(Debug)]
pub struct OwnedPerfectHash<K, V> {
    pub m: K,
    pub values: Vec<Option<V>>,
}

impl<K, V> OwnedPerfectHash<K, V>
where
    K: Copy + Clone + Reduce,
    rand::distributions::Standard: Distribution<K>,
{
    /// Tries to generate a perfect hash, returning the hash on success and
    /// the input `Vec` on failure (so it can be reused).
    ///
    /// - `values` is data that will be owned by the resulting hash
    /// - `key` is a function which derives a key from a value
    /// - `n` is the number of slots in the resulting hash; this must be
    ///   `>= values.len()`
    /// - `rng` is a random number generator
    fn try_gen<R: rand::Rng, F: Fn(&V) -> K>(
        values: Vec<V>,
        key: &F,
        n: usize,
        rng: &mut R,
    ) -> Result<Self, Vec<V>> {
        assert!(n >= values.len());

        let m = rng.gen::<K>();

        let mut vs = values
            .iter()
            .map(|v| key(v).reduce(m) as usize % n)
            .collect::<Vec<usize>>();
        vs.sort_unstable();
        vs.dedup();
        if vs.len() != values.len() {
            return Err(values);
        }

        let mut out = (0..n).map(|_| None).collect::<Vec<_>>();
        for v in values.into_iter() {
            let index = key(&v).reduce(m) as usize % n;
            assert!(out[index].is_none());
            out[index] = Some(v);
        }

        Ok(OwnedPerfectHash { m, values: out })
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Attempt to generate a perfect hash for the given input data
pub fn generate_hash<K, V, F>(
    mut values: Vec<V>,
    key: F,
) -> Result<OwnedPerfectHash<K, V>>
where
    V: Eq + Hash,
    K: Copy + Clone + Reduce + std::fmt::Debug,
    rand::distributions::Standard: Distribution<K>,
    F: Fn(&V) -> K,
{
    if values.iter().collect::<HashSet<_>>().len() != values.len() {
        bail!("Cannot build a perfect hash with duplicate elements");
    }

    const TRY_COUNT: usize = 10_000;
    let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
    for p in values.len()..(2 * values.len()) {
        for _ in 0..TRY_COUNT {
            let mut tmp_values = vec![];
            std::mem::swap(&mut values, &mut tmp_values);
            match OwnedPerfectHash::try_gen(tmp_values, &key, p, &mut rng) {
                Ok(out) => return Ok(out),
                Err(vs) => values = vs,
            }
        }
    }

    bail!("Could not generate perfect hash");
}

////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn it_works() {
        let irqs: Vec<u32> = vec![36, 51, 13, 14];
        println!("{:?}", generate_hash(irqs, |i| *i).unwrap());

        let irqs: Vec<u32> =
            vec![36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96];
        println!("{:?}", generate_hash(irqs, |i| *i).unwrap());

        let tuples: Vec<(u32, u32)> = vec![
            (2, 0b1),
            (3, 0b1),
            (4, 0b1),
            (5, 0b1),
            (9, 0b1),
            (9, 0b10),
            (9, 0b100),
            (9, 0b1000),
        ];
        println!("{:?}", generate_hash(tuples, |i| *i).unwrap());

        let tuples: Vec<u32> = vec![5, 7];
        println!("{:?}", generate_hash(tuples, |i| *i).unwrap());
    }
}
