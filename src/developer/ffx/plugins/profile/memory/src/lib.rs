// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Library that obtains and prints memory digests of a running fuchsia device.

mod bucket;
mod digest;
mod plugin_output;
mod write_csv_output;
mod write_human_readable_output;

use {
    crate::plugin_output::filter_digest_by_process,
    crate::write_csv_output::write_csv_output,
    crate::write_human_readable_output::write_human_readable_output,
    anyhow::Context,
    anyhow::Result,
    digest::{processed, raw},
    ffx_core::ffx_plugin,
    ffx_profile_memory_args::MemoryCommand,
    ffx_writer::Writer,
    fidl::endpoints::ProtocolMarker,
    fidl::Socket,
    fidl_fuchsia_developer_remotecontrol as rc,
    fidl_fuchsia_io::OpenFlags,
    fidl_fuchsia_memory::MonitorProxy,
    fidl_fuchsia_memory_inspection::CollectorProxy,
    futures::AsyncReadExt,
    plugin_output::ProfileMemoryOutput,
    std::io::Write,
    std::time::Duration,
};

/// Abstracts a JSON collector.
pub trait JsonCollector {
    fn collect_json_stats(&self, tx: Socket) -> Result<()>;
}

// Implements a JSON collector with CollectorProxy.
struct JsonCollectorImpl {
    collector_proxy: CollectorProxy,
}

impl JsonCollector for JsonCollectorImpl {
    fn collect_json_stats(&self, tx: Socket) -> Result<()> {
        self.collector_proxy.collect_json_stats(tx)?;
        Ok(())
    }
}

// Implements a JSON collector with MonitorProxy.
// Deprecated. Exists to maintain backwards compatibility.
struct DeprecatedJsonCollectorImpl {
    monitor_proxy: MonitorProxy,
}

impl JsonCollector for DeprecatedJsonCollectorImpl {
    fn collect_json_stats(&self, tx: Socket) -> Result<()> {
        self.monitor_proxy.write_json_capture_and_buckets(tx)?;
        Ok(())
    }
}

/// Connect to a protocol on a remote device using the remote control proxy.
async fn remotecontrol_connect<S: ProtocolMarker>(
    remote_control: &rc::RemoteControlProxy,
    moniker: &str,
) -> Result<S::Proxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<S>()
        .with_context(|| format!("failed to create proxy to {}", S::DEBUG_NAME))?;
    remote_control
        .connect_capability(
            moniker,
            S::DEBUG_NAME,
            server_end.into_channel(),
            OpenFlags::RIGHT_READABLE,
        )
        .await?
        .map_err(|e| {
            anyhow::anyhow!("failed to connect to {} at {}: {:?}", S::DEBUG_NAME, moniker, e)
        })?;
    Ok(proxy)
}

#[ffx_plugin()]
/// Prints a memory digest to stdout.
pub async fn plugin_entrypoint(
    remote_control: rc::RemoteControlProxy,
    cmd: MemoryCommand,
    #[ffx(machine = ProfileMemoryOutput)] mut writer: Writer,
) -> Result<()> {
    let collector: Box<dyn JsonCollector> = {
        // Attempt to connect to memory monitor's fuchsia.memory.inspection.Collector.
        let proxy = remotecontrol_connect::<fidl_fuchsia_memory_inspection::CollectorMarker>(
            &remote_control,
            "/core/memory_monitor",
        )
        .await;

        if let Ok(proxy) = proxy {
            Box::new(JsonCollectorImpl { collector_proxy: proxy })
        } else {
            // Failed to connect to `fuchsia.memory.inspection.Collector`.
            // This is probably because the target is running an older version of the
            // `memory_monitor` component.
            // Fallback to connecting to the previous FIDL protocol, `fuchsia.memory.Monitor`.
            // TODO(fxb/122950) Stop connecting to`fuchsia.memory.Monitor`.
            let deprecated_proxy = remotecontrol_connect::<fidl_fuchsia_memory::MonitorMarker>(
                &remote_control,
                "/core/memory_monitor",
            )
            .await;
            let monitor_proxy = deprecated_proxy.expect("Failed to connect to fuchsia.memory.inspection.Collector and fuchsia.memory.Monitor.");
            Box::new(DeprecatedJsonCollectorImpl { monitor_proxy })
        }
    };

    // Either call `print_output` once, or call `print_output` repeatedly every `interval` seconds
    // until the user presses ctrl-C.
    match cmd.interval {
        None => print_output(&*collector, &cmd, &mut writer).await?,
        Some(interval) => loop {
            print_output(&*collector, &cmd, &mut writer).await?;
            fuchsia_async::Timer::new(Duration::from_secs_f64(interval)).await;
        },
    }
    Ok(())
}

pub async fn print_output(
    collector: &dyn JsonCollector,
    cmd: &MemoryCommand,
    writer: &mut Writer,
) -> Result<()> {
    if cmd.debug_json {
        let raw_data = get_raw_data(collector).await?;
        writeln!(writer, "{}", String::from_utf8(raw_data)?)?;
        Ok(())
    } else {
        let memory_monitor_output = get_output(collector).await?;
        let processed_digest =
            processed::digest_from_memory_monitor_output(memory_monitor_output, cmd.buckets);
        let output = if cmd.process_koids.is_empty() && cmd.process_names.is_empty() {
            ProfileMemoryOutput::CompleteDigest(processed_digest)
        } else {
            filter_digest_by_process(processed_digest, &cmd.process_koids, &cmd.process_names)
        };
        if cmd.csv {
            write_csv_output(writer, output, cmd.buckets)
        } else {
            if writer.is_machine() {
                writer.machine(&output)
            } else {
                write_human_readable_output(writer, output, cmd.exact_sizes)
            }
        }
    }
}

/// Returns a buffer containing the data that `JsonCollector` wrote.
async fn get_raw_data(collector: &dyn JsonCollector) -> Result<Vec<u8>> {
    // Create a socket.
    let (rx, tx) = fidl::Socket::create_stream();

    // Ask the collector to fill the socket with the data.
    collector.collect_json_stats(tx)?;

    // Read all the bytes sent from the other end of the socket.
    let mut rx_async = fidl::AsyncSocket::from_socket(rx)?;
    let mut buffer = Vec::new();
    rx_async.read_to_end(&mut buffer).await?;

    Ok(buffer)
}

/// Returns the `MemoryMonitorOutput` obtained via the `MonitorProxyInterface`. Performs basic schema validation.
async fn get_output(collector: &dyn JsonCollector) -> anyhow::Result<raw::MemoryMonitorOutput> {
    let buffer = get_raw_data(collector).await?;
    Ok(serde_json::from_slice(&buffer)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::AsyncWriteExt;

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

    struct TestJsonCollectorImpl {}

    impl JsonCollector for TestJsonCollectorImpl {
        fn collect_json_stats(&self, socket: Socket) -> Result<()> {
            let mut s = fidl::AsyncSocket::from_socket(socket).unwrap();
            fuchsia_async::Task::local(async move {
                s.write_all(&DATA_WRITTEN_BY_MEMORY_MONITOR).await.unwrap();
            })
            .detach();
            Ok(())
        }
    }

    /// Tests that `get_raw_data` properly reads data from the memory monitor service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_raw_data_test() {
        let collector = TestJsonCollectorImpl {};
        let raw_data = get_raw_data(&collector).await.expect("failed to get raw data");
        assert_eq!(raw_data, *DATA_WRITTEN_BY_MEMORY_MONITOR);
    }

    /// Tests that `get_output` properly reads and parses data from the memory monitor service.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_output_test() {
        let collector = TestJsonCollectorImpl {};
        let output = get_output(&collector).await.expect("failed to get output");
        assert_eq!(output, *EXPECTED_OUTPUT);
    }
}
