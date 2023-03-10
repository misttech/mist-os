// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_core::ffx_plugin,
    ffx_heapdump::{build_process_selector, check_collector_error},
    ffx_heapdump_snapshot_args::SnapshotCommand,
    ffx_writer::Writer,
    fidl_fuchsia_memory_heapdump_client as fheapdump_client,
};

#[ffx_plugin(
    "ffx_profile_heapdump",
    fheapdump_client::CollectorProxy = "core/heapdump-collector:expose:fuchsia.memory.heapdump.client.Collector"
)]
pub async fn snapshot(
    collector: fheapdump_client::CollectorProxy,
    cmd: SnapshotCommand,
    _writer: Writer,
) -> Result<()> {
    let mut process_selector = build_process_selector(cmd.by_name, cmd.by_koid)?;

    let result = collector.take_live_snapshot(&mut process_selector).await?;
    let _receiver_stream = check_collector_error(result)?.into_stream()?;

    unimplemented!("Support for receiving snapshots has not been implemented yet");
}
