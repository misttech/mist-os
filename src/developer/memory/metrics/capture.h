// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_H_
#define SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_H_

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/zx/handle.h>
#include <lib/zx/time.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

class TestMonitor;

namespace memory {
struct Process {
  zx_koid_t koid;
  zx_koid_t job;
  char name[ZX_MAX_NAME_LEN];
  std::vector<zx_koid_t> vmos;
};

struct Vmo {
  explicit Vmo(const zx_info_vmo_t& v)
      : koid(v.koid),
        parent_koid(v.parent_koid),
        committed_bytes(v.committed_bytes),
        populated_bytes(v.populated_bytes),
        allocated_bytes(v.size_bytes) {
    strncpy(name, v.name, sizeof(name));
  }
  zx_koid_t koid;
  char name[ZX_MAX_NAME_LEN];
  zx_koid_t parent_koid;
  uint64_t committed_bytes;
  uint64_t populated_bytes;
  uint64_t allocated_bytes;
  std::vector<zx_koid_t> children;
};

enum class CaptureLevel : uint8_t { KMEM, KMEM_EXTENDED, PROCESS, VMO };

// OS is an abstract interface to Zircon OS calls.
class OS {
 public:
  virtual ~OS() = default;
  virtual zx_status_t GetKernelStats(fidl::WireSyncClient<fuchsia_kernel::Stats>* stats_client) = 0;
  virtual zx_handle_t ProcessSelf() = 0;
  virtual zx_time_t GetMonotonic() = 0;
  virtual zx_status_t GetProcesses(
      fit::function<zx_status_t(int /* depth */, zx::handle /* handle */, zx_koid_t /* koid */,
                                zx_koid_t /* parent_koid */)>
          cb) = 0;
  virtual zx_status_t GetProperty(zx_handle_t handle, uint32_t property, void* value,
                                  size_t name_len) = 0;
  virtual zx_status_t GetInfo(zx_handle_t handle, uint32_t topic, void* buffer, size_t buffer_size,
                              size_t* actual, size_t* avail) = 0;

  virtual zx_status_t GetKernelMemoryStats(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_t& kmem) = 0;
  virtual zx_status_t GetKernelMemoryStatsExtended(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_extended_t& kmem_ext, zx_info_kmem_stats_t* kmem) = 0;
  virtual zx_status_t GetKernelMemoryStatsCompression(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_compression_t& kmem_compression) = 0;
};

// Returns an OS implementation querying Zircon Kernel.
std::unique_ptr<OS> CreateDefaultOS();

// Extracts VMO information out of a process tree.
// TODO(b/366157407): Remove CaptureStrategy abstraction.
class CaptureStrategy {
 public:
  virtual ~CaptureStrategy() {}

  // For a given capture, |OnNewProcess| is called first for all processes on the system, before
  // the call to |Finalize|.
  virtual zx_status_t OnNewProcess(OS& os, Process process, zx::handle process_handle) = 0;

  // Finalize is called once after all calls to |OnNewProcess| are done.
  virtual zx::result<
      std::tuple<std::unordered_map<zx_koid_t, Process>, std::unordered_map<zx_koid_t, Vmo>>>
  Finalize(OS& os) = 0;
};

class Capture {
 public:
  static const std::vector<std::string> kDefaultRootedVmoNames;

  zx_time_t time() const { return time_; }
  const zx_info_kmem_stats_t& kmem() const { return kmem_; }
  const std::optional<zx_info_kmem_stats_extended_t>& kmem_extended() const {
    return kmem_extended_;
  }
  const std::optional<zx_info_kmem_stats_compression_t>& kmem_compression() const {
    return kmem_compression_;
  }

  const std::unordered_map<zx_koid_t, Process>& koid_to_process() const { return koid_to_process_; }

  const std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo() const { return koid_to_vmo_; }

  const Process& process_for_koid(zx_koid_t koid) const { return koid_to_process_.at(koid); }

  const Vmo& vmo_for_koid(zx_koid_t koid) const { return koid_to_vmo_.at(koid); }

 private:
  zx_time_t time_;
  zx_info_kmem_stats_t kmem_ = {};
  std::optional<zx_info_kmem_stats_extended_t> kmem_extended_;
  std::optional<zx_info_kmem_stats_compression_t> kmem_compression_;
  std::unordered_map<zx_koid_t, Process> koid_to_process_;
  std::unordered_map<zx_koid_t, Vmo> koid_to_vmo_;
  std::vector<zx_koid_t> root_vmos_;

  friend class ::TestMonitor;
  friend class TestUtils;
  friend class CaptureMaker;
};

// Holds the necessary components required to create a |Capture|.
class CaptureMaker {
 public:
  static fit::result<zx_status_t, std::unique_ptr<CaptureMaker>> Create(std::unique_ptr<OS> os);

  zx_status_t GetCapture(
      Capture* capture, CaptureLevel level,
      const std::vector<std::string>& rooted_vmo_names = Capture::kDefaultRootedVmoNames);

 private:
  CaptureMaker(fidl::WireSyncClient<fuchsia_kernel::Stats> stats_client, std::unique_ptr<OS> os);
  static void ReallocateDescendents(Vmo& parent, std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo);
  static void ReallocateDescendents(const std::vector<std::string>& rooted_vmo_names,
                                    std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo);
  // zx_koid_t self_koid_;
  fidl::WireSyncClient<fuchsia_kernel::Stats> stats_client_;
  std::unique_ptr<OS> os_;

  friend class TestUtils;
};

}  // namespace memory

#endif  // SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_H_
