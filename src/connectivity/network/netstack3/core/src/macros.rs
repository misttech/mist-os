// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Macros used in Netstack3.

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
#[cfg(test)]
macro_rules! bench {
    ($name:ident, $fn:expr) => {
        #[cfg(benchmark)]
        fn $name(b: &mut criterion::Bencher) {
            $fn(b);
        }

        #[cfg(not(benchmark))]
        #[test]
        fn $name() {
            $fn(&mut crate::testutil::benchmarks::TestBencher);
        }
    };
}
