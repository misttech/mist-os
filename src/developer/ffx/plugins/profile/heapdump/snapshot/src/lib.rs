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
    std::io::Write,
};

#[ffx_plugin(
    "ffx_profile_heapdump",
    fheapdump_client::CollectorProxy = "core/heapdump-collector:expose:fuchsia.memory.heapdump.client.Collector"
)]
pub async fn snapshot(
    collector: fheapdump_client::CollectorProxy,
    cmd: SnapshotCommand,
    mut writer: Writer,
) -> Result<()> {
    let mut process_selector = build_process_selector(cmd.by_name, cmd.by_koid)?;

    let result = collector.take_live_snapshot(&mut process_selector).await?;
    let receiver_stream = check_collector_error(result)?.into_stream()?;

    // TODO(fxbug.dev/123442): Convert the received snapshot into a pprof file.
    let snapshot = heapdump_snapshot::Snapshot::receive_from(receiver_stream).await?;
    writeln!(writer, "{:#x?}", snapshot)?;

    Ok(())
}
