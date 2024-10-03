// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ahash::AHasher;
use flyweights::FlyStr;
use fuchsia_criterion::criterion::{self, black_box, BatchSize, Fun};
use fuchsia_criterion::FuchsiaCriterion;
use std::hash::{Hash, Hasher};
use std::time::Duration;

const INPUTS: &[&str; 6] = &[
    // Empty
    "",
    // Extra-small
    "1",
    // Largest inline
    "1234567",
    // Smallest heap
    "12345678",
    // Larger heap
    "123456789123456789123456789123456789123456789123456789123456789123456789123456789123456789",
    // Really big heap
    concat!(
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
        "123456789123456789123456789123456789123456789123456789123456789123456789123456789",
    ),
];

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        .sample_size(1000);

    for input in INPUTS {
        c.bench_functions(
            "fuchsia.flyweights",
            vec![
                // Lifecycle
                Fun::new(&format!("FromStrFirst/{}", input.len()), |b, input: &&str| {
                    let input = black_box(*input);
                    b.iter(|| FlyStr::from(input));
                }),
                Fun::new(&format!("FromStrDupe/{}", input.len()), |b, input: &&str| {
                    let input = black_box(*input);
                    // Make a copy of the FlyStr that will be live the whole time.
                    let _existing = FlyStr::from(input);
                    b.iter(|| FlyStr::from(input));
                }),
                Fun::new(&format!("Clone/{}", input.len()), |b, input: &&str| {
                    let first = black_box(FlyStr::from(*input));
                    b.iter(|| first.clone());
                }),
                Fun::new(&format!("DropDupe/{}", input.len()), |b, input: &&str| {
                    // Make a copy of the FlyStr that will be live the whole time so we drop a copy.
                    let input = black_box(FlyStr::from(*input));
                    b.iter_batched(|| input.clone(), |input| drop(input), BatchSize::PerIteration);
                }),
                Fun::new(&format!("DropLast/{}", input.len()), |b, input: &&str| {
                    let input = black_box(*input);
                    b.iter_batched(
                        || FlyStr::from(input),
                        |input| drop(input),
                        BatchSize::PerIteration,
                    );
                }),
                // Access
                Fun::new(&format!("AsStr/{}", input.len()), |b, input: &&str| {
                    let input = black_box(FlyStr::from(*input));
                    b.iter(|| input.as_str());
                }),
                // Hashing
                Fun::new(&format!("AHash/{}", input.len()), |b, input: &&str| {
                    let input = black_box(FlyStr::from(*input));
                    b.iter_batched_ref(
                        || AHasher::default(),
                        |hasher| {
                            input.hash(hasher);
                            hasher.finish()
                        },
                        BatchSize::PerIteration,
                    );
                }),
                // Comparison
                Fun::new(&format!("Eq/{}", input.len()), |b, input: &&str| {
                    let first = black_box(FlyStr::from(*input));
                    let second = black_box(first.clone());
                    b.iter(|| first == second);
                }),
            ],
            *input,
        );
    }
}
