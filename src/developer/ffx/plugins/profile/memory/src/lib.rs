// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library that obtains and prints memory digests of a running fuchsia device.

mod bucket;
mod digest;
mod plugin_output;
mod write_csv_output;
mod write_human_readable_output;

use crate::plugin_output::filter_digest_by_process;
use crate::write_csv_output::write_csv_output;
use crate::write_human_readable_output::write_human_readable_output;
use anyhow::Result;
use async_trait::async_trait;
use digest::{processed, raw};
use ffx_profile_memory_args::MemoryCommand;
use ffx_writer::{MachineWriter, ToolIO};
use fho::{FfxMain, FfxTool};
use fidl_fuchsia_memory_inspection::CollectorProxy;
use futures::AsyncReadExt;
use plugin_output::ProfileMemoryOutput;
use std::io::Write;
use std::time::Duration;
use target_holders::moniker;

#[derive(FfxTool)]
pub struct MemoryTool {
    #[command]
    cmd: MemoryCommand,
    #[with(moniker("/core/memory_monitor"))]
    collector_proxy: CollectorProxy,
}

fho::embedded_plugin!(MemoryTool);

#[async_trait(?Send)]
impl FfxMain for MemoryTool {
    type Writer = MachineWriter<ProfileMemoryOutput>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        plugin_entrypoint(&self.collector_proxy, self.cmd, writer).await?;
        Ok(())
    }
}

/// Prints a memory digest to stdout.
pub async fn plugin_entrypoint(
    collector: &CollectorProxy,
    cmd: MemoryCommand,
    mut writer: MachineWriter<ProfileMemoryOutput>,
) -> Result<()> {
    // Either call `print_output` once, or call `print_output` repeatedly every `interval` seconds
    // until the user presses ctrl-C.
    match cmd.interval {
        None => print_output(collector, &cmd, &mut writer).await?,
        Some(interval) => loop {
            print_output(collector, &cmd, &mut writer).await?;
            fuchsia_async::Timer::new(Duration::from_secs_f64(interval)).await;
        },
    }
    Ok(())
}

pub async fn print_output(
    collector: &CollectorProxy,
    cmd: &MemoryCommand,
    writer: &mut MachineWriter<ProfileMemoryOutput>,
) -> Result<()> {
    if cmd.debug_json {
        let raw_data = get_raw_data(collector).await?;
        writeln!(writer, "{}", String::from_utf8(raw_data)?)?;
        Ok(())
    } else {
        let memory_monitor_output = get_output(collector).await?;
        let processed_digest = processed::digest_from_memory_monitor_output(
            memory_monitor_output,
            cmd.buckets,
            cmd.undigested,
        );
        let output = if cmd.process_koids.is_empty() && cmd.process_names.is_empty() {
            ProfileMemoryOutput::CompleteDigest(processed_digest)
        } else {
            let process_koids = cmd
                .process_koids
                .iter()
                .copied()
                .map(processed::ProcessKoid::new)
                .collect::<Vec<processed::ProcessKoid>>();
            filter_digest_by_process(processed_digest, &process_koids, &cmd.process_names)
        };
        if cmd.csv {
            write_csv_output(writer, output, cmd.buckets)
        } else {
            if writer.is_machine() {
                writer.machine(&output)?;
                Ok(())
            } else {
                write_human_readable_output(writer, output, cmd.exact_sizes)
            }
        }
    }
}

/// Returns a buffer containing the data that `CollectorProxy` wrote.
async fn get_raw_data(collector: &CollectorProxy) -> Result<Vec<u8>> {
    // Create a socket.
    let (rx, tx) = fidl::Socket::create_stream();

    // Ask the collector to fill the socket with the data.
    collector.collect_json_stats(tx)?;

    // Read all the bytes sent from the other end of the socket.
    let mut rx_async = fidl::AsyncSocket::from_socket(rx);
    let mut buffer = Vec::new();
    rx_async.read_to_end(&mut buffer).await?;

    Ok(buffer)
}

/// Returns the `MemoryMonitorOutput` obtained via the `CollectorProxy`. Performs basic schema validation.
async fn get_output(collector: &CollectorProxy) -> anyhow::Result<raw::MemoryMonitorOutput> {
    let buffer = get_raw_data(collector).await?;
    Ok(serde_json::from_slice(&buffer)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::AsyncWriteExt;
    use target_holders::fake_proxy;

    lazy_static::lazy_static! {
        static ref EXPECTED_CAPTURE: raw::Capture = raw::Capture{
            time: 0,
            kernel: raw::Kernel{
            total: 0,
            free: 0,
            wired: 0,
            total_heap: 0,
            free_heap: 0,
            vmo: 0,
            vmo_pager_total: 0,
            vmo_pager_newest: 0,
            vmo_pager_oldest: 0,
            vmo_discardable_locked: 0,
            vmo_discardable_unlocked: 0,
            mmu: 0,
            ipc: 0,
            other: 0,
            zram_compressed_total: None,
            zram_fragmentation: None,
            zram_uncompressed: None
            },
            processes: vec![
            raw::Process::Headers(raw::ProcessHeaders::default()),
            raw::Process::Data(raw::ProcessData{koid: 2, name: "process1".to_string(), vmos: vec![1, 2, 3]}),
            raw::Process::Data(raw::ProcessData{koid: 3, name: "process2".to_string(), vmos: vec![2, 3, 4]}),
            ],
            vmo_names: vec!["name1".to_string(), "name2".to_string()],
            vmos: vec![],
        };

        static ref EXPECTED_OUTPUT: raw::MemoryMonitorOutput = raw::MemoryMonitorOutput{
            capture: EXPECTED_CAPTURE.clone(),
            buckets_definitions: vec![]
        };

        static ref DATA_WRITTEN_BY_MEMORY_MONITOR: Vec<u8> = serde_json::to_vec(&*EXPECTED_OUTPUT).unwrap();

    }

    fn create_fake_collector_proxy() -> CollectorProxy {
        fake_proxy(move |req| match req {
            fidl_fuchsia_memory_inspection::CollectorRequest::CollectJsonStats {
                socket, ..
            } => {
                let mut s = fidl::AsyncSocket::from_socket(socket);
                fuchsia_async::Task::local(async move {
                    s.write_all(&DATA_WRITTEN_BY_MEMORY_MONITOR).await.unwrap();
                })
                .detach();
            }
            _ => panic!("Expected CollectorRequest/CollectJsonStats; got {:?}", req),
        })
    }

    /// Tests that `get_raw_data` properly reads data from the memory monitor service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_raw_data_test() {
        let collector = create_fake_collector_proxy();
        let raw_data = get_raw_data(&collector).await.expect("failed to get raw data");
        assert_eq!(raw_data, *DATA_WRITTEN_BY_MEMORY_MONITOR);
    }

    /// Tests that `get_output` properly reads and parses data from the memory monitor service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_output_test() {
        let collector = create_fake_collector_proxy();
        let output = get_output(&collector).await.expect("failed to get output");
        assert_eq!(output, *EXPECTED_OUTPUT);
    }
}
