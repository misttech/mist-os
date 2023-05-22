// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl_fuchsia_net_debug as fnet_debug;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fuchsia_zircon as zx;
use futures::{channel::oneshot, FutureExt as _};
use humansize::FileSize as _;
use netstack_testing_common::realms::{
    KnownServiceProvider, Netstack, ProdNetstack2, ProdNetstack3, TestSandboxExt as _,
};
use nonzero_ext::nonzero;
use std::{collections::HashMap, sync::Arc};

mod interfaces;
mod sockets;

#[fuchsia::main]
async fn main() {
    const BENCHMARK_NAME: &str = "fuchsia.netstack.resource_usage";
    const NUM_RUNS: std::num::NonZeroUsize = nonzero!(5usize);
    let metrics = if let Ok(_) = std::env::var("NETSTACK3") {
        let benchmark_name = format!("{BENCHMARK_NAME}.netstack3");
        [
            run_benchmark::<sockets::UdpSockets, ProdNetstack3>(&benchmark_name, NUM_RUNS).await,
            run_benchmark::<sockets::TcpSockets, ProdNetstack3>(&benchmark_name, NUM_RUNS).await,
            run_benchmark::<interfaces::Interfaces, ProdNetstack3>(&benchmark_name, NUM_RUNS).await,
        ]
    } else {
        [
            run_benchmark::<sockets::UdpSockets, ProdNetstack2>(BENCHMARK_NAME, NUM_RUNS).await,
            run_benchmark::<sockets::TcpSockets, ProdNetstack2>(BENCHMARK_NAME, NUM_RUNS).await,
            run_benchmark::<interfaces::Interfaces, ProdNetstack2>(BENCHMARK_NAME, NUM_RUNS).await,
        ]
    }
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let metrics_json = serde_json::to_string_pretty(&metrics).expect("serialize metrics as JSON");
    std::fs::write(format!("/custom_artifacts/results.fuchsiaperf.json"), metrics_json)
        .expect("write metrics as custom artifact");
}

#[async_trait(?Send)]
trait Workload {
    const NAME: &'static str;

    /// Run a self-contained, repeatable workload against the provided hermetic
    /// netstack.
    async fn run(netstack: &netemul::TestRealm<'_>);
}

async fn run_benchmark<W: Workload, N: Netstack>(
    suite_name: &str,
    runs: std::num::NonZeroUsize,
) -> Vec<fuchsiaperf::FuchsiaPerfBenchmarkResult> {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let netstack = sandbox
        .create_netstack_realm_with::<N, _, _>(
            W::NAME.replace("/", "-"),
            &[KnownServiceProvider::SecureStash],
        )
        .expect("create netstack");

    // Wait for the netstack to start up and initialize the loopback interface.
    let interfaces = {
        let interfaces_state = netstack
            .connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("connect to protocol");
        fnet_interfaces_ext::existing(
            fnet_interfaces_ext::event_stream_from_state(&interfaces_state)
                .expect("get interface event stream"),
            HashMap::<u64, _>::new(),
        )
        .await
        .expect("collect existing interfaces")
    };
    assert_eq!(interfaces.len(), 1);
    let fnet_interfaces_ext::Properties { device_class, online, .. } =
        interfaces.values().into_iter().next().unwrap();
    assert_eq!(device_class, &fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty));
    assert!(online);

    let diagnostics = netstack
        .connect_to_protocol::<fnet_debug::DiagnosticsMarker>()
        .expect("connect to protocol");
    let process = diagnostics
        .get_process_handle_for_inspection()
        .await
        .expect("get handle to netstack process");
    let process = Arc::new(process);

    // Record baseline resource (memory + handle) usage.
    let baseline = ResourceUsage::record(&process);

    let (done, rx) = oneshot::channel();
    let process_clone = process.clone();
    let measure_peak = fuchsia_async::Task::spawn(measure_peak_usage(rx, process_clone));
    let start_time = std::time::Instant::now();

    W::run(&netstack).await;
    let initial_increase = ResourceUsage::record(&process) - &baseline;
    for _ in 0..runs.get() - 1 {
        W::run(&netstack).await;
    }

    let runtime = start_time.elapsed();
    done.send(()).expect("peak usage task should be running");
    let peak = measure_peak.await;

    // Record the increase in resource usage after we are done exercising the
    // netstack.
    //
    // This is more likely to indicate a real resource leak than any amount of
    // initial increase measured after just one run; there is often an initial
    // amount of allocation that must occur to service a large workload but
    // which does not actually leak in the sense that it can be reused if the
    // same workload is re-run.
    let increase = ResourceUsage::record(&process) - &baseline;

    eprintln!("================== workload: {} ==================\n", W::NAME);
    eprintln!("Running workload {} times took {:?}\n", runs.get(), runtime);
    eprintln!("Baseline resource usage:\n{baseline}");
    eprintln!("Peak resource usage:\n{peak}");
    eprintln!("Increase in resource usage from baseline:\n{increase}");

    [
        baseline.generate_fuchsiaperf(suite_name, &format!("{}/Baseline", W::NAME)),
        initial_increase.generate_fuchsiaperf(suite_name, &format!("{}/InitialIncrease", W::NAME)),
        increase.generate_fuchsiaperf(suite_name, &format!("{}/Increase", W::NAME)),
        peak.generate_fuchsiaperf(suite_name, &format!("{}/Peak", W::NAME)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[derive(Debug, Default)]
struct ResourceUsage {
    handles: HandleUsage,
    memory: MemoryUsage,
}

#[derive(Debug)]
struct HandleUsage {
    total: usize,
    counts: [usize; zx::sys::ZX_OBJ_TYPE_UPPER_BOUND],
}

impl Default for HandleUsage {
    fn default() -> Self {
        Self { total: 0, counts: [0; zx::sys::ZX_OBJ_TYPE_UPPER_BOUND] }
    }
}

impl From<zx::ProcessHandleStats> for HandleUsage {
    fn from(zx::ProcessHandleStats { handle_count }: zx::ProcessHandleStats) -> Self {
        Self {
            total: handle_count.iter().copied().map(|n| n as usize).sum(),
            counts: handle_count.map(|n| n.try_into().unwrap()),
        }
    }
}

#[derive(Debug, Default)]
struct MemoryUsage {
    private_bytes: usize,
    shared_bytes: usize,
}

impl From<zx::TaskStatsInfo> for MemoryUsage {
    fn from(info: zx::TaskStatsInfo) -> Self {
        let zx::TaskStatsInfo {
            mem_mapped_bytes: _,
            mem_private_bytes,
            mem_shared_bytes,
            mem_scaled_shared_bytes: _,
        } = info;
        Self { private_bytes: mem_private_bytes, shared_bytes: mem_shared_bytes }
    }
}

impl ResourceUsage {
    fn record(process: &Arc<zx::Process>) -> Self {
        Self {
            handles: process.handle_stats().expect("get netstack handle stats").into(),
            memory: process.task_stats().expect("get netstack task stats").into(),
        }
    }

    fn generate_fuchsiaperf(
        self,
        suite: &str,
        prefix: &str,
    ) -> Vec<fuchsiaperf::FuchsiaPerfBenchmarkResult> {
        let ResourceUsage {
            handles: HandleUsage { total, counts },
            memory: MemoryUsage { private_bytes, shared_bytes },
        } = self;

        let mut results = vec![
            fuchsiaperf::FuchsiaPerfBenchmarkResult {
                test_suite: suite.to_string(),
                label: format!("{prefix}/Memory/Private"),
                values: vec![private_bytes as f64],
                unit: "bytes".to_string(),
            },
            fuchsiaperf::FuchsiaPerfBenchmarkResult {
                test_suite: suite.to_string(),
                label: format!("{prefix}/Memory/Shared"),
                values: vec![shared_bytes as f64],
                unit: "bytes".to_string(),
            },
            fuchsiaperf::FuchsiaPerfBenchmarkResult {
                test_suite: suite.to_string(),
                label: format!("{prefix}/Handles/Total"),
                values: vec![total as f64],
                unit: "count".to_string(),
            },
        ];
        for (object_type, n) in counts.into_iter().enumerate() {
            let name = match zx::ObjectType::from_raw(
                object_type.try_into().expect("object type from index"),
            ) {
                zx::ObjectType::VMO => "Vmo",
                zx::ObjectType::CHANNEL => "Channel",
                zx::ObjectType::SOCKET => "Socket",
                zx::ObjectType::EVENTPAIR => "Eventpair",
                zx::ObjectType::FIFO => "Fifo",
                // Do not report counts for these handle types as separate metrics in order to
                // avoid producing an inordinate amount of total metrics. None of these handle
                // types is currently used frequently by the netstack, and if that changes, it
                // will be reflected in the total handle count metric.
                zx::ObjectType::PROCESS
                | zx::ObjectType::THREAD
                | zx::ObjectType::EVENT
                | zx::ObjectType::PORT
                | zx::ObjectType::INTERRUPT
                | zx::ObjectType::PCI_DEVICE
                | zx::ObjectType::DEBUGLOG
                | zx::ObjectType::RESOURCE
                | zx::ObjectType::JOB
                | zx::ObjectType::VMAR
                | zx::ObjectType::GUEST
                | zx::ObjectType::VCPU
                | zx::ObjectType::TIMER
                | zx::ObjectType::IOMMU
                | zx::ObjectType::BTI
                | zx::ObjectType::PROFILE
                | zx::ObjectType::PMT
                | zx::ObjectType::SUSPEND_TOKEN
                | zx::ObjectType::PAGER
                | zx::ObjectType::EXCEPTION
                | zx::ObjectType::CLOCK
                | zx::ObjectType::STREAM
                | zx::ObjectType::MSI
                | zx::ObjectType::NONE => continue,
                _ if n == 0 => continue,
                other => {
                    eprintln!("unexpected zircon object type: {:?} n={}", other, n);
                    continue;
                }
            };
            results.push(fuchsiaperf::FuchsiaPerfBenchmarkResult {
                test_suite: suite.to_string(),
                label: format!("{prefix}/Handles/{name}"),
                values: vec![n as f64],
                unit: "count".to_string(),
            });
        }
        results
    }
}

impl std::ops::Sub<&ResourceUsage> for ResourceUsage {
    type Output = Self;

    fn sub(self, rhs: &Self) -> Self::Output {
        let mut result = ResourceUsage {
            handles: HandleUsage {
                total: self.handles.total.saturating_sub(rhs.handles.total),
                counts: self.handles.counts,
            },
            memory: MemoryUsage {
                private_bytes: self.memory.private_bytes.saturating_sub(rhs.memory.private_bytes),
                shared_bytes: self.memory.shared_bytes.saturating_sub(rhs.memory.shared_bytes),
            },
        };
        for (object_type, n) in result.handles.counts.iter_mut().enumerate() {
            *n = n.saturating_sub(rhs.handles.counts[object_type]);
        }
        result
    }
}

impl std::fmt::Display for ResourceUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { handles, memory } = self;
        writeln!(f, "Total handles: {}", handles.total)?;
        for (object_type, n) in handles.counts.iter().enumerate() {
            if *n != 0 {
                let object_type = zx::ObjectType::from_raw(
                    object_type.try_into().expect("object type from index"),
                );
                writeln!(f, "\t{object_type:?} handles: {n}")?;
            }
        }
        writeln!(f, "Memory usage:")?;
        writeln!(
            f,
            "\tPrivate: {}",
            memory
                .private_bytes
                .file_size(humansize::file_size_opts::BINARY)
                .expect("format memory usage")
        )?;
        writeln!(
            f,
            "\tShared: {}",
            memory
                .shared_bytes
                .file_size(humansize::file_size_opts::BINARY)
                .expect("format memory usage")
        )?;

        Ok(())
    }
}

async fn measure_peak_usage(
    done: oneshot::Receiver<()>,
    process: Arc<zx::Process>,
) -> ResourceUsage {
    let mut max = ResourceUsage::default();
    let mut done = done.fuse();
    loop {
        let current = ResourceUsage::record(&process);
        max.handles.total = usize::max(max.handles.total, current.handles.total);
        for (object_type, n) in max.handles.counts.iter_mut().enumerate() {
            *n = usize::max(*n, current.handles.counts[object_type]);
        }
        max.memory.private_bytes =
            usize::max(max.memory.private_bytes, current.memory.private_bytes);
        max.memory.shared_bytes = usize::max(max.memory.shared_bytes, current.memory.shared_bytes);

        futures::select! {
            result = done => {
                result.expect("should receive message when benchmark is complete");
                break;
            },
            () = fuchsia_async::Timer::new(zx::Duration::from_millis(100)).fuse() => {},
        }
    }
    max
}
