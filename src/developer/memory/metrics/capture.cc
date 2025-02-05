// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/capture.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/channel.h>
#include <lib/zx/job.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <optional>

#include <task-utils/walker.h>

#include "src/developer/memory/metrics/capture_strategy.h"

namespace memory {

class OSImpl : public OS, public TaskEnumerator {
 private:
  zx_status_t GetKernelStats(fidl::WireSyncClient<fuchsia_kernel::Stats>* stats) override {
    auto client_end = component::Connect<fuchsia_kernel::Stats>();
    if (!client_end.is_ok()) {
      return client_end.status_value();
    }

    *stats = fidl::WireSyncClient(std::move(*client_end));
    return ZX_OK;
  }

  zx_handle_t ProcessSelf() override { return zx_process_self(); }
  zx_time_t GetMonotonic() override { return zx_clock_get_monotonic(); }

  zx_status_t GetProcesses(
      fit::function<zx_status_t(int, zx::handle, zx_koid_t, zx_koid_t)> cb) override {
    TRACE_DURATION("memory_metrics", "Capture::GetProcesses");
    cb_ = [cb = std::move(cb)](int depth, zx_handle_t handle, zx_koid_t koid,
                               zx_koid_t parent_koid) {
      // Each time |WalkJobTree| iterates over a new process, it closes the previously sent handle.
      // However, we want to keep the handles around to avoid re-walking the tree for filtering. To
      // do that, we use |zx_handle_duplicate| to have a handle that survives the callback call.
      // Using a |zx::handle| object ensures the duplicated handle will be closed upon the object
      // destruction.
      zx::handle new_handle;
      zx_status_t s =
          zx_handle_duplicate(handle, ZX_RIGHT_SAME_RIGHTS, new_handle.reset_and_get_address());
      if (s != ZX_OK) {
        return s;
      }
      return cb(depth, std::move(new_handle), koid, parent_koid);
    };
    auto client_end = component::Connect<fuchsia_kernel::RootJobForInspect>();
    if (!client_end.is_ok()) {
      return client_end.status_value();
    }

    auto result = fidl::WireCall(*client_end)->Get();
    if (result.status() != ZX_OK) {
      return result.status();
    }

    return WalkJobTree(result->job.get());
  }

  zx_status_t OnProcess(int depth, zx_handle_t handle, zx_koid_t koid,
                        zx_koid_t parent_koid) override {
    return cb_(depth, handle, koid, parent_koid);
  }

  zx_status_t GetProperty(zx_handle_t handle, uint32_t property, void* value,
                          size_t name_len) override {
    return zx_object_get_property(handle, property, value, name_len);
  }

  zx_status_t GetInfo(zx_handle_t handle, uint32_t topic, void* buffer, size_t buffer_size,
                      size_t* actual, size_t* avail) override {
    TRACE_DURATION("memory_metrics", "OSImpl::GetInfo", "topic", topic, "buffer_size", buffer_size);
    return zx_object_get_info(handle, topic, buffer, buffer_size, actual, avail);
  }

  zx_status_t GetKernelMemoryStats(const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
                                   zx_info_kmem_stats_t& kmem) override {
    TRACE_DURATION("memory_metrics", "Capture::GetKernelMemoryStats");
    if (!stats_client.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
    auto result = stats_client->GetMemoryStats();
    if (result.status() != ZX_OK) {
      return result.status();
    }
    const auto& stats = result->stats;
    kmem.total_bytes = stats.total_bytes();
    kmem.free_bytes = stats.free_bytes();
    kmem.wired_bytes = stats.wired_bytes();
    kmem.total_heap_bytes = stats.total_heap_bytes();
    kmem.free_heap_bytes = stats.free_heap_bytes();
    kmem.vmo_bytes = stats.vmo_bytes();
    kmem.mmu_overhead_bytes = stats.mmu_overhead_bytes();
    kmem.ipc_bytes = stats.ipc_bytes();
    kmem.other_bytes = stats.other_bytes();
    return ZX_OK;
  }

  zx_status_t GetKernelMemoryStatsExtended(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_extended_t& kmem_ext, zx_info_kmem_stats_t* kmem) override {
    TRACE_DURATION("memory_metrics", "Capture::GetKernelMemoryStatsExtended");
    if (!stats_client.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
    auto result = stats_client->GetMemoryStatsExtended();
    if (result.status() != ZX_OK) {
      return result.status();
    }
    const auto& stats = result->stats;
    kmem_ext.total_bytes = stats.total_bytes();
    kmem_ext.free_bytes = stats.free_bytes();
    kmem_ext.wired_bytes = stats.wired_bytes();
    kmem_ext.total_heap_bytes = stats.total_heap_bytes();
    kmem_ext.free_heap_bytes = stats.free_heap_bytes();
    kmem_ext.vmo_bytes = stats.vmo_bytes();
    kmem_ext.vmo_pager_total_bytes = stats.vmo_pager_total_bytes();
    kmem_ext.vmo_pager_newest_bytes = stats.vmo_pager_newest_bytes();
    kmem_ext.vmo_pager_oldest_bytes = stats.vmo_pager_oldest_bytes();
    kmem_ext.vmo_discardable_locked_bytes = stats.vmo_discardable_locked_bytes();
    kmem_ext.vmo_discardable_unlocked_bytes = stats.vmo_discardable_unlocked_bytes();
    kmem_ext.mmu_overhead_bytes = stats.mmu_overhead_bytes();
    kmem_ext.ipc_bytes = stats.ipc_bytes();
    kmem_ext.other_bytes = stats.other_bytes();

    // Copy over shared fields from kmem_ext to kmem, if provided.
    if (kmem) {
      kmem->total_bytes = kmem_ext.total_bytes;
      kmem->free_bytes = kmem_ext.free_bytes;
      kmem->wired_bytes = kmem_ext.wired_bytes;
      kmem->total_heap_bytes = kmem_ext.total_heap_bytes;
      kmem->free_heap_bytes = kmem_ext.free_heap_bytes;
      kmem->vmo_bytes = kmem_ext.vmo_bytes;
      kmem->mmu_overhead_bytes = kmem_ext.mmu_overhead_bytes;
      kmem->ipc_bytes = kmem_ext.ipc_bytes;
      kmem->other_bytes = kmem_ext.other_bytes;
    }
    return ZX_OK;
  }

  zx_status_t GetKernelMemoryStatsCompression(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_compression_t& kmem_compression) override {
    TRACE_DURATION("memory_metrics", "Capture::GetKernelMemoryStatsCompression");
    if (!stats_client.is_valid()) {
      return ZX_ERR_BAD_STATE;
    }
    auto result = stats_client->GetMemoryStatsCompression();
    if (result.status() != ZX_OK) {
      return result.status();
    }
    kmem_compression.uncompressed_storage_bytes = result->uncompressed_storage_bytes();
    kmem_compression.compressed_storage_bytes = result->compressed_storage_bytes();
    kmem_compression.compressed_fragmentation_bytes = result->compressed_fragmentation_bytes();
    kmem_compression.compression_time = result->compression_time();
    kmem_compression.decompression_time = result->decompression_time();
    kmem_compression.total_page_compression_attempts = result->total_page_compression_attempts();
    kmem_compression.failed_page_compression_attempts = result->failed_page_compression_attempts();
    kmem_compression.total_page_decompressions = result->total_page_decompressions();
    kmem_compression.compressed_page_evictions = result->compressed_page_evictions();
    kmem_compression.eager_page_compressions = result->eager_page_compressions();
    kmem_compression.memory_pressure_page_compressions =
        result->memory_pressure_page_compressions();
    kmem_compression.critical_memory_page_compressions =
        result->critical_memory_page_compressions();
    kmem_compression.pages_decompressed_unit_ns = result->pages_decompressed_unit_ns();
    std::copy(result->pages_decompressed_within_log_time().begin(),
              result->pages_decompressed_within_log_time().end(),
              std::begin(kmem_compression.pages_decompressed_within_log_time));
    return ZX_OK;
  }

  bool has_on_process() const final { return true; }

  fit::function<zx_status_t(int /* depth */, zx_handle_t /* handle */, zx_koid_t /* koid */,
                            zx_koid_t /* parent_koid */)>
      cb_;
};
// Returns an OS implementation querying Zircon Kernel.
std::unique_ptr<OS> CreateDefaultOS() { return std::make_unique<OSImpl>(); }

const std::vector<std::string> Capture::kDefaultRootedVmoNames = {
    "SysmemContiguousPool", "SysmemAmlogicProtectedPool", "Sysmem-core"};

CaptureMaker::CaptureMaker(fidl::WireSyncClient<fuchsia_kernel::Stats> stats_client,
                           std::unique_ptr<OS> os)
    : stats_client_(std::move(stats_client)), os_(std::move(os)) {
  FX_CHECK(os_);
}

zx_status_t CaptureMaker::GetCapture(Capture* capture, CaptureLevel level,
                                     const std::vector<std::string>& rooted_vmo_names) {
  TRACE_DURATION("memory_metrics", "Capture::GetCapture");
  capture->time_ = os_->GetMonotonic();

  // Capture level KMEM only queries ZX_INFO_KMEM_STATS, as opposed to ZX_INFO_KMEM_STATS_EXTENDED
  // which queries a more detailed set of kernel metrics. KMEM capture level is used to poll the
  // free memory level every 10s in order to keep the highwater digest updated, so a lightweight
  // syscall is preferable.
  if (level == CaptureLevel::KMEM) {
    return os_->GetKernelMemoryStats(stats_client_, capture->kmem_);
  }

  // ZX_INFO_KMEM_STATS_EXTENDED is more expensive to collect than ZX_INFO_KMEM_STATS, so only
  // query it for the more detailed capture levels. Use kmem_extended_ to populate the shared
  // fields in kmem_ (kmem_extended_ is a superset of kmem_), avoiding the need for a redundant
  // syscall.
  zx_status_t err = os_->GetKernelMemoryStatsExtended(
      stats_client_, capture->kmem_extended_.emplace(), &capture->kmem_);
  if (err != ZX_OK) {
    return err;
  }

  // Some Fuchsia systems use ZRAM, ie. compressed RAM. ZX_INFO_KMEM_STATS_COMPRESSION retrieves
  // information about this compression, so we can get an accurate view of the actual physical
  // memory used.
  err = os_->GetKernelMemoryStatsCompression(stats_client_, capture->kmem_compression_.emplace());
  // Assume compression is disabled when there is no storage.
  if (!capture->kmem_compression_->compressed_storage_bytes) {
    capture->kmem_compression_ = std::nullopt;
  }
  if (err != ZX_OK) {
    return err;
  }

  StarnixCaptureStrategy strategy;
  // We don't have a guarantee on the iteration order of GetProcesses. To be able to filter jobs
  // correctly based on the name of their processes, we need to go through all processes first. We
  // extract the process handle and keep it to avoid walking the process tree a second time.
  err = os_->GetProcesses(
      [this, &strategy](int depth, zx::handle handle, zx_koid_t koid, zx_koid_t parent_koid) {
        TRACE_DURATION("memory_metrics", "Capture::GetProcesses::Callback");
        Process process{.koid = koid, .job = parent_koid};

        zx_status_t s = os_->GetProperty(handle.get(), ZX_PROP_NAME, process.name, ZX_MAX_NAME_LEN);
        if (s != ZX_OK) {
          return s == ZX_ERR_BAD_STATE ? ZX_OK : s;
        }

        s = strategy.OnNewProcess(*os_, std::move(process), std::move(handle));
        return s == ZX_ERR_BAD_STATE ? ZX_OK : s;
      });

  auto result = StarnixCaptureStrategy::Finalize(*os_, std::move(strategy));
  if (result.is_error()) {
    return result.error_value();
  }
  auto& [koid_to_process, koid_to_vmo] = result.value();
  capture->koid_to_process_ = std::move(koid_to_process);
  capture->koid_to_vmo_ = std::move(koid_to_vmo);

  TRACE_DURATION_BEGIN("memory_metrics", "Capture::GetProcesses::ReallocateDescendents");
  ReallocateDescendents(rooted_vmo_names, capture->koid_to_vmo_);
  TRACE_DURATION_END("memory_metrics", "Capture::GetProcesses::ReallocateDescendents");
  return err;
}

fit::result<zx_status_t, CaptureMaker> CaptureMaker::Create(std::unique_ptr<OS> os) {
  TRACE_DURATION("memory_metrics", "Capture::GetCaptureState");
  fidl::WireSyncClient<fuchsia_kernel::Stats> stats_client;

  zx_status_t err = os->GetKernelStats(&stats_client);
  if (err != ZX_OK) {
    return fit::error(err);
  }

  return fit::ok(CaptureMaker{std::move(stats_client), std::move(os)});
}

// Descendents of this vmo will have their allocated_bytes treated as an allocation of their
// immediate parent. This supports a usage pattern where a potentially large allocation is done
// and then slices are given to read / write children. In this case the children have no
// committed_bytes of their own. For accounting purposes it gives more clarity to push the
// committed bytes to the lowest points in the tree, where the vmo names give more specific
// meanings.
void CaptureMaker::ReallocateDescendents(Vmo& parent,
                                         std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo) {
  for (auto& child_koid : parent.children) {
    auto& child = koid_to_vmo.at(child_koid);
    if (child.parent_koid == parent.koid) {
      uint64_t reallocated_bytes = std::min(parent.committed_bytes.integral, child.allocated_bytes);
      parent.committed_bytes.integral -= reallocated_bytes;
      child.committed_bytes.integral = reallocated_bytes;
      ReallocateDescendents(child, koid_to_vmo);
    }
  }
}

// See the above description of ReallocateDescendents(zx_koid_t) for the specific behavior for each
// vmo that has a name listed in rooted_vmo_names.
void CaptureMaker::ReallocateDescendents(const std::vector<std::string>& rooted_vmo_names,
                                         std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo) {
  TRACE_DURATION("memory_metrics", "Capture::ReallocateDescendents");
  std::vector<zx_koid_t> root_vmos;
  for (auto const& [_, child] : koid_to_vmo) {
    if (child.parent_koid == ZX_KOID_INVALID) {
      root_vmos.push_back(child.koid);
      continue;
    }
    auto parent_it = koid_to_vmo.find(child.parent_koid);
    if (parent_it == koid_to_vmo.end()) {
      continue;
    }
    parent_it->second.children.push_back(child.koid);
  }
  for (auto& vmo_koid : root_vmos) {
    auto& vmo = koid_to_vmo.at(vmo_koid);
    for (const auto& vmo_name : rooted_vmo_names) {
      if (vmo.name != vmo_name) {
        continue;
      }
      ReallocateDescendents(vmo, koid_to_vmo);
    }
  }
}
}  // namespace memory
