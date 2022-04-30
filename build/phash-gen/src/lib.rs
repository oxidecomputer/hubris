// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{collections::HashSet, hash::Hash};

use anyhow::{bail, Result};
use rand::prelude::*;
use rand_chacha::ChaCha20Rng;

use phash::Reduce;

////////////////////////////////////////////////////////////////////////////////

pub enum PerfectHash<T> {
    Flat(FlatPerfectHash<T>),
    Nested(NestedPerfectHash<T>),
}

impl<T: std::fmt::Debug> PerfectHash<T> {
    pub fn codegen_with<F: Fn(usize) -> String>(
        &self,
        ty: &str,
        gen: F,
        invalid: &str,
    ) -> (String, String) {
        match self {
            Self::Flat(f) => (
                format!(
                    "FlatPerfectHash::<{}, {}>",
                    std::any::type_name::<T>(),
                    ty
                ),
                f.codegen_with(gen, invalid),
            ),
            Self::Nested(f) => (
                format!(
                    "NestedPerfectHash::<{}, {}>",
                    std::any::type_name::<T>(),
                    ty
                ),
                f.codegen_with(gen),
            ),
        }
    }
    pub fn codegen(&self) -> (String, String) {
        self.codegen_with("u32", |i| format!("{}", i), "usize::MAX")
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct FlatPerfectHash<T> {
    m: T,
    values: Vec<Option<usize>>,
}

impl<T: std::fmt::Debug> FlatPerfectHash<T> {
    fn codegen_with<F: Fn(usize) -> String>(
        &self,
        gen: F,
        invalid: &str,
    ) -> String {
        let mut out = format!(
            "FlatPerfectHash {{
    m: {:?},
    values: &[",
            self.m
        );
        for v in &self.values {
            out += &format!(
                "\n        {},",
                match v {
                    Some(v) => gen(*v),
                    None => invalid.to_string(),
                }
            );
        }
        out += "\n    ],\n}";
        out
    }
}

////////////////////////////////////////////////////////////////////////////////

/// The simplest perfect hash is (A * B) % C
/// This only works for very small sets of data
impl<T> FlatPerfectHash<T>
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
        Some(FlatPerfectHash { m, values: indexes })
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Perfect hash with one level of indirection
pub struct NestedPerfectHash<T> {
    m: T,
    g: Vec<T>,
    values: Vec<Vec<usize>>,
}

impl<T: std::fmt::Debug> NestedPerfectHash<T> {
    fn codegen_with<F: Fn(usize) -> String>(&self, gen: F) -> String {
        let mut out = format!(
            "NestedPerfectHash {{
    m: {:?},
    g: &{:?},
    values: &[
",
            self.m, self.g
        );
        for v in &self.values {
            out += "        &[";
            for w in v {
                out += &format!("\n            {},", gen(*w));
            }
            out += "\n        ],\n";
        }
        out += "    ],\n}";
        out
    }
}

impl<T> NestedPerfectHash<T>
where
    T: Copy + Clone + Reduce,
    rand::distributions::Standard: Distribution<T>,
{
    fn try_gen<R: rand::Rng>(
        values: &[T],
        n: usize,
        rng: &mut R,
    ) -> Option<Self> {
        // First stage: reduce to `n` slots
        let m: T = rng.gen::<T>();
        if n != values
            .iter()
            .map(|v| (v.reduce(m) as usize) % n)
            .collect::<HashSet<usize>>()
            .len()
        {
            return None;
        }

        // Second stage: reduce the subset of items in each slot
        let g = (0..n).map(|_| rng.gen::<T>()).collect::<Vec<_>>();
        let mut out = vec![];
        for (i, g) in g.iter().enumerate() {
            let vs = values
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, v)| (v.reduce(m) as usize) % n == i)
                .collect::<Vec<(usize, T)>>();

            let mut indexes = vec![0; vs.len()];
            if vs.len()
                != vs
                    .iter()
                    .map(|(j, v)| {
                        let index = v.reduce(*g) as usize % vs.len();
                        indexes[index] = *j;
                        index
                    })
                    .collect::<HashSet<_>>()
                    .len()
            {
                return None;
            }
            out.push(indexes);
        }
        Some(NestedPerfectHash { m, g, values: out })
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
    println!("Hashing {:?}", values);

    const TRY_COUNT: usize = 1_000_000;

    let mut rng = ChaCha20Rng::seed_from_u64(0x1de);
    for i in 0..TRY_COUNT {
        if let Some(out) =
            FlatPerfectHash::try_gen(values, values.len(), &mut rng)
        {
            println!("Got flat map in {} attempts", i);
            return Ok(PerfectHash::Flat(out));
        }
    }

    const PRIMES: [usize; 25] = [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67,
        71, 73, 79, 83, 89, 97,
    ];
    for &p in PRIMES.iter().filter(|p| **p >= values.len()).take(3) {
        for i in 0..TRY_COUNT {
            if let Some(out) = FlatPerfectHash::try_gen(values, p, &mut rng) {
                println!("Got flat map in {} attempts", i);
                return Ok(PerfectHash::Flat(out));
            }
        }
    }

    // Try to pick the minimum table size, but don't try _too_ hard
    for &p in PRIMES.iter().filter(|p| **p < values.len()) {
        for i in 0..TRY_COUNT {
            if let Some(out) = NestedPerfectHash::try_gen(values, p, &mut rng) {
                println!("Got nested map in {} attempts", i);
                return Ok(PerfectHash::Nested(out));
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
        println!("{}", generate_hash(&irqs).unwrap().codegen().1);

        let irqs = [36, 51, 85, 61, 31, 32, 33, 34, 72, 73, 95, 96];
        println!("{}", generate_hash(&irqs).unwrap().codegen().1);

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
        println!("{}", generate_hash(&tuples).unwrap().codegen().1);
        println!("{}", generate_hash(&tuples).unwrap().codegen().1);

        let tuples = [5, 7];
        println!("{}", generate_hash(&tuples).unwrap().codegen().1);
    }
}
