// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, KernelStats};
use crate::vfs::{
    DynamicFile, DynamicFileBuf, DynamicFileSource, FileSystemHandle, FsNodeHandle, FsNodeInfo,
    FsNodeOps,
};
use starnix_logging::log_error;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, mode};

use std::sync::Arc;

pub fn meminfo_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        MeminfoFile::new_node(&current_task.kernel().stats),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}

#[derive(Clone)]
struct MeminfoFile {
    kernel_stats: Arc<KernelStats>,
}

impl MeminfoFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}

impl DynamicFileSource for MeminfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let stats = self.kernel_stats.get();
        let memory_stats =
            stats.get_memory_stats_extended(zx::MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("FIDL error getting memory stats: {e}");
                errno!(EIO)
            })?;
        let compression_stats =
            stats.get_memory_stats_compression(zx::MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("FIDL error getting memory compression stats: {e}");
                errno!(EIO)
            })?;

        let mem_total = memory_stats.total_bytes.unwrap_or_default() / 1024;
        let mem_free = memory_stats.free_bytes.unwrap_or_default() / 1024;
        let mem_available = (memory_stats.free_bytes.unwrap_or_default()
            + memory_stats.vmo_discardable_unlocked_bytes.unwrap_or_default())
            / 1024;

        let swap_used = compression_stats.uncompressed_storage_bytes.unwrap_or_default() / 1024;
        // Fuchsia doesn't have a limit on the size of its swap file, so we just pretend that
        // we're willing to grow the swap by half the amount of free memory.
        let swap_free = mem_free / 2;
        let swap_total = swap_used + swap_free;

        writeln!(sink, "MemTotal:       {:8} kB", mem_total)?;
        writeln!(sink, "MemFree:        {:8} kB", mem_free)?;
        writeln!(sink, "MemAvailable:   {:8} kB", mem_available)?;
        writeln!(sink, "SwapTotal:      {:8} kB", swap_total)?;
        writeln!(sink, "SwapFree:       {:8} kB", swap_free)?;
        Ok(())
    }
}
