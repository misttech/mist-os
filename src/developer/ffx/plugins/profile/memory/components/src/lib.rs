// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod json;
mod output;

#[macro_use]
extern crate prettytable;
use anyhow::Result;
use async_trait::async_trait;
use attribution_processing::kernel_statistics::KernelStatistics;
use attribution_processing::summary::MemorySummary;
use attribution_processing::{AttributionData, Principal, Resource};
use errors::ffx_error;
use ffx_profile_memory_components_args::ComponentsCommand;
use ffx_writer::{SimpleWriter, ToolIO as _};
use fho::{AvailabilityFlag, FfxMain, FfxTool};
use futures::AsyncReadExt;
use json::JsonConvertible;
use target_holders::moniker;

use fidl_fuchsia_memory_attribution_plugin as fplugin;

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_memory_components"))]
pub struct MemoryComponentsTool {
    #[command]
    cmd: ComponentsCommand,
    #[with(moniker("/core/memory_monitor2"))]
    monitor_proxy: fplugin::MemoryMonitorProxy,
}

fho::embedded_plugin!(MemoryComponentsTool);

#[async_trait(?Send)]
impl FfxMain for MemoryComponentsTool {
    type Writer = SimpleWriter;

    /// Forwards the specified memory pressure level to the fuchsia.memory.debug.MemoryPressure FIDL
    /// interface.
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let snapshot = match self.cmd.stdin_input {
            false => self.load_from_device().await?,
            true => {
                fplugin::Snapshot::from_json(&serde_json::from_reader(std::io::stdin()).unwrap())
                    .unwrap()
            }
        };
        if self.cmd.debug_json {
            println!("{}", serde_json::to_string(&snapshot.to_json()).unwrap());
        } else {
            let (output, kernel_statistics) = process_snapshot(snapshot);
            output::write_summary(&mut writer, &output, kernel_statistics)
                .or_else(|e| writeln!(writer.stderr(), "Error: {}", e))
                .map_err(|e| fho::Error::Unexpected(e.into()))?;
        }
        Ok(())
    }
}

impl MemoryComponentsTool {
    async fn load_from_device(&self) -> fho::Result<fplugin::Snapshot> {
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

fn process_snapshot(snapshot: fplugin::Snapshot) -> (MemorySummary, KernelStatistics) {
    // Map from moniker token ID to Principal struct.
    let principals: Vec<Principal> =
        snapshot.principals.into_iter().flatten().map(|p| p.into()).collect();

    // Map from kernel resource koid to Resource struct.
    let resources: Vec<Resource> =
        snapshot.resources.into_iter().flatten().map(|r| r.into()).collect();
    // Map from subject moniker token ID to Attribution struct.
    let attributions = snapshot.attributions.unwrap().into_iter().map(|a| a.into()).collect();

    (
        attribution_processing::attribute_vmos(AttributionData {
            principals_vec: principals,
            resources_vec: resources,
            resource_names: snapshot.resource_names.unwrap(),
            attributions,
        })
        .summary(),
        snapshot.kernel_statistics.unwrap().into(),
    )
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
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1003),
                    name_index: Some(3),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
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
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1007),
                    name_index: Some(7),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(128),
                        populated_bytes: Some(256),
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
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        parent: Some(1011),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1011),
                    name_index: Some(11),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1012),
                    name_index: Some(12),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1013),
                    name_index: Some(13),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                "root_job".to_owned(),
                "root_process".to_owned(),
                "root_vmo".to_owned(),
                "shared_vmo".to_owned(),
                "runner_job".to_owned(),
                "runner_process".to_owned(),
                "runner_vmo".to_owned(),
                "component_vmo".to_owned(),
                "component_2_job".to_owned(),
                "2_process".to_owned(),
                "2_vmo".to_owned(),
                "2_vmo_parent".to_owned(),
                "component_vmo_mapped".to_owned(),
                "component_vmo_mapped2".to_owned(),
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
            ..Default::default()
        };

        let (output, _) = process_snapshot(snapshot);

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 4);

        let principals: HashMap<u64, PrincipalSummary> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

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
                        "root_vmo".to_owned(),
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
                        "shared_vmo".to_owned(),
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
                    "runner_vmo".to_owned(),
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
                committed_private: 2048,
                committed_scaled: 2560.0,
                committed_total: 3072,
                populated_private: 4096,
                populated_scaled: 5120.0,
                populated_total: 6144,
                attributor: Some("root".to_owned()),
                processes: vec!["2_process (1009)".to_owned()],
                vmos: vec![
                    (
                        "shared_vmo".to_owned(),
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
                        "2_vmo".to_owned(),
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
                        "2_vmo_parent".to_owned(),
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
                        "component_vmo".to_owned(),
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
                        "component_vmo_mapped".to_owned(),
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
                        "component_vmo_mapped2".to_owned(),
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
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                "root_job".to_owned(),
                "component_job".to_owned(),
                "component_process".to_owned(),
                "component_vmo".to_owned(),
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
            ..Default::default()
        };

        let (output, _) = process_snapshot(snapshot);

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 3);

        let principals: HashMap<u64, PrincipalSummary> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

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
                    "component_vmo".to_owned(),
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
