// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use component_events::events::{Event, EventStream, ExitStatus, Stopped};
use component_events::matcher::EventMatcher;
use diagnostics_reader::{ArchiveReader, Inspect, Logs};
use fidl_fuchsia_io::FileProxy;
use fuchsia_component_test::{RealmBuilder, RealmBuilderParams, RealmInstance};
use futures::StreamExt;
use log::info;
use std::collections::BTreeMap;

#[fuchsia::test]
async fn o_shutdown() {
    let mut events = EventStream::open().await.unwrap();
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new()
            .realm_name("o_shutdown")
            .from_relative_url("#meta/kernel_with_container.cm"),
    )
    .await
    .unwrap();

    info!("starting realm");
    let kernel_with_container = builder.build().await.unwrap();
    let realm_moniker = format!("realm_builder:{}", kernel_with_container.root.child_name());
    info!(realm_moniker:%; "started");
    let container_moniker = format!("{realm_moniker}/debian_container");
    let kernel_moniker = format!("{realm_moniker}/kernel");
    let test_fxfs_moniker = format!("test-fxfs");

    let mut kernel_logs = ArchiveReader::new()
        .select_all_for_moniker(&kernel_moniker)
        .snapshot_then_subscribe::<Logs>()
        .unwrap();

    // Open sysrq-trigger to start the kernel, then make sure we see its logs.
    let sysrq = open_sysrq_trigger(&kernel_with_container).await;
    let first_kernel_log = kernel_logs.next().await.unwrap().unwrap();
    info!(first_kernel_log:?; "receiving logs from starnix kernel now that it's started");

    info!("writing o to sysrq, ignoring result");
    let _ = fuchsia_fs::file::write(&sysrq, "o").await;

    info!("waiting for exit");
    assert_matches!(
        wait_for_exit_status(&mut events, [&container_moniker, &kernel_moniker]).await,
        [ExitStatus::Clean, ExitStatus::Clean]
    );

    info!("collecting test-fxfs inspect");
    let test_fxfs_inspect = ArchiveReader::new()
        .select_all_for_moniker(&test_fxfs_moniker)
        .snapshot::<Inspect>()
        .await
        .unwrap();
    assert_eq!(test_fxfs_inspect.len(), 1);
    let payload = test_fxfs_inspect[0].payload.as_ref().unwrap();
    let child = payload.get_child("starnix_volume").unwrap();
    info!("make sure the starnix volume was unmounted on shutdown");
    assert!(child.get_property("mounted").and_then(|p| p.boolean()) == Some(false));
}

async fn open_sysrq_trigger(realm: &RealmInstance) -> FileProxy {
    info!("opening sysrq trigger");
    fuchsia_fs::directory::open_file(
        realm.root.get_exposed_dir(),
        "/fs_root/proc/sysrq-trigger",
        fuchsia_fs::PERM_WRITABLE,
    )
    .await
    .unwrap()
}

async fn wait_for_exit_status<const N: usize>(
    events: &mut EventStream,
    monikers: [&str; N],
) -> [ExitStatus; N] {
    info!(monikers:% = monikers.join(","); "waiting for exit status");
    let mut statuses = BTreeMap::new();

    // Wait for all the provided monikers to stop.
    let mut num_stopped = 0;
    while num_stopped < N {
        let stopped = EventMatcher::ok().monikers(monikers).wait::<Stopped>(events).await.unwrap();
        let moniker = stopped.target_moniker().to_string();
        let status = stopped.result().unwrap().status;
        info!(moniker:%, status:?; "component stopped");
        statuses.insert(moniker, status);
        num_stopped += 1;
    }

    // Put the exit statuses in the order of monikers provided.
    let mut ret = [ExitStatus::Clean; N];
    for (i, moniker) in monikers.iter().enumerate() {
        ret[i] = statuses[*moniker];
    }
    ret
}
