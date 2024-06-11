// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/339502691): Return to the default limit once lock
// ordering no longer causes overflows.
#![recursion_limit = "256"]

//! A benchmark runner for Netstack3, based on Criterion.
//!
//! Submodules contain integration benchmarks for netstack3, and the main
//! function aggregates non-integration benchmarks split across the
//! netstack3-crates.

mod forwarding;

use fuchsia_criterion::criterion::Criterion;
use fuchsia_criterion::FuchsiaCriterion;

fn main() {
    let benches = forwarding::get_benchmark();
    let benches = netstack3_base::benchmarks::add_benches(benches);

    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(std::time::Duration::from_millis(1))
        .measurement_time(std::time::Duration::from_millis(100))
        .sample_size(100);
    let _: &mut Criterion = c.bench("fuchsia.netstack3.core", benches);
}
