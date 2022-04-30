// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{collections::HashSet, hash::Hash};

use anyhow::{bail, Result};
use rand::prelude::*;
use rand_chacha::ChaCha20Rng;

use phash::Reduce;

////////////////////////////////////////////////////////////////////////////////

pub struct PerfectHash<T> {
    m: T,
    values: Vec<Option<usize>>,
}

impl<T: std::fmt::Debug> PerfectHash<T> {
    pub fn codegen_with<F: Fn(Option<usize>) -> String>(
        &self,
        index_to_string: F,
    ) -> String {
        let mut out = format!(
            "PerfectHash {{
    m: {:?},
    values: &[",
            self.m
        );
        for &v in &self.values {
            out += &format!("\n        {},", index_to_string(v));
        }
        out += "\n    ],\n}";
        out
    }

    pub fn codegen(&self) -> String {
        self.codegen_with(|i| {
            i.map(|i| format!("{}", i))
                .unwrap_or_else(|| "u32::MAX".to_string())
        })
    }
}

/// The simplest perfect hash is (A * B) % C
/// This only works for very small sets of data
impl<T> PerfectHash<T>
where
    T: Copy + Clone + Reduce,
    rand::distributions::Standard: Distribution<T>,
{
    fn try_gen<R: rand::Rng>(
        values: &[T],
        n: usize,
        rng: &mut R,
    ) -> Option<Self> {
        assert!(n >= values.len());

        let m: T = rng.gen::<T>();

        let mut indexes = vec![None; n];
        values.iter().enumerate().for_each(|(j, v)| {
            let index = v.reduce(m) as usize % n;
            indexes[index] = Some(j);
        });

        if indexes.iter().filter(|p| p.is_some()).count() != values.len() {
            return None;
        }
        Some(PerfectHash { m, values: indexes })
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Attempt to generate a perfect hash for the given input data
pub fn generate_hash<T>(values: &[T]) -> Result<PerfectHash<T>>
where
    T: Copy + Clone + Reduce + std::fmt::Debug + Hash + Eq,
    rand::distributions::Standard: Distribution<T>,
{
    if values.iter().clone().collect::<HashSet<_>>().len() != values.len() {
        bail!("Cannot build a perfect hash with duplicate elements");
    }

    const TRY_COUNT: usize = 10_000;
    let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
    for p in values.len()..(2 * values.len()) {
        for _ in 0..TRY_COUNT {
            if let Some(out) = PerfectHash::try_gen(values, p, &mut rng) {
                return Ok(out);
            }
        }
    }

    bail!("Could not generate perfect hash");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn it_works() {
        let irqs = [36, 51, 13, 14];
        println!("{}", generate_hash(&irqs).unwrap().codegen());

        let irqs = [36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96];
        println!("{}", generate_hash(&irqs).unwrap().codegen());

        let tuples = [
            (2, 0b1),
            (3, 0b1),
            (4, 0b1),
            (5, 0b1),
            (9, 0b1),
            (9, 0b10),
            (9, 0b100),
            (9, 0b1000),
        ];
        println!("{}", generate_hash(&tuples).unwrap().codegen());
        println!("{}", generate_hash(&tuples).unwrap().codegen());

        let tuples = [5, 7];
        println!("{}", generate_hash(&tuples).unwrap().codegen());
    }
}
