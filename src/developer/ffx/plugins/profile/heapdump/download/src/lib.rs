// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_core::ffx_plugin,
    ffx_profile_heapdump_common::{check_collector_error, connect_to_collector, export_to_pprof},
    ffx_profile_heapdump_download_args::DownloadCommand,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
};

#[ffx_plugin("ffx_profile_heapdump")]
pub async fn download(
    remote_control: RemoteControlProxy,
    cmd: DownloadCommand,
    _writer: Writer,
) -> Result<()> {
    let collector = connect_to_collector(&remote_control, cmd.collector).await?;
    let result = collector.download_stored_snapshot(cmd.snapshot_id).await?;
    let receiver_stream = check_collector_error(result)?.into_stream()?;

    let snapshot = heapdump_snapshot::Snapshot::receive_from(receiver_stream).await?;
    export_to_pprof(&snapshot, &mut std::fs::File::create(cmd.output_file)?)?;

    Ok(())
}
