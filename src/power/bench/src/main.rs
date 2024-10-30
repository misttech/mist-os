// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A benchmark runner for SAG, based on Criterion.
//!
//! main.rs contains benchmarks for TakeWakeLease from SAG.

mod daemon_work;
mod sag_work;

use anyhow::Result;
use {
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_system as fsystem,
    fidl_fuchsia_power_topology_test as fpt,
};

use fuchsia_criterion::criterion::Criterion;
use fuchsia_criterion::FuchsiaCriterion;
use std::sync::Arc;

fn bench_take_wake_lease(
    b: &mut criterion::Bencher,
    sag: Arc<fsystem::ActivityGovernorSynchronousProxy>,
) {
    b.iter(|| {
        sag_work::execute(&sag);
    });
}

fn bench_toggle_lease(
    b: &mut criterion::Bencher,
    topology_control: Arc<fpt::TopologyControlSynchronousProxy>,
    status_channel: Arc<fbroker::StatusSynchronousProxy>,
) {
    b.iter(|| {
        daemon_work::execute(&topology_control, &status_channel);
    });
}

fn get_sag_benches() -> criterion::Benchmark {
    let sag_arc = sag_work::obtain_sag_proxy();
    criterion::Benchmark::new("TakeWakeLease", move |b| {
        bench_take_wake_lease(b, sag_arc.clone())
    })
}

fn get_daemon_benches() -> criterion::Benchmark {
    let (topology_control, status_channel) = daemon_work::prepare_work();
    criterion::Benchmark::new("ToggleLease", move |b| {
        bench_toggle_lease(b, topology_control.clone(), status_channel.clone())
    })
}

fn main() -> Result<()> {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(std::time::Duration::from_millis(10))
        .measurement_time(std::time::Duration::from_millis(100))
        .sample_size(100);
    let _: &mut Criterion = c.bench("fuchsia.power.framework", get_sag_benches());
    let _: &mut Criterion = c.bench("fuchsia.power.framework", get_daemon_benches());

    Ok(())
}
