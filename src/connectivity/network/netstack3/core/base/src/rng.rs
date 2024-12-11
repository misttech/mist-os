// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Base types and traits for generating random numbers in netstack.
//!
//! NOTE:
//! - Code in netstack3 is required to only obtain random values through an
//!   [`RngContext`]. This allows a deterministic RNG to be provided when useful
//!   (for example, in tests).
//! - The CSPRNG requirement exists so that random values produced within the
//!   network stack are not predictable by outside observers. This helps prevent
//!   certain kinds of fingerprinting and denial of service attacks.

use rand::{CryptoRng, RngCore};

/// A context that provides a random number generator (RNG).
pub trait RngContext {
    // NB: If the CSPRNG requirement becomes a performance problem,
    // introduce a second, non-cryptographically secure, RNG.

    /// The random number generator (RNG) provided by this `RngContext`.
    ///
    /// The provided RNG must be cryptographically secure, and users may rely on
    /// that property for their correctness and security.
    type Rng<'a>: RngCore + CryptoRng
    where
        Self: 'a;

    /// Gets the random number generator (RNG).
    fn rng(&mut self) -> Self::Rng<'_>;
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::sync::Arc;

    use rand::{CryptoRng, Rng as _, RngCore, SeedableRng};
    use rand_xorshift::XorShiftRng;

    use crate::sync::Mutex;
    use crate::RngContext;

    /// A wrapper which implements `RngCore` and `CryptoRng` for any `RngCore`.
    ///
    /// This is used to satisfy [`RngContext`]'s requirement that the
    /// associated `Rng` type implements `CryptoRng`.
    ///
    /// # Security
    ///
    /// This is obviously insecure. Don't use it except in testing!
    #[derive(Clone, Debug)]
    pub struct FakeCryptoRng<R = XorShiftRng>(Arc<Mutex<R>>);

    impl Default for FakeCryptoRng<XorShiftRng> {
        fn default() -> FakeCryptoRng<XorShiftRng> {
            FakeCryptoRng::new_xorshift(12957992561116578403)
        }
    }

    impl FakeCryptoRng<XorShiftRng> {
        /// Creates a new [`FakeCryptoRng<XorShiftRng>`] from a seed.
        pub fn new_xorshift(seed: u128) -> FakeCryptoRng<XorShiftRng> {
            Self(Arc::new(Mutex::new(new_rng(seed))))
        }

        /// Returns a deep clone of this RNG, copying the current RNG state.
        pub fn deep_clone(&self) -> Self {
            Self(Arc::new(Mutex::new(self.0.lock().clone())))
        }

        /// Creates `iterations` fake RNGs.
        ///
        /// `with_fake_rngs` will create `iterations` different
        /// [`FakeCryptoRng`]s and call the function `f` for each one of them.
        ///
        /// This function can be used for tests that weed out weirdness that can
        /// happen with certain random number sequences.
        pub fn with_fake_rngs<F: Fn(Self)>(iterations: u128, f: F) {
            for seed in 0..iterations {
                f(FakeCryptoRng::new_xorshift(seed))
            }
        }
    }

    impl<R: RngCore> RngCore for FakeCryptoRng<R> {
        fn next_u32(&mut self) -> u32 {
            self.0.lock().next_u32()
        }
        fn next_u64(&mut self) -> u64 {
            self.0.lock().next_u64()
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.0.lock().fill_bytes(dest)
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.0.lock().try_fill_bytes(dest)
        }
    }

    impl<R: RngCore> CryptoRng for FakeCryptoRng<R> {}

    impl<R: SeedableRng> SeedableRng for FakeCryptoRng<R> {
        type Seed = R::Seed;

        fn from_seed(seed: Self::Seed) -> Self {
            Self(Arc::new(Mutex::new(R::from_seed(seed))))
        }
    }

    impl<R: RngCore> RngContext for FakeCryptoRng<R> {
        type Rng<'a>
            = &'a mut Self
        where
            Self: 'a;

        fn rng(&mut self) -> Self::Rng<'_> {
            self
        }
    }

    /// Create a new deterministic RNG from a seed.
    pub fn new_rng(mut seed: u128) -> XorShiftRng {
        if seed == 0 {
            // XorShiftRng can't take 0 seeds
            seed = 1;
        }
        XorShiftRng::from_seed(seed.to_ne_bytes())
    }

    /// Invokes a function multiple times with different RNG seeds.
    pub fn run_with_many_seeds<F: FnMut(u128)>(mut f: F) {
        // Arbitrary seed.
        let mut rng = new_rng(0x0fe50fae6c37593d71944697f1245847);
        for _ in 0..64 {
            f(rng.gen());
        }
    }
}
