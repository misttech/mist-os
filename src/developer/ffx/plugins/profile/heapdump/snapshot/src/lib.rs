// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    errors::ffx_bail,
    ffx_core::ffx_plugin,
    ffx_profile_heapdump_common::{
        build_process_selector, check_collector_error, connect_to_collector, export_to_pprof,
    },
    ffx_profile_heapdump_snapshot_args::SnapshotCommand,
    ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fidl_fuchsia_memory_heapdump_client as fheapdump_client,
    std::io::Write,
};

#[ffx_plugin("ffx_profile_heapdump")]
pub async fn snapshot(
    remote_control: RemoteControlProxy,
    cmd: SnapshotCommand,
    _writer: Writer,
) -> Result<()> {
    let process_selector = build_process_selector(cmd.by_name, cmd.by_koid)?;
    let contents_dir = cmd.output_contents_dir.as_ref().map(std::path::Path::new);
    let request = fheapdump_client::CollectorTakeLiveSnapshotRequest {
        process_selector: Some(process_selector),
        with_contents: Some(contents_dir.is_some()),
        ..Default::default()
    };

    let collector = connect_to_collector(&remote_control, cmd.collector).await?;
    let result = collector.take_live_snapshot(&request).await?;
    let receiver_stream = check_collector_error(result)?.into_stream()?;

    let snapshot = heapdump_snapshot::Snapshot::receive_from(receiver_stream).await?;

    // If the user has requested the blocks' contents, ensure that `contents_dir` is an empty
    // directory (creating it if necessary), then dump the contents of each allocated block to a
    // different file.
    if let Some(contents_dir) = contents_dir {
        if let Ok(mut iterator) = std::fs::read_dir(contents_dir) {
            // While not strictly necessary, requiring that the target directory is empty makes it
            // much harder to accidentally flood important directories.
            if iterator.next().is_some() {
                ffx_bail!("Output directory is not empty: {}", contents_dir.display());
            }
        } else {
            if let Err(err) = std::fs::create_dir(contents_dir) {
                ffx_bail!("Failed to create output directory: {}: {}", contents_dir.display(), err);
            }
        }

        for (address, info) in &snapshot.allocations {
            if let Some(ref data) = info.contents {
                let filename = format!("0x{:x}", address);
                let mut file = std::fs::File::create(contents_dir.join(filename))?;
                file.write_all(&data)?;
            }
        }
    }

    export_to_pprof(&snapshot, &mut std::fs::File::create(cmd.output_file)?)?;
    Ok(())
}
