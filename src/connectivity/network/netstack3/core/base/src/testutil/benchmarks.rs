// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 benchmark utilities.

/// Declare a benchmark function.
///
/// The function will be named `$name`. If the `benchmark` cfg is enabled, the
/// provided `$fn` will be invoked with a `&mut criterion::Bencher` - in other
/// words, a real benchmark. If the `benchmark` cfg is disabled, the function
/// will be annotated with the `#[test]` attribute, and the provided `$fn` will
/// be invoked with a `&mut TestBencher`, which has the effect of creating a
/// test that runs the benchmarked function for a small, fixed number of
/// iterations. This test allows the use of debug assertions to verify the
/// correctness of the benchmark.
///
/// Note that `$fn` doesn't have to be a named function - it can also be an
/// anonymous closure.
#[macro_export]
macro_rules! bench {
    ($name:ident, $fn:expr) => {
        #[cfg(benchmark)]
        pub(crate) fn $name(b: &mut $crate::testutil::RealBencher) {
            $fn(b);
        }

        #[cfg(not(benchmark))]
        #[test]
        fn $name() {
            $fn(&mut $crate::testutil::TestBencher);
        }
    };
}

/// A trait to allow faking of the type providing benchmarking.
pub trait Bencher {
    /// Benchmarks `inner` by running it multiple times.
    fn iter<T, F: FnMut() -> T>(&mut self, inner: F);

    /// Abstracts blackboxing.
    ///
    /// `black_box` prevents the compiler from optimizing a function with an
    /// unused return type.
    fn black_box<T>(placeholder: T) -> T;
}

/// An alias for the bencher used in real benchmarks.
pub use criterion::Bencher as RealBencher;

impl Bencher for RealBencher {
    fn iter<T, F: FnMut() -> T>(&mut self, inner: F) {
        criterion::Bencher::iter(self, inner)
    }

    #[inline(always)]
    fn black_box<T>(placeholder: T) -> T {
        criterion::black_box(placeholder)
    }
}

/// A `Bencher` whose `iter` method runs the provided argument a small,
/// fixed number of times.
pub struct TestBencher;

impl Bencher for TestBencher {
    fn iter<T, F: FnMut() -> T>(&mut self, mut inner: F) {
        const NUM_TEST_ITERS: u32 = 3;
        for _ in 0..NUM_TEST_ITERS {
            let _: T = inner();
        }
    }

    #[inline(always)]
    fn black_box<T>(placeholder: T) -> T {
        placeholder
    }
}
