// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    errors::ffx_error,
    ffx_profile_heapdump_common::{check_collector_error, connect_to_collector, export_to_pprof},
    ffx_profile_heapdump_download_args::DownloadCommand,
    fho::{AvailabilityFlag, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
};

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_heapdump"))]
pub struct DownloadTool {
    #[command]
    cmd: DownloadCommand,
    remote_control: RemoteControlProxy,
}

fho::embedded_plugin!(DownloadTool);

#[async_trait(?Send)]
impl FfxMain for DownloadTool {
    type Writer = SimpleWriter;

    /// Forwards the specified memory pressure level to the fuchsia.memory.Debugger FIDL interface.
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let DownloadTool { cmd, remote_control } = self;
        let collector = connect_to_collector(&remote_control, cmd.collector)
            .await
            .map_err(|err| ffx_error!("Failed to connect to collector: {err}"))?;
        let result = collector
            .download_stored_snapshot(cmd.snapshot_id)
            .await
            .map_err(|err| ffx_error!("Failed to download stored snapshot: {err}"))?;
        let receiver_stream = check_collector_error(result)?.into_stream().unwrap();

        let snapshot = heapdump_snapshot::Snapshot::receive_from(receiver_stream)
            .await
            .map_err(|err| ffx_error!("Failed to receive snapshot: {err}"))?;
        export_to_pprof(
            &snapshot,
            &mut std::fs::File::create(cmd.output_file)
                .map_err(|err| ffx_error!("Failed to create file: {err}"))?,
        )?;

        Ok(())
    }
}
