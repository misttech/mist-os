// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::lsm_tree::types::FuzzyHash;
use anyhow::{Error, Result};
use bit_vec::BitVec;
use rustc_hash::FxHasher;
use std::hash::{Hash as _, Hasher as _};
use std::marker::PhantomData;

/// A bloom filter provides a probabilistic means of determining if a layer file *might* contain
/// records related to a given key.
///
/// A bloom filter is a bitmap with two associated constants `m` and `k`.  The bitmap is `m` bits
/// wide, and there are `k` associated hash functions with the filter.
///
/// There are two operations for a bloom filter:
/// - Adding a new entry.  Each of the `k` hash functions is computed for the entry, and this hash
///   corresponds to a single bit in the bitmap.  All `k` of these bits are set to 1.
/// - Querying for an entry.  Each of the `k` hash functions is computed for the entry, and if any
///   of the corresponding bits in the bitmap are set to 0, the bloom filter returns false, which
///   means the layer file definitely does not contain records for that key.  Note that false
///   positives are possible by design; all bits may have been set by unrelated entries, in which
///   case the query returns true.
///
/// # Efficiency & Tuning
///
/// As mentioned above, there are two parameters `k` and `m` which we can tune to optimize the
/// BloomFilter.  There are three factors to consider:
/// - How much space we use for the bloom filter (i.e. `m`).
/// - How efficient the operations are (i.e. `k`, since we have to compute `k` hashes for each
///   insert/query).
/// - Minimizing false positive rate.  Increasing `k` or `m` will both reduce the false positive
///   rate.
///
/// For simplicity we fix a target false-positive rate and derive `k` and `m`.  See estimate_params.

// To avoid the need for storing the nonces, we generate them pseudo-randomly.
// Changing the generation of these values will require a version update (which is guarded against
// in a test below).
fn generate_hash_nonces(seed: u64, num_nonces: usize) -> Vec<u64> {
    use rand::rngs::SmallRng;
    use rand::{Rng as _, SeedableRng as _};
    let mut output = Vec::with_capacity(num_nonces);
    let mut rng = SmallRng::seed_from_u64(seed);
    for _ in 0..num_nonces {
        let nonce: u64 = rng.gen();
        output.push(nonce);
    }
    output
}

// Returns a tuple (bloom_filter_size_bits, num_hashes).  The bloom filter will always be sized up
// to the nearest power-of-two, which makes its usage later a bit more efficient (since we take
// indexes modulo the size of the filter).
// We target a false positive rate of 0.1%, from which we can derive the constants via relationships
// described in https://en.wikipedia.org/wiki/Bloom_filter#Optimal_number_of_hash_functions.
fn estimate_params(num_items: usize) -> (usize, usize) {
    if num_items == 0 {
        return (0, 0);
    }
    const TARGET_FP_RATE: f64 = 0.001;
    use std::f64::consts::LN_2;

    let n = num_items as f64;
    let bits = ((n * TARGET_FP_RATE.ln()) / (-LN_2 * LN_2)).ceil() as u64;
    // Round up to a power-of-two number of bytes.
    let bits = std::cmp::max(bits, 8).next_power_of_two();
    let m = bits as f64;

    // Then, solve for a minimal `k` such that the probability of FP is <= TARGET_FP_RATE.
    let mut k: f64 = 0.0;
    let mut fp_rate = 1.0;
    while fp_rate > TARGET_FP_RATE {
        k += 1.0;
        fp_rate = (1f64 - (-k / (m / n)).exp()).powf(k);
    }
    (bits as usize, k.round() as usize)
}

/// A read-only handle to a bloom filter.  To create a bloom filter, use `BloomFilterWriter`.
pub struct BloomFilterReader<V> {
    data: BitVec,
    hash_nonces: Vec<u64>,
    _type: PhantomData<V>,
}

pub struct BloomFilterStats {
    pub size: usize,
    pub num_nonces: usize,
    // The percentage (rounded up to the nearest whole) of the bits which are set.
    pub fill_percentage: usize,
}

impl<V: FuzzyHash> BloomFilterReader<V> {
    /// Creates a BloomFilterReader by reading the serialized contents from `buf`.
    /// `seed` and `num_nonces` must match the values passed into BloomFilterWriter.
    pub fn read(buf: &[u8], seed: u64, num_nonces: usize) -> Result<Self, Error> {
        Ok(Self {
            data: BitVec::from_bytes(buf),
            hash_nonces: generate_hash_nonces(seed, num_nonces),
            _type: PhantomData::default(),
        })
    }

    /// Returns whether the bloom filter *might* contain the given value (or any part of it, for
    /// range-based keys.
    pub fn maybe_contains(&self, value: &V) -> bool {
        let mut num = 0;
        for hash in value.fuzzy_hash() {
            if self.maybe_contains_inner(hash) {
                return true;
            }
            num += 1;
            debug_assert!(num < 4, "Too many hash partitions");
        }
        false
    }

    fn maybe_contains_inner(&self, initial_hash: u64) -> bool {
        let mut hasher = FxHasher::default();
        initial_hash.hash(&mut hasher);
        for nonce in &self.hash_nonces {
            hasher.write_u64(*nonce);
            let idx = hasher.finish() as usize % self.data.len();
            if !self.data.get(idx).unwrap() {
                return false;
            }
        }
        true
    }

    /// Call sparingly; this is expensive to compute.
    /// Note that the return value can be trivially cached since the reader is immutable.
    pub fn stats(&self) -> BloomFilterStats {
        BloomFilterStats {
            size: self.data.len().div_ceil(8),
            num_nonces: self.hash_nonces.len(),
            fill_percentage: self.compute_fill_percentage(),
        }
    }

    // Use sparingly; this is expensive to compute.
    fn compute_fill_percentage(&self) -> usize {
        if self.data.is_empty() {
            return 0;
        }
        (100 * self.data.iter().filter(|x| *x).count()).div_ceil(self.data.len())
    }

    #[cfg(test)]
    fn new_empty(num_items: usize) -> Self {
        let (bits, num_nonces) = estimate_params(num_items);
        Self {
            data: BitVec::from_elem(bits, false),
            hash_nonces: generate_hash_nonces(0, num_nonces),
            _type: PhantomData::default(),
        }
    }

    #[cfg(test)]
    fn new_full(num_items: usize) -> Self {
        let (bits, num_nonces) = estimate_params(num_items);
        Self {
            data: BitVec::from_elem(bits, true),
            hash_nonces: generate_hash_nonces(0, num_nonces),
            _type: PhantomData::default(),
        }
    }
}

pub struct BloomFilterWriter<V> {
    data: BitVec,
    seed: u64,
    hash_nonces: Vec<u64>,
    _type: PhantomData<V>,
}

impl<V: FuzzyHash> BloomFilterWriter<V> {
    /// Creates a new bloom filter suitable for the given input size.  See module comments for the
    /// heuristics used.
    /// `seed` is a value which is mixed into hashes in the bloom filter.  Note that it is not used
    /// in a secure manner so should not contain any secrets.  It should be unpredictable to prevent
    /// timing attacks.
    pub fn new(seed: u64, num_items: usize) -> Self {
        let (bits, num_nonces) = estimate_params(num_items);
        Self {
            data: BitVec::from_elem(bits, false),
            seed,
            hash_nonces: generate_hash_nonces(seed, num_nonces),
            _type: PhantomData::default(),
        }
    }

    /// Returns the size the bloom filter will occupy when serialized.
    pub fn serialized_size(&self) -> usize {
        self.data.len() / 8
    }

    pub fn num_nonces(&self) -> usize {
        self.hash_nonces.len()
    }

    pub fn write<W>(&self, writer: &mut W) -> Result<(), Error>
    where
        W: std::io::Write,
    {
        Ok(writer.write_all(&self.data.to_bytes()[..])?)
    }

    pub fn insert(&mut self, value: &V) {
        for hash in value.fuzzy_hash() {
            self.insert_inner(hash);
        }
    }

    fn insert_inner(&mut self, initial_hash: u64) {
        let mut hasher = FxHasher::default();
        initial_hash.hash(&mut hasher);
        for nonce in &self.hash_nonces {
            hasher.write_u64(*nonce);
            let idx = hasher.finish() as usize % self.data.len();
            self.data.set(idx, true);
        }
    }
}

#[cfg(test)]
impl<T> From<BloomFilterWriter<T>> for BloomFilterReader<T> {
    fn from(writer: BloomFilterWriter<T>) -> Self {
        Self { data: writer.data, hash_nonces: writer.hash_nonces, _type: PhantomData::default() }
    }
}

#[cfg(test)]
mod tests {
    use crate::lsm_tree::bloom_filter::{
        estimate_params, generate_hash_nonces, BloomFilterReader, BloomFilterWriter,
    };
    use crate::object_store::allocator::AllocatorKey;

    #[test]
    fn estimated_params() {
        // Compare to https://hur.st/bloomfilter (keeping in mind the rounding-up of the size of the
        // bloom filter).
        assert_eq!(estimate_params(0), (0, 0));
        assert_eq!(estimate_params(1), (16, 6));
        assert_eq!(estimate_params(5000), (131072, 4));
        assert_eq!(estimate_params(50000), (1048576, 4));
        assert_eq!(estimate_params(5_000_000), (134217728, 4));
    }

    #[test]
    fn hash_nonces_are_stable() {
        let expected: [u64; 16] = [
            15601892068231251798,
            4249550343631144082,
            11170505403239506035,
            3141899402926357684,
            10455158105512296971,
            1544954577306875038,
            7546141422416683882,
            10809374736430664972,
            14270886949990153586,
            12890306703619732195,
            10531031577317334640,
            2369458071968706433,
            8554654157920588268,
            4945022529079265586,
            4849687277068177250,
            11824207122193630909,
        ];
        for i in 1..=expected.len() {
            let generated = generate_hash_nonces(0xfeedbad, i);
            assert_eq!(&generated[..], &expected[..i]);
        }
    }

    const TEST_KEYS: [i32; 4] = [0, 65535, i32::MAX, i32::MIN];

    #[test]
    fn test_empty() {
        let filter = BloomFilterReader::new_empty(TEST_KEYS.len());
        for key in &TEST_KEYS {
            assert!(!filter.maybe_contains(key));
        }
    }

    #[test]
    fn test_full() {
        let filter = BloomFilterReader::new_full(TEST_KEYS.len());
        for key in &TEST_KEYS {
            assert!(filter.maybe_contains(key));
        }
    }

    #[test]
    fn test_insert() {
        for key in &TEST_KEYS {
            // Use a new filter each time so we don't get false positives.
            let mut filter = BloomFilterWriter::new(0, TEST_KEYS.len());
            filter.insert(key);
            let filter = BloomFilterReader::from(filter);
            assert!(filter.maybe_contains(key));
        }
    }

    #[test]
    fn test_range_key() {
        let mut filter = BloomFilterWriter::new(0, 2);
        filter.insert(&AllocatorKey { device_range: 0..2097152 });
        filter.insert(&AllocatorKey { device_range: 4194304..4194305 });
        let filter = BloomFilterReader::from(filter);

        assert!(filter.maybe_contains(&AllocatorKey { device_range: 0..1 }));
        assert!(filter.maybe_contains(&AllocatorKey { device_range: 2097151..2097152 }));
        assert!(!filter.maybe_contains(&AllocatorKey { device_range: 2097152..2097153 }));
        assert!(!filter.maybe_contains(&AllocatorKey { device_range: 3145727..3145728 }));
        assert!(filter.maybe_contains(&AllocatorKey { device_range: 4193404..4194305 }));

        assert!(filter.maybe_contains(&AllocatorKey { device_range: 0..2097153 }));
        assert!(filter.maybe_contains(&AllocatorKey { device_range: 2097152..4194305 }));
        assert!(!filter.maybe_contains(&AllocatorKey { device_range: 104857600..104857601 }));
    }

    #[test]
    fn test_serde() {
        for key in &TEST_KEYS {
            // Use a new filter each time so we don't get false positives.
            let mut filter = BloomFilterWriter::new(0, 1);
            filter.insert(key);
            let num_nonces = filter.num_nonces();
            let mut buf = vec![];
            {
                let mut cursor = std::io::Cursor::new(&mut buf);
                filter.write(&mut cursor).expect("write failed");
            }
            let filter = BloomFilterReader::read(&buf[..], 0, num_nonces).expect("read failed");
            assert!(filter.maybe_contains(key));
        }
    }
}
