// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod detailed;
mod json;
mod output;
mod statistics;

#[macro_use]
extern crate prettytable;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use attribution_processing::summary::{ComponentSummaryProfileResult, MemorySummary};
use attribution_processing::{
    digest, AttributionData, AttributionDataProvider, Principal, Resource, ResourcesVisitor, ZXName,
};
use errors::ffx_error;
use ffx_profile_memory_components_args::ComponentsCommand;
use ffx_writer::{MachineWriter, ToolIO};
use fho::{AvailabilityFlag, FfxMain, FfxTool};
use fidl_fuchsia_memory_attribution_plugin::{self as fplugin, ResourceType};
use futures::AsyncReadExt;
use json::JsonConvertible;
use regex::bytes::Regex;
use serde::Serialize;
use std::io::Write;
use std::thread::sleep;
use std::time::Duration;
use target_holders::moniker;
use zerocopy::transmute_ref;

use crate::detailed::process_snapshot_detailed;
use crate::statistics::CommandMemoryStatistics;

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_memory_components"))]
pub struct MemoryComponentsTool {
    #[command]
    pub cmd: ComponentsCommand,
    #[with(moniker("/core/memory_monitor2"))]
    pub monitor_proxy: fplugin::MemoryMonitorProxy,
}

fho::embedded_plugin!(MemoryComponentsTool);

/// Minimal interface to output text or data.
/// It makes possible to adapt `MachineWriter` of various types and to delegate execution between
/// plugins.
pub trait PluginOutput<T>
where
    T: Serialize,
{
    fn is_machine(&self) -> bool;
    fn machine(&mut self, output: T) -> Result<()>;
    fn stderr(&mut self) -> &mut dyn Write;
    fn stdout(&mut self) -> &mut dyn Write;
}

#[derive(Serialize)]
pub enum ComponentProfileResult {
    Summary(ComponentSummaryProfileResult),
    Detailed(detailed::ComponentDetailedProfileResult),
}

impl PluginOutput<ComponentProfileResult> for MachineWriter<ComponentProfileResult> {
    fn is_machine(&self) -> bool {
        ToolIO::is_machine(self)
    }

    fn machine(&mut self, output: ComponentProfileResult) -> Result<()> {
        MachineWriter::<ComponentProfileResult>::machine(self, &output)?;
        Ok(())
    }

    fn stderr(&mut self) -> &mut dyn Write {
        ToolIO::stderr(self)
    }

    fn stdout(&mut self) -> &mut dyn Write {
        self
    }
}

#[async_trait(?Send)]
impl FfxMain for MemoryComponentsTool {
    type Writer = MachineWriter<ComponentProfileResult>;

    /// Forwards the specified memory pressure level to the fuchsia.memory.debug.MemoryPressure FIDL
    /// interface.
    async fn main(self, writer: MachineWriter<ComponentProfileResult>) -> fho::Result<()> {
        self.run(writer).await
    }
}

impl MemoryComponentsTool {
    pub async fn run(&self, writer: impl PluginOutput<ComponentProfileResult>) -> fho::Result<()> {
        match self.cmd.stats_only {
            Some(interval) => self.process_statistics(writer, interval).await,
            None => self.process_snapshot(writer).await,
        }
    }

    async fn process_statistics(
        &self,
        mut writer: impl PluginOutput<ComponentProfileResult>,
        interval: u64,
    ) -> std::result::Result<(), fho::Error> {
        if self.cmd.stdin_input {
            return Err(fho::Error::User(anyhow!(
                "--stdin-input is not compatible with --stats-only"
            )));
        }
        if !self.cmd.csv {
            return Err(fho::Error::User(anyhow!("only --csv is supported with --stats-only")));
        }
        let mut w = csv::WriterBuilder::new().has_headers(true).from_writer(writer.stdout());
        loop {
            let statistics: CommandMemoryStatistics = self
                .monitor_proxy
                .get_system_statistics()
                .await
                .map_err(|err| ffx_error!("Failed to get statistics: {err}"))?
                .try_into()
                .map_err(|err| ffx_error!("Failed to convert statistics: {err}"))?;

            w.serialize(statistics)
                .map_err(|err| ffx_error!("Failed to write statistics: {err}"))?;
            w.flush().map_err(|err| ffx_error!("Failed to flush stdout: {err}"))?;
            sleep(Duration::from_secs(interval));
        }
    }

    async fn process_snapshot(
        &self,
        mut writer: impl PluginOutput<ComponentProfileResult>,
    ) -> std::result::Result<(), fho::Error> {
        let snapshot = match self.cmd.stdin_input {
            false => self.load_snapshot_from_device().await?,
            true => {
                fplugin::Snapshot::from_json(&serde_json::from_reader(std::io::stdin()).unwrap())
                    .unwrap()
            }
        };

        if self.cmd.debug_json {
            println!("{}", serde_json::to_string(&snapshot.to_json()).unwrap());
            return Ok(());
        }

        if self.cmd.detailed {
            if !writer.is_machine() {
                return Err(fho::Error::User(anyhow::anyhow!(
                    "--detailed requires machine output"
                )));
            }
            let output = process_snapshot_detailed(snapshot)?;
            writer.machine(ComponentProfileResult::Detailed(output))?;
            return Ok(());
        }

        let profile_result = process_snapshot_summary(snapshot);
        if writer.is_machine() {
            writer.machine(ComponentProfileResult::Summary(profile_result))?;
        } else {
            output::write_summary(&mut writer.stdout(), self.cmd.csv, &profile_result)
                .or_else(|e| writeln!(writer.stderr(), "Error: {}", e))
                .map_err(|e| fho::Error::Unexpected(e.into()))?;
        }
        Ok(())
    }

    async fn load_snapshot_from_device(&self) -> fho::Result<fplugin::Snapshot> {
        let (client_end, server_end) = fidl::Socket::create_stream();
        let mut client_socket = fidl::AsyncSocket::from_socket(client_end);

        self.monitor_proxy
            .get_snapshot(server_end)
            .map_err(|err| ffx_error!("Failed to call MemoryMonitorProxy/GetSnapshot: {err}"))?;

        let mut buffer: Vec<u8> = Vec::new();
        client_socket
            .read_to_end(&mut buffer)
            .await
            .map_err(|err| ffx_error!("Failed to read socket: {err}"))?;
        let snapshot: fplugin::Snapshot = fidl::unpersist(&buffer)
            .map_err(|err| ffx_error!("Failed to unpersist elements: {err}"))?;
        Ok(snapshot)
    }
}

pub struct SnapshotAttributionDataProvider<'a> {
    resources: &'a Vec<Resource>,
    resource_names: &'a Vec<fplugin::ResourceName>,
}

impl<'a> AttributionDataProvider for SnapshotAttributionDataProvider<'a> {
    fn for_each_resource(&self, visitor: &mut impl ResourcesVisitor) -> Result<(), anyhow::Error> {
        for resource in self.resources {
            if let Resource { koid, name_index, resource_type: ResourceType::Vmo(vmo), .. } =
                resource
            {
                visitor.on_vmo(
                    *koid,
                    transmute_ref!(&self.resource_names[*name_index]),
                    vmo.clone(),
                )?;
            }
        }
        for resource in self.resources {
            if let Resource {
                koid,
                name_index,
                resource_type: ResourceType::Process(process),
                ..
            } = resource
            {
                visitor.on_process(
                    *koid,
                    transmute_ref!(&self.resource_names[*name_index]),
                    process.clone(),
                )?;
            }
        }

        Ok(())
    }

    fn get_attribution_data(
        &self,
    ) -> futures::future::BoxFuture<'_, std::result::Result<AttributionData, anyhow::Error>> {
        todo!()
    }
}

fn process_snapshot_summary(snapshot: fplugin::Snapshot) -> ComponentSummaryProfileResult {
    // Map from moniker token ID to Principal struct.
    let principals: Vec<Principal> =
        snapshot.principals.into_iter().flatten().map(|p| p.into()).collect();

    // Map from kernel resource koid to Resource struct.
    let resources: Vec<Resource> =
        snapshot.resources.into_iter().flatten().map(|r| r.into()).collect();
    // Map from subject moniker token ID to Attribution struct.
    let attributions = snapshot.attributions.unwrap().into_iter().map(|a| a.into()).collect();
    let default_empty_vec = Vec::new();
    let bucket_definitions: Vec<digest::BucketDefinition> = snapshot
        .bucket_definitions
        .as_ref()
        .unwrap_or(&default_empty_vec)
        .iter()
        .map(|bd| digest::BucketDefinition {
            name: bd.name.clone().unwrap_or_default(),
            process: bd.process.as_ref().map(|p| Regex::new(&p).unwrap()),
            vmo: bd.vmo.as_ref().map(|p| Regex::new(&p).unwrap()),
            event_code: 0, // The information is unavailable client side.
        })
        .collect();
    let digest = digest::Digest::compute(
        &SnapshotAttributionDataProvider {
            resources: &resources,
            resource_names: snapshot.resource_names.as_ref().unwrap(),
        },
        &snapshot.kernel_statistics.as_ref().unwrap().memory_stats.as_ref().unwrap(),
        &snapshot.kernel_statistics.as_ref().unwrap().compression_stats.as_ref().unwrap(),
        &bucket_definitions,
    )
    .expect("Digest computation should succeed");
    let MemorySummary { principals, unclaimed } =
        attribution_processing::attribute_vmos(AttributionData {
            principals_vec: principals,
            resources_vec: resources,
            resource_names: snapshot
                .resource_names
                .unwrap()
                .iter()
                .map(|n| ZXName::from_bytes_lossy(n))
                .collect(),
            attributions,
        })
        .summary();
    ComponentSummaryProfileResult {
        kernel: snapshot.kernel_statistics.unwrap().into(),
        principals,
        unclaimed,
        digest,
        performance: snapshot.performance_metrics.unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use attribution_processing::summary::{PrincipalSummary, VmoSummary};
    use fidl_fuchsia_memory_attribution_plugin as fplugin;

    #[test]
    fn test_gather_resources() {
        // Create a fake snapshot with 4 principals:
        // root (0)
        //  - runner (1)
        //    - component 3 (3)
        //  - component 2 (2)
        //
        // and the following job/process/vmo hierarchy:
        // root_job (1000)
        //  * root_process (1001)
        //    . root_vmo (1002)
        //    . shared_vmo (1003)
        //  - runner_job (1004)
        //    * runner_process (1005)
        //      . runner_vmo (1006)
        //      . component_vmo (1007)
        //      . component_vmo2 (1012)
        //      . component_vmo3 (1013)
        //  - component_2_job (1008)
        //    * 2_process (1009)
        //      . 2_vmo (1010)
        //      . shared_vmo (1003)
        // And an additional parent VMO for 2_vmo, 2_vmo_parent (1011).

        let snapshot = fplugin::Snapshot {
            attributions: Some(vec![
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1000)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1004)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1008)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 3 }),
                    resources: Some(vec![
                        fplugin::ResourceReference::KernelObject(1007),
                        fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                            process: 1005,
                            base: 1024,
                            len: 1024,
                        }),
                    ]),
                    ..Default::default()
                },
            ]),
            principals: Some(vec![
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    description: Some(fplugin::Description::Component("root".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: None,
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    description: Some(fplugin::Description::Component("runner".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    description: Some(fplugin::Description::Component("component 2".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 3 }),
                    description: Some(fplugin::Description::Component("component 3".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    ..Default::default()
                },
            ]),
            resources: Some(vec![
                fplugin::Resource {
                    koid: Some(1000),
                    name_index: Some(0),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![1004, 1008]),
                        processes: Some(vec![1001]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1001),
                    name_index: Some(1),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1002, 1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1002),
                    name_index: Some(2),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1003),
                    name_index: Some(3),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1004),
                    name_index: Some(4),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1005]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1005),
                    name_index: Some(5),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1006, 1007, 1012]),
                        mappings: Some(vec![
                            fplugin::Mapping {
                                vmo: Some(1006),
                                address_base: Some(0),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1012),
                                address_base: Some(1024),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1013),
                                address_base: Some(1536),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1006),
                                address_base: Some(2048),
                                size: Some(512),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1006),
                    name_index: Some(6),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1007),
                    name_index: Some(7),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(128),
                        private_populated_bytes: Some(256),
                        scaled_committed_bytes: Some(128),
                        scaled_populated_bytes: Some(256),
                        total_committed_bytes: Some(128),
                        total_populated_bytes: Some(256),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1008),
                    name_index: Some(8),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1009]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1009),
                    name_index: Some(9),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1010, 1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1010),
                    name_index: Some(10),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        parent: Some(1011),
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1011),
                    name_index: Some(11),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1012),
                    name_index: Some(12),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1013),
                    name_index: Some(13),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                *ZXName::from_string_lossy("root_job").buffer(),
                *ZXName::from_string_lossy("root_process").buffer(),
                *ZXName::from_string_lossy("root_vmo").buffer(),
                *ZXName::from_string_lossy("shared_vmo").buffer(),
                *ZXName::from_string_lossy("runner_job").buffer(),
                *ZXName::from_string_lossy("runner_process").buffer(),
                *ZXName::from_string_lossy("runner_vmo").buffer(),
                *ZXName::from_string_lossy("component_vmo").buffer(),
                *ZXName::from_string_lossy("component_2_job").buffer(),
                *ZXName::from_string_lossy("2_process").buffer(),
                *ZXName::from_string_lossy("2_vmo").buffer(),
                *ZXName::from_string_lossy("2_vmo_parent").buffer(),
                *ZXName::from_string_lossy("component_vmo_mapped").buffer(),
                *ZXName::from_string_lossy("component_vmo_mapped2").buffer(),
            ]),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(fidl_fuchsia_kernel::MemoryStats {
                    total_bytes: Some(1),
                    free_bytes: Some(2),
                    free_loaned_bytes: Some(3),
                    wired_bytes: Some(4),
                    total_heap_bytes: Some(5),
                    free_heap_bytes: Some(6),
                    vmo_bytes: Some(7),
                    mmu_overhead_bytes: Some(8),
                    ipc_bytes: Some(9),
                    cache_bytes: Some(10),
                    slab_bytes: Some(11),
                    zram_bytes: Some(12),
                    other_bytes: Some(13),
                    vmo_reclaim_total_bytes: Some(14),
                    vmo_reclaim_newest_bytes: Some(15),
                    vmo_reclaim_oldest_bytes: Some(16),
                    vmo_reclaim_disabled_bytes: Some(17),
                    vmo_discardable_locked_bytes: Some(18),
                    vmo_discardable_unlocked_bytes: Some(19),
                    ..Default::default()
                }),
                compression_stats: Some(fidl_fuchsia_kernel::MemoryStatsCompression {
                    uncompressed_storage_bytes: Some(15),
                    compressed_storage_bytes: Some(16),
                    compressed_fragmentation_bytes: Some(17),
                    compression_time: Some(18),
                    decompression_time: Some(19),
                    total_page_compression_attempts: Some(20),
                    failed_page_compression_attempts: Some(21),
                    total_page_decompressions: Some(22),
                    compressed_page_evictions: Some(23),
                    eager_page_compressions: Some(24),
                    memory_pressure_page_compressions: Some(25),
                    critical_memory_page_compressions: Some(26),
                    pages_decompressed_unit_ns: Some(27),
                    pages_decompressed_within_log_time: Some([0, 1, 2, 3, 4, 5, 6, 7]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            performance_metrics: Some(fplugin::PerformanceImpactMetrics {
                some_memory_stalls_ns: Some(10),
                full_memory_stalls_ns: Some(5),
                ..Default::default()
            }),
            bucket_definitions: Some(vec![fplugin::BucketDefinition {
                name: Some("da_bucket".to_string()),
                process: None,
                vmo: Some("root_vmo".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let ComponentSummaryProfileResult { principals, unclaimed, performance, digest, .. } =
            process_snapshot_summary(snapshot);

        // VMO 1011 is the parent of VMO 1010, but not claimed by any Principal; it is thus
        // unclaimed.
        assert_eq!(unclaimed, 2048);
        assert_eq!(principals.len(), 4);

        let principals: HashMap<u64, PrincipalSummary> =
            principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &PrincipalSummary {
                id: 0,
                name: "root".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1536.0,
                committed_total: 2048,
                populated_private: 2048,
                populated_scaled: 3072.0,
                populated_total: 4096,
                attributor: None,
                processes: vec!["root_process (1001)".to_owned()],
                vmos: vec![
                    (
                        ZXName::from_string_lossy("root_vmo"),
                        VmoSummary {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        ZXName::from_string_lossy("shared_vmo"),
                        VmoSummary {
                            count: 1,
                            committed_private: 0,
                            committed_scaled: 512.0,
                            committed_total: 1024,
                            populated_private: 0,
                            populated_scaled: 1024.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    )
                ]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&1).unwrap(),
            &PrincipalSummary {
                id: 1,
                name: "runner".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1024.0,
                committed_total: 1024,
                populated_private: 2048,
                populated_scaled: 2048.0,
                populated_total: 2048,
                attributor: Some("root".to_owned()),
                processes: vec!["runner_process (1005)".to_owned()],
                vmos: vec![(
                    ZXName::from_string_lossy("runner_vmo"),
                    VmoSummary {
                        count: 1,
                        committed_private: 1024,
                        committed_scaled: 1024.0,
                        committed_total: 1024,
                        populated_private: 2048,
                        populated_scaled: 2048.0,
                        populated_total: 2048,
                        ..Default::default()
                    }
                )]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&2).unwrap(),
            &PrincipalSummary {
                id: 2,
                name: "component 2".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1536.0,
                committed_total: 2048,
                populated_private: 2048,
                populated_scaled: 3072.0,
                populated_total: 4096,
                attributor: Some("root".to_owned()),
                processes: vec!["2_process (1009)".to_owned()],
                vmos: vec![
                    (
                        ZXName::from_string_lossy("shared_vmo"),
                        VmoSummary {
                            count: 1,
                            committed_private: 0,
                            committed_scaled: 512.0,
                            committed_total: 1024,
                            populated_private: 0,
                            populated_scaled: 1024.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        ZXName::from_string_lossy("2_vmo"),
                        VmoSummary {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                ]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&3).unwrap(),
            &PrincipalSummary {
                id: 3,
                name: "component 3".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 2176,
                committed_scaled: 2176.0,
                committed_total: 2176,
                populated_private: 4352,
                populated_scaled: 4352.0,
                populated_total: 4352,
                attributor: Some("runner".to_owned()),
                processes: vec!["runner_process (1005)".to_owned()],
                vmos: vec![
                    (
                        ZXName::from_string_lossy("component_vmo"),
                        VmoSummary {
                            count: 1,
                            committed_private: 128,
                            committed_scaled: 128.0,
                            committed_total: 128,
                            populated_private: 256,
                            populated_scaled: 256.0,
                            populated_total: 256,
                            ..Default::default()
                        }
                    ),
                    (
                        ZXName::from_string_lossy("component_vmo_mapped"),
                        VmoSummary {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        ZXName::from_string_lossy("component_vmo_mapped2"),
                        VmoSummary {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    )
                ]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            performance,
            fplugin::PerformanceImpactMetrics {
                some_memory_stalls_ns: Some(10),
                full_memory_stalls_ns: Some(5),
                ..Default::default()
            }
        );

        assert_eq!(
            digest,
            digest::Digest {
                buckets: vec![
                    digest::Bucket { name: "da_bucket".to_string(), size: 1024 },
                    digest::Bucket { name: "Undigested".to_string(), size: 6272 },
                    digest::Bucket { name: "Orphaned".to_string(), size: 0 },
                    digest::Bucket { name: "Kernel".to_string(), size: 39 },
                    digest::Bucket { name: "Free".to_string(), size: 2 },
                    digest::Bucket { name: "[Addl]PagerTotal".to_string(), size: 14 },
                    digest::Bucket { name: "[Addl]PagerNewest".to_string(), size: 15 },
                    digest::Bucket { name: "[Addl]PagerOldest".to_string(), size: 16 },
                    digest::Bucket { name: "[Addl]DiscardableLocked".to_string(), size: 18 },
                    digest::Bucket { name: "[Addl]DiscardableUnlocked".to_string(), size: 19 },
                    digest::Bucket { name: "[Addl]ZramCompressedBytes".to_string(), size: 16 }
                ]
            }
        );
    }

    #[test]
    fn test_reshare_resources() {
        // Create a fake snapshot with 3 principals:
        // root (0)
        //  - component 1 (1)
        //    - component 2 (2)
        //
        // and the following job/process/vmo hierarchy:
        // root_job (1000)
        //  - component_job (1001)
        //    * component_process (1002)
        //      . component_vmo (1003)
        //
        // In this scenario, component 1 reattributes component_job to component 2 entirely.

        let snapshot = fplugin::Snapshot {
            attributions: Some(vec![
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1000)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1001)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1001)]),
                    ..Default::default()
                },
            ]),
            principals: Some(vec![
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    description: Some(fplugin::Description::Component("root".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: None,
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    description: Some(fplugin::Description::Component("component 1".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    description: Some(fplugin::Description::Component("component 2".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    ..Default::default()
                },
            ]),
            resources: Some(vec![
                fplugin::Resource {
                    koid: Some(1000),
                    name_index: Some(0),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![1001]),
                        processes: Some(vec![]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1001),
                    name_index: Some(1),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1002]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1002),
                    name_index: Some(2),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1003),
                    name_index: Some(3),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                *ZXName::from_string_lossy("root_job").buffer(),
                *ZXName::from_string_lossy("component_job").buffer(),
                *ZXName::from_string_lossy("component_process").buffer(),
                *ZXName::from_string_lossy("component_vmo").buffer(),
            ]),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(fidl_fuchsia_kernel::MemoryStats {
                    total_bytes: Some(1),
                    free_bytes: Some(2),
                    free_loaned_bytes: Some(3),
                    wired_bytes: Some(4),
                    total_heap_bytes: Some(5),
                    free_heap_bytes: Some(6),
                    vmo_bytes: Some(7),
                    mmu_overhead_bytes: Some(8),
                    ipc_bytes: Some(9),
                    cache_bytes: Some(10),
                    slab_bytes: Some(11),
                    zram_bytes: Some(12),
                    other_bytes: Some(13),
                    vmo_reclaim_total_bytes: Some(14),
                    vmo_reclaim_newest_bytes: Some(15),
                    vmo_reclaim_oldest_bytes: Some(16),
                    vmo_reclaim_disabled_bytes: Some(17),
                    vmo_discardable_locked_bytes: Some(18),
                    vmo_discardable_unlocked_bytes: Some(19),
                    ..Default::default()
                }),
                compression_stats: Some(fidl_fuchsia_kernel::MemoryStatsCompression {
                    uncompressed_storage_bytes: Some(15),
                    compressed_storage_bytes: Some(16),
                    compressed_fragmentation_bytes: Some(17),
                    compression_time: Some(18),
                    decompression_time: Some(19),
                    total_page_compression_attempts: Some(20),
                    failed_page_compression_attempts: Some(21),
                    total_page_decompressions: Some(22),
                    compressed_page_evictions: Some(23),
                    eager_page_compressions: Some(24),
                    memory_pressure_page_compressions: Some(25),
                    critical_memory_page_compressions: Some(26),
                    pages_decompressed_unit_ns: Some(27),
                    pages_decompressed_within_log_time: Some([0, 1, 2, 3, 4, 5, 6, 7]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            performance_metrics: Some(fplugin::PerformanceImpactMetrics {
                some_memory_stalls_ns: Some(10),
                full_memory_stalls_ns: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };

        let ComponentSummaryProfileResult { principals, unclaimed, .. } =
            process_snapshot_summary(snapshot);

        assert_eq!(unclaimed, 0);
        assert_eq!(principals.len(), 3);

        let principals: HashMap<u64, PrincipalSummary> =
            principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &PrincipalSummary {
                id: 0,
                name: "root".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 0,
                committed_scaled: 0.0,
                committed_total: 0,
                populated_private: 0,
                populated_scaled: 0.0,
                populated_total: 0,
                attributor: None,
                processes: vec![],
                vmos: vec![].into_iter().collect(),
            }
        );

        assert_eq!(
            principals.get(&1).unwrap(),
            &PrincipalSummary {
                id: 1,
                name: "component 1".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 0,
                committed_scaled: 0.0,
                committed_total: 0,
                populated_private: 0,
                populated_scaled: 0.0,
                populated_total: 0,
                attributor: Some("root".to_owned()),
                processes: vec![],
                vmos: vec![].into_iter().collect(),
            }
        );

        assert_eq!(
            principals.get(&2).unwrap(),
            &PrincipalSummary {
                id: 2,
                name: "component 2".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1024.0,
                committed_total: 1024,
                populated_private: 2048,
                populated_scaled: 2048.0,
                populated_total: 2048,
                attributor: Some("component 1".to_owned()),
                processes: vec!["component_process (1002)".to_owned()],
                vmos: vec![(
                    ZXName::from_string_lossy("component_vmo"),
                    VmoSummary {
                        count: 1,
                        committed_private: 1024,
                        committed_scaled: 1024.0,
                        committed_total: 1024,
                        populated_private: 2048,
                        populated_scaled: 2048.0,
                        populated_total: 2048,
                        ..Default::default()
                    }
                ),]
                .into_iter()
                .collect(),
            }
        );
    }
}
