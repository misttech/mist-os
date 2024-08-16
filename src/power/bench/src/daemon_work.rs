// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! common functions to be used by Criterion or integration test for the
/// Topology Test Daemon.
use anyhow::Result;
use fidl::endpoints::create_sync_proxy;
use fuchsia_component::client::connect_to_protocol_sync;
use {
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_topology_test as fpt,
    fuchsia_zircon as zx,
};

use std::sync::Arc;

#[inline(always)]
fn black_box<T>(placeholder: T) -> T {
    criterion::black_box(placeholder)
}

fn work_func(
    topology_control: &fpt::TopologyControlSynchronousProxy,
    status_channel: &Arc<fbroker::StatusSynchronousProxy>,
) -> Result<()> {
    // Acquire lease for C @ 5.

    let _ = topology_control.acquire_lease("C", 5, zx::Time::INFINITE).unwrap();
    let level = status_channel
        .watch_power_level(zx::Time::INFINITE)
        .expect("Fidl call should work")
        .expect("Result should be good");
    assert_eq!(level, 5);

    let _ = topology_control.drop_lease("C", zx::Time::INFINITE).unwrap();
    let level = status_channel
        .watch_power_level(zx::Time::INFINITE)
        .expect("Fidl call should work")
        .expect("Result should be good");
    assert_eq!(level, 0);

    Ok(())
}

pub(crate) fn prepare_work(
) -> (Arc<fpt::TopologyControlSynchronousProxy>, Arc<fbroker::StatusSynchronousProxy>) {
    // Current Criterion library doesn't support async call yet.
    let topology_control = connect_to_protocol_sync::<fpt::TopologyControlMarker>().unwrap();

    let elements: [fpt::Element; 2] = [
        fpt::Element {
            element_name: "C".to_string(),
            initial_current_level: 0,
            valid_levels: vec![0, 5],
            dependencies: vec![fpt::LevelDependency {
                dependency_type: fpt::DependencyType::Assertive,
                dependent_level: 5,
                requires_element: "P".to_string(),
                requires_level: 50,
            }],
        },
        fpt::Element {
            element_name: "P".to_string(),
            initial_current_level: 0,
            valid_levels: vec![0, 30, 50],
            dependencies: vec![],
        },
    ];
    let _ = topology_control.create(&elements, zx::Time::INFINITE).unwrap();
    let (status_channel, server_channel) = create_sync_proxy::<fbroker::StatusMarker>();
    let _ = topology_control.open_status_channel("C", server_channel, zx::Time::INFINITE);

    let level = status_channel
        .watch_power_level(zx::Time::INFINITE)
        .expect("Fidl call should work")
        .expect("Result should be good");
    assert_eq!(level, 0);

    (Arc::new(topology_control), Arc::new(status_channel))
}

pub(crate) fn execute(
    topology_control: &fpt::TopologyControlSynchronousProxy,
    status_channel: &Arc<fbroker::StatusSynchronousProxy>,
) {
    let _ = black_box(work_func(topology_control, status_channel));
}
