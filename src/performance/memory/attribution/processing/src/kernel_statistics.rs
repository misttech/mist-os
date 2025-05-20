// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Serialize;
use {fidl_fuchsia_kernel as fkernel, fidl_fuchsia_memory_attribution_plugin as fplugin};

#[derive(Serialize)]
#[serde(remote = "fkernel::MemoryStats")]
pub struct MemoryStatsDef {
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub wired_bytes: Option<u64>,
    pub total_heap_bytes: Option<u64>,
    pub free_heap_bytes: Option<u64>,
    pub vmo_bytes: Option<u64>,
    pub mmu_overhead_bytes: Option<u64>,
    pub ipc_bytes: Option<u64>,
    pub other_bytes: Option<u64>,
    pub free_loaned_bytes: Option<u64>,
    pub cache_bytes: Option<u64>,
    pub slab_bytes: Option<u64>,
    pub zram_bytes: Option<u64>,
    pub vmo_reclaim_total_bytes: Option<u64>,
    pub vmo_reclaim_newest_bytes: Option<u64>,
    pub vmo_reclaim_oldest_bytes: Option<u64>,
    pub vmo_reclaim_disabled_bytes: Option<u64>,
    pub vmo_discardable_locked_bytes: Option<u64>,
    pub vmo_discardable_unlocked_bytes: Option<u64>,
    #[doc(hidden)]
    #[serde(skip_serializing)]
    pub __source_breaking: fidl::marker::SourceBreaking,
}
#[derive(Serialize)]
#[serde(remote = "fkernel::MemoryStatsCompression")]
pub struct MemoryStatsCompressionDef {
    pub uncompressed_storage_bytes: Option<u64>,
    pub compressed_storage_bytes: Option<u64>,
    pub compressed_fragmentation_bytes: Option<u64>,
    pub compression_time: Option<i64>,
    pub decompression_time: Option<i64>,
    pub total_page_compression_attempts: Option<u64>,
    pub failed_page_compression_attempts: Option<u64>,
    pub total_page_decompressions: Option<u64>,
    pub compressed_page_evictions: Option<u64>,
    pub eager_page_compressions: Option<u64>,
    pub memory_pressure_page_compressions: Option<u64>,
    pub critical_memory_page_compressions: Option<u64>,
    pub pages_decompressed_unit_ns: Option<u64>,
    pub pages_decompressed_within_log_time: Option<[u64; 8]>,
    #[doc(hidden)]
    #[serde(skip_serializing)]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Default, PartialEq, Debug, Clone, Serialize)]
pub struct KernelStatistics {
    #[serde(with = "MemoryStatsDef")]
    pub memory_statistics: fkernel::MemoryStats,
    #[serde(with = "MemoryStatsCompressionDef")]
    pub compression_statistics: fkernel::MemoryStatsCompression,
}

impl From<fplugin::KernelStatistics> for KernelStatistics {
    fn from(value: fplugin::KernelStatistics) -> KernelStatistics {
        KernelStatistics {
            memory_statistics: value.memory_stats.unwrap(),
            compression_statistics: value.compression_stats.unwrap(),
        }
    }
}

impl Into<fplugin::KernelStatistics> for KernelStatistics {
    fn into(self) -> fplugin::KernelStatistics {
        fplugin::KernelStatistics {
            memory_stats: Some(self.memory_statistics),
            compression_stats: Some(self.compression_statistics),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_convert() {
        let fplugin_kernel_statistics = fplugin::KernelStatistics {
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
        };

        let kernel_statistics: KernelStatistics = fplugin_kernel_statistics.clone().into();

        assert_eq!(kernel_statistics.memory_statistics.total_bytes, Some(1));
        assert_eq!(kernel_statistics.memory_statistics.free_bytes, Some(2));

        assert_eq!(kernel_statistics.compression_statistics.uncompressed_storage_bytes, Some(15));

        assert_eq!(fplugin_kernel_statistics, kernel_statistics.into());
    }
}
