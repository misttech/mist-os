// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fplugin::{ResourceType, Vmo};
use crate::AttributionData;
use fidl_fuchsia_kernel as fkernel;
use regex::Regex;
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use std::collections::hash_map::Entry::Occupied;
use std::collections::HashMap;

const UNDIGESTED: &str = "Undigested";
const ORPHANED: &str = "Orphaned";
const KERNEL: &str = "Kernel";
const FREE: &str = "Free";
const PAGER_TOTAL: &str = "[Addl]PagerTotal";
const PAGER_NEWEST: &str = "[Addl]PagerNewest";
const PAGER_OLDEST: &str = "[Addl]PagerOldest";
const DISCARDABLE_LOCKED: &str = "[Addl]DiscardableLocked";
const DISCARDABLE_UNLOCKED: &str = "[Addl]DiscardableUnlocked";
const ZRAM_COMPRESSED_BYTES: &str = "[Addl]ZramCompressedBytes";

/// Represents a specification for aggregating memory usage in meaningful groups.
///
/// `name` represents the meaningful name of the group; grouping is done based on process and VMO
/// names.
#[derive(Deserialize)]
pub struct BucketDefinition {
    pub name: String,
    #[serde(deserialize_with = "deserialize_regex")]
    pub process: Option<Regex>,
    #[serde(deserialize_with = "deserialize_regex")]
    pub vmo: Option<Regex>,
}

impl BucketDefinition {
    /// Tests whether a process matches this bucket's definition, based on its name.
    fn process_match(&self, process: &str) -> bool {
        self.process.as_ref().map_or(true, |p| p.is_match(process))
    }

    /// Tests whether a VMO matches this bucket's definition, based on its name.
    fn vmo_match(&self, vmo: &str) -> bool {
        self.vmo.as_ref().map_or(true, |v| v.is_match(vmo))
    }
}

// Teach serde to deserialize an optional regex.
fn deserialize_regex<'de, D>(d: D) -> Result<Option<Regex>, D::Error>
where
    D: Deserializer<'de>,
{
    // Deserialize as Option<&str>
    Option::<&str>::deserialize(d)
        // If the parsing failed, return the error, otherwise transform the value
        .and_then(|os| {
            os
                // If there is a value, try to parse it as a Regex.
                .map(|s| {
                    Regex::new(s)
                        // If the regex compilation failed, wrap the error in the error type expected
                        // by serde.
                        .map_err(D::Error::custom)
                })
                // If there was a value but it failed to compile, return an error, otherwise return
                // the potentially parsed option.
                .transpose()
        })
}

/// Aggregates bytes in categories with human readable names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub name: String,
    pub size: u64,
}

/// Contains a view of the system's memory usage, aggregated in groups called buckets, which are
/// configurable.
#[derive(Debug, PartialEq, Eq)]
pub struct Digest {
    pub buckets: Vec<Bucket>,
}

impl Digest {
    /// Given means to query the system for memory usage, and a specification, this function
    /// aggregates the current memory usage into human displayable units we call buckets.
    pub fn new(
        attribution_data: &AttributionData,
        kmem_stats: fkernel::MemoryStats,
        kmem_stats_compression: fkernel::MemoryStatsCompression,
        bucket_definitions: &Vec<BucketDefinition>,
    ) -> Self {
        // Contains all VMOs that have not yet been aggregated.
        let mut undigested_vmos: HashMap<u64, (&Vmo, &String)> = attribution_data
            .resources_vec
            .iter()
            .filter_map(|r| match &r.resource_type {
                ResourceType::Vmo(vmo) => {
                    Some((r.koid, (vmo, &attribution_data.resource_names[r.name_index])))
                }
                _ => None,
            })
            .collect();

        // Collection of processes currently live in the system.
        let processes =
            attribution_data.resources_vec.iter().filter_map(|r| match &r.resource_type {
                ResourceType::Process(process) => {
                    Some((process, &attribution_data.resource_names[r.name_index]))
                }
                _ => None,
            });
        // For each bucket, filter matching processes, and among their VMOs, account for those that
        // have not been previously grouped in another buckets.  This means that the order of bucket
        // definitions defines a priority in this aggregation.
        let mut buckets: Vec<Bucket> = bucket_definitions
            .iter()
            .map(|bucket_def| Bucket {
                name: bucket_def.name.clone(),
                size: processes
                    .clone()
                    .filter(|&(_, process_name)| bucket_def.process_match(process_name))
                    .fold(0, |size, (process, _)| {
                        size + process
                            .vmos
                            .iter()
                            .flatten()
                            .map(|vmo| match undigested_vmos.entry(*vmo) {
                                Occupied(e) => {
                                    let &(vmo, name) = e.get();
                                    if bucket_def.vmo_match(name) {
                                        e.remove_entry();
                                        vmo.total_committed_bytes.unwrap_or(0)
                                    } else {
                                        0
                                    }
                                }
                                _ => 0,
                            })
                            .sum::<u64>()
                    }),
            })
            .collect();

        let vmo_size: u64 = buckets.iter().map(|Bucket { size, .. }| size).sum();
        // Extend the configured aggregation with a number of additional, occasionally useful meta
        // aggregations.
        buckets.extend(vec![
            // This bucket contains the total size of the VMOs that have not been covered by any
            // other bucket.
            Bucket {
                name: UNDIGESTED.to_string(),
                size: undigested_vmos
                    .iter()
                    .filter_map(|(_, (vmo, _))| vmo.total_committed_bytes)
                    .sum(),
            },
            // This bucket accounts for VMO bytes that have been allocated by the kernel, but not
            // claimed by any VMO (anymore).
            Bucket {
                name: ORPHANED.to_string(),
                size: (kmem_stats.vmo_bytes.unwrap_or(0) - vmo_size)
                    .clamp(0, kmem_stats.vmo_bytes.unwrap_or(0)),
            },
            // This bucket aggregates overall kernel memory usage.
            Bucket {
                name: KERNEL.to_string(),
                size: (|| {
                    Some(
                        kmem_stats.wired_bytes?
                            + kmem_stats.total_heap_bytes?
                            + kmem_stats.mmu_overhead_bytes?
                            + kmem_stats.ipc_bytes?
                            + kmem_stats.other_bytes?,
                    )
                })()
                .unwrap_or(0),
            },
            // This bucket contains this amount of free memory in the system.
            Bucket { name: FREE.to_string(), size: kmem_stats.free_bytes.unwrap_or(0) },
            // Those buckets contain pager related information.
            Bucket {
                name: PAGER_TOTAL.to_string(),
                size: kmem_stats.vmo_reclaim_total_bytes.unwrap_or(0),
            },
            Bucket {
                name: PAGER_NEWEST.to_string(),
                size: kmem_stats.vmo_reclaim_newest_bytes.unwrap_or(0),
            },
            Bucket {
                name: PAGER_OLDEST.to_string(),
                size: kmem_stats.vmo_reclaim_oldest_bytes.unwrap_or(0),
            },
            // Those buckets account for discardable memory.
            Bucket {
                name: DISCARDABLE_LOCKED.to_string(),
                size: kmem_stats.vmo_discardable_locked_bytes.unwrap_or(0),
            },
            Bucket {
                name: DISCARDABLE_UNLOCKED.to_string(),
                size: kmem_stats.vmo_discardable_unlocked_bytes.unwrap_or(0),
            },
            // This bucket accounts for compressed memory.
            Bucket {
                name: ZRAM_COMPRESSED_BYTES.to_string(),
                size: kmem_stats_compression.compressed_storage_bytes.unwrap_or(0),
            },
        ]);
        Digest { buckets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Attribution, Principal, PrincipalDescription, PrincipalIdentifier, PrincipalType, Resource,
        ResourceReference,
    };

    use fidl_fuchsia_memory_attribution_plugin as fplugin;

    fn get_attribution_data() -> AttributionData {
        AttributionData {
            principals_vec: vec![Principal {
                identifier: PrincipalIdentifier(1),
                description: PrincipalDescription::Component("principal".to_owned()),
                principal_type: PrincipalType::Runnable,
                parent: None,
            }],
            resources_vec: vec![
                Resource {
                    koid: 10,
                    name_index: 0,
                    resource_type: fplugin::ResourceType::Vmo(fplugin::Vmo {
                        parent: None,
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    }),
                },
                Resource {
                    koid: 20,
                    name_index: 1,
                    resource_type: fplugin::ResourceType::Vmo(fplugin::Vmo {
                        parent: None,
                        private_committed_bytes: Some(1024),
                        private_populated_bytes: Some(2048),
                        scaled_committed_bytes: Some(1024),
                        scaled_populated_bytes: Some(2048),
                        total_committed_bytes: Some(1024),
                        total_populated_bytes: Some(2048),
                        ..Default::default()
                    }),
                },
                Resource {
                    koid: 30,
                    name_index: 1,
                    resource_type: fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![10, 20]),
                        ..Default::default()
                    }),
                },
            ],
            resource_names: vec!["resource".to_owned(), "matched".to_owned()],
            attributions: vec![Attribution {
                source: PrincipalIdentifier(1),
                subject: PrincipalIdentifier(1),
                resources: vec![ResourceReference::KernelObject(10)],
            }],
        }
    }

    fn get_kernel_stats() -> (fkernel::MemoryStats, fkernel::MemoryStatsCompression) {
        (
            fkernel::MemoryStats {
                total_bytes: Some(1),
                free_bytes: Some(2),
                wired_bytes: Some(3),
                total_heap_bytes: Some(4),
                free_heap_bytes: Some(5),
                vmo_bytes: Some(10000),
                mmu_overhead_bytes: Some(7),
                ipc_bytes: Some(8),
                other_bytes: Some(9),
                free_loaned_bytes: Some(10),
                cache_bytes: Some(11),
                slab_bytes: Some(12),
                zram_bytes: Some(13),
                vmo_reclaim_total_bytes: Some(14),
                vmo_reclaim_newest_bytes: Some(15),
                vmo_reclaim_oldest_bytes: Some(16),
                vmo_reclaim_disabled_bytes: Some(17),
                vmo_discardable_locked_bytes: Some(18),
                vmo_discardable_unlocked_bytes: Some(19),
                ..Default::default()
            },
            fkernel::MemoryStatsCompression {
                uncompressed_storage_bytes: Some(20),
                compressed_storage_bytes: Some(21),
                compressed_fragmentation_bytes: Some(22),
                compression_time: Some(23),
                decompression_time: Some(24),
                total_page_compression_attempts: Some(25),
                failed_page_compression_attempts: Some(26),
                total_page_decompressions: Some(27),
                compressed_page_evictions: Some(28),
                eager_page_compressions: Some(29),
                memory_pressure_page_compressions: Some(30),
                critical_memory_page_compressions: Some(31),
                pages_decompressed_unit_ns: Some(32),
                pages_decompressed_within_log_time: Some([40, 41, 42, 43, 44, 45, 46, 47]),
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_digest_no_definitions() {
        let (kernel_stats, kernel_stats_compression) = get_kernel_stats();
        let digest =
            Digest::new(&get_attribution_data(), kernel_stats, kernel_stats_compression, &vec![]);
        let expected_buckets = vec![
            Bucket { name: UNDIGESTED.to_string(), size: 2048 }, // The two VMOs are unmatched, 1024 + 1024
            Bucket { name: ORPHANED.to_string(), size: 10000 }, // No matched VMOs => kernel's VMO bytes
            Bucket { name: KERNEL.to_string(), size: 31 }, // wired + heap + mmu + ipc + other => 3 + 4 + 7 + 8 + 9 = 31
            Bucket { name: FREE.to_string(), size: 2 },
            Bucket { name: PAGER_TOTAL.to_string(), size: 14 },
            Bucket { name: PAGER_NEWEST.to_string(), size: 15 },
            Bucket { name: PAGER_OLDEST.to_string(), size: 16 },
            Bucket { name: DISCARDABLE_LOCKED.to_string(), size: 18 },
            Bucket { name: DISCARDABLE_UNLOCKED.to_string(), size: 19 },
            Bucket { name: ZRAM_COMPRESSED_BYTES.to_string(), size: 21 },
        ];

        assert_eq!(digest.buckets, expected_buckets);
    }

    #[test]
    fn test_digest_with_matching_vmo() -> Result<(), anyhow::Error> {
        let (kernel_stats, kernel_stats_compression) = get_kernel_stats();
        let digest = Digest::new(
            &get_attribution_data(),
            kernel_stats,
            kernel_stats_compression,
            &vec![BucketDefinition {
                name: "matched".to_string(),
                process: None,
                vmo: Some(Regex::new("matched")?),
            }],
        );
        let expected_buckets = vec![
            Bucket { name: "matched".to_string(), size: 1024 }, // One VMO is matched, the other is not
            Bucket { name: UNDIGESTED.to_string(), size: 1024 }, // One unmatched VMO
            Bucket { name: ORPHANED.to_string(), size: 8976 }, // One matched VMO => 10000 - 1024 = 8976
            Bucket { name: KERNEL.to_string(), size: 31 }, // wired + heap + mmu + ipc + other => 3 + 4 + 7 + 8 + 9 = 31
            Bucket { name: FREE.to_string(), size: 2 },
            Bucket { name: PAGER_TOTAL.to_string(), size: 14 },
            Bucket { name: PAGER_NEWEST.to_string(), size: 15 },
            Bucket { name: PAGER_OLDEST.to_string(), size: 16 },
            Bucket { name: DISCARDABLE_LOCKED.to_string(), size: 18 },
            Bucket { name: DISCARDABLE_UNLOCKED.to_string(), size: 19 },
            Bucket { name: ZRAM_COMPRESSED_BYTES.to_string(), size: 21 },
        ];

        assert_eq!(digest.buckets, expected_buckets);
        Ok(())
    }

    #[test]
    fn test_digest_with_matching_process() -> Result<(), anyhow::Error> {
        let (kernel_stats, kernel_stats_compression) = get_kernel_stats();
        let digest = Digest::new(
            &get_attribution_data(),
            kernel_stats,
            kernel_stats_compression,
            &vec![BucketDefinition {
                name: "matched".to_string(),
                process: Some(Regex::new("matched")?),
                vmo: None,
            }],
        );
        let expected_buckets = vec![
            Bucket { name: "matched".to_string(), size: 2048 }, // Both VMOs are matched => 1024 + 1024 = 2048
            Bucket { name: UNDIGESTED.to_string(), size: 0 },   // No unmatched VMO
            Bucket { name: ORPHANED.to_string(), size: 7952 }, // Two matched VMO => 10000 - 1024 - 1024 = 7952
            Bucket { name: KERNEL.to_string(), size: 31 }, // wired + heap + mmu + ipc + other => 3 + 4 + 7 + 8 + 9 = 31
            Bucket { name: FREE.to_string(), size: 2 },
            Bucket { name: PAGER_TOTAL.to_string(), size: 14 },
            Bucket { name: PAGER_NEWEST.to_string(), size: 15 },
            Bucket { name: PAGER_OLDEST.to_string(), size: 16 },
            Bucket { name: DISCARDABLE_LOCKED.to_string(), size: 18 },
            Bucket { name: DISCARDABLE_UNLOCKED.to_string(), size: 19 },
            Bucket { name: ZRAM_COMPRESSED_BYTES.to_string(), size: 21 },
        ];

        assert_eq!(digest.buckets, expected_buckets);
        Ok(())
    }

    #[test]
    fn test_digest_with_matching_process_and_vmo() -> Result<(), anyhow::Error> {
        let (kernel_stats, kernel_stats_compression) = get_kernel_stats();
        let digest = Digest::new(
            &get_attribution_data(),
            kernel_stats,
            kernel_stats_compression,
            &vec![BucketDefinition {
                name: "matched".to_string(),
                process: Some(Regex::new("matched")?),
                vmo: Some(Regex::new("matched")?),
            }],
        );
        let expected_buckets = vec![
            Bucket { name: "matched".to_string(), size: 1024 }, // One VMO is matched, the other is not
            Bucket { name: UNDIGESTED.to_string(), size: 1024 }, // One unmatched VMO
            Bucket { name: ORPHANED.to_string(), size: 8976 }, // One matched VMO => 10000 - 1024 = 8976
            Bucket { name: KERNEL.to_string(), size: 31 }, // wired + heap + mmu + ipc + other => 3 + 4 + 7 + 8 + 9 = 31
            Bucket { name: FREE.to_string(), size: 2 },
            Bucket { name: PAGER_TOTAL.to_string(), size: 14 },
            Bucket { name: PAGER_NEWEST.to_string(), size: 15 },
            Bucket { name: PAGER_OLDEST.to_string(), size: 16 },
            Bucket { name: DISCARDABLE_LOCKED.to_string(), size: 18 },
            Bucket { name: DISCARDABLE_UNLOCKED.to_string(), size: 19 },
            Bucket { name: ZRAM_COMPRESSED_BYTES.to_string(), size: 21 },
        ];

        assert_eq!(digest.buckets, expected_buckets);
        Ok(())
    }
}
