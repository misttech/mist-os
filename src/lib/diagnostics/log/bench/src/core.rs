// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_criterion::{criterion, FuchsiaCriterion};
use std::time::Duration;

fn main() {
    // TODO(fxbug.dev/124921): add benchmarks.
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        // We must reduce the sample size from the default of 100, otherwise
        // Criterion will sometimes override the 1ms + 500ms suggested times
        // and run for much longer.
        .sample_size(10);

    let bench = criterion::Benchmark::new("TODO", |_b| {});
    c.bench("fuchsia.diagnostics_log.benchmarks", bench);
}
