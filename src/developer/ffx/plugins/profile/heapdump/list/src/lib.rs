// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    async_trait::async_trait,
    errors::ffx_error,
    ffx_profile_heapdump_common::{
        build_process_selector, check_collector_error, connect_to_collector,
    },
    ffx_profile_heapdump_list_args::ListCommand,
    ffx_writer::ToolIO,
    fho::{AvailabilityFlag, FfxMain, FfxTool, MachineWriter},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fidl_fuchsia_memory_heapdump_client as fheapdump_client,
    prettytable::{cell, row, Table},
    serde::Serialize,
};

/// Representation of the [fheapdump_client::StoredSnapshot] FIDL type for machine output.
#[derive(Serialize)]
pub struct StoredSnapshot {
    snapshot_id: u32,
    snapshot_name: String,
    process_koid: u64,
    process_name: String,
}

async fn receive_list_of_stored_snapshots(
    iterator: fheapdump_client::StoredSnapshotIteratorProxy,
) -> Result<Vec<StoredSnapshot>> {
    let mut result = Vec::new();
    loop {
        let batch = iterator.get_next().await?;
        if batch.is_empty() {
            break;
        }

        result.reserve(batch.len());
        for elem in batch {
            result.push(StoredSnapshot {
                snapshot_id: elem.snapshot_id.context("missing snapshot_id")?,
                snapshot_name: elem.snapshot_name.context("missing snapshot_name")?,
                process_koid: elem.process_koid.context("missing process_koid")?,
                process_name: elem.process_name.context("missing process_name")?,
            });
        }
    }

    result.sort_by_key(|snapshot| snapshot.snapshot_id);
    return Ok(result);
}

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_heapdump"))]
pub struct ListTool {
    #[command]
    cmd: ListCommand,
    remote_control: RemoteControlProxy,
}

fho::embedded_plugin!(ListTool);

#[async_trait(?Send)]
impl FfxMain for ListTool {
    type Writer = MachineWriter<Vec<StoredSnapshot>>;

    /// Forwards the specified memory pressure level to the fuchsia.memory.Debugger FIDL interface.
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let ListTool { cmd, remote_control } = self;
        let (iterator_proxy, iterator_server) = create_proxy().unwrap();
        let request = fheapdump_client::CollectorListStoredSnapshotsRequest {
            iterator: Some(iterator_server),
            process_selector: match (cmd.by_name, cmd.by_koid) {
                (None, None) => None,
                (by_name, by_koid) => Some(build_process_selector(by_name, by_koid)?),
            },
            ..Default::default()
        };

        let collector = connect_to_collector(&remote_control, cmd.collector).await?;
        check_collector_error(
            collector
                .list_stored_snapshots(request)
                .await
                .map_err(|err| ffx_error!("Failed to list stored snapshots: {err}"))?,
        )?;

        let stored_snapshots = receive_list_of_stored_snapshots(iterator_proxy).await?;
        if writer.is_machine() {
            writer.machine(&stored_snapshots)?;
        } else {
            let mut table = Table::new();
            table.set_titles(row!["ID", "NAME", "PROCESS"]);

            for elem in stored_snapshots {
                table.add_row(row![
                    format!("{}", elem.snapshot_id),
                    elem.snapshot_name,
                    format!("{}[{}]", elem.process_name, elem.process_koid),
                ]);
            }

            table.print(&mut writer).map_err(|err| ffx_error!("Failed to print table: {err}"))?;
        }

        Ok(())
    }
}
