// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A benchmark runner for SAG, based on Criterion.
//!
//! main.rs contains benchmarks for TakeWakeLease from SAG.

mod work;

use anyhow::Result;
use fidl_fuchsia_power_system as fsystem;

use fuchsia_criterion::criterion::Criterion;
use fuchsia_criterion::FuchsiaCriterion;
use std::sync::Arc;

fn bench_take_wake_lease(
    b: &mut criterion::Bencher,
    sag: Arc<fsystem::ActivityGovernorSynchronousProxy>,
) {
    b.iter(|| {
        work::execute(&sag);
    });
}

fn main() -> Result<()> {
    let sag_arc = work::obtain_sag_proxy();
    let benches = criterion::Benchmark::new("TakeWakeLease", move |b| {
        bench_take_wake_lease(b, sag_arc.clone())
    });

    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(std::time::Duration::from_millis(1))
        .measurement_time(std::time::Duration::from_millis(100))
        .sample_size(100);
    let _: &mut Criterion = c.bench("fuchsia.power.sag", benches);
    Ok(())
}
