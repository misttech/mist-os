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
        allocated_bytes(v.size_bytes) {
    strncpy(name, v.name, sizeof(name));
  }
  zx_koid_t koid;
  char name[ZX_MAX_NAME_LEN];
  zx_koid_t parent_koid;
  uint64_t committed_bytes;
  uint64_t allocated_bytes;
  std::vector<zx_koid_t> children;
};

enum class CaptureLevel { KMEM, PROCESS, VMO };

struct CaptureState {
  fidl::WireSyncClient<fuchsia_kernel::Stats> stats_client;
  zx_koid_t self_koid;
};

// OS is an abstract interface to Zircon OS calls.
class OS {
 public:
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
      zx_info_kmem_stats_t* kmem) = 0;
  virtual zx_status_t GetKernelMemoryStatsExtended(
      const fidl::WireSyncClient<fuchsia_kernel::Stats>& stats_client,
      zx_info_kmem_stats_extended_t* kmem_ext, zx_info_kmem_stats_t* kmem = nullptr) = 0;
};

// GetInfoVector executes an OS::GetInfo call that outputs a list of element inside |buffer|,
// ensuring that |buffer| is big enough to receive the list of elements. |GetInfoVector| returns the
// status of the call and the number of elements effectively returned.
template <typename T>
zx::result<size_t> GetInfoVector(OS& os, zx_handle_t handle, uint32_t topic,
                                 std::vector<T>& buffer) {
  size_t num_entries = 0, available_entries = 0;
  zx_status_t s = os.GetInfo(handle, topic, buffer.data(), buffer.size() * sizeof(T), &num_entries,
                             &available_entries);
  if (s != ZX_OK) {
    return zx::error(s);
  } else if (num_entries == available_entries) {
    return zx::ok(num_entries);
  }
  buffer.resize(available_entries);
  s = os.GetInfo(handle, topic, buffer.data(), buffer.size() * sizeof(T), &num_entries,
                 &available_entries);
  if (s != ZX_OK) {
    return zx::error(s);
  }
  return zx::ok(num_entries);
}

// A CaptureStrategy holds the strategy for getting VMO information out of a process tree.
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
  static zx_status_t GetCaptureState(CaptureState* state);

  // Initialize a Capture instance. Be sure to call GetCapture prior to passing
  // the Capture instance to other systems (such as a Digest).
  //
  // Tip: This may require capabilities (in your .cml file) for
  //   fuchsia.kernel.RootJobForInspect and fuchsia.kernel.Stats, e.g.:
  //   "use": {
  //       "protocol": [
  //           "fuchsia.kernel.RootJobForInspect",
  //           "fuchsia.kernel.Stats",
  //           ...
  //
  // GetCapture takes ownership of the provided |strategy|, as it stateful and should not be reused
  // between calls.
  static zx_status_t GetCapture(
      Capture* capture, const CaptureState& state, CaptureLevel level,
      std::unique_ptr<CaptureStrategy> strategy,
      const std::vector<std::string>& rooted_vmo_names = kDefaultRootedVmoNames);

  zx_time_t time() const { return time_; }
  const zx_info_kmem_stats_t& kmem() const { return kmem_; }
  const zx_info_kmem_stats_extended_t& kmem_extended() const { return kmem_extended_; }

  const std::unordered_map<zx_koid_t, Process>& koid_to_process() const { return koid_to_process_; }

  const std::unordered_map<zx_koid_t, Vmo>& koid_to_vmo() const { return koid_to_vmo_; }

  const Process& process_for_koid(zx_koid_t koid) const { return koid_to_process_.at(koid); }

  const Vmo& vmo_for_koid(zx_koid_t koid) const { return koid_to_vmo_.at(koid); }

 private:
  static zx_status_t GetCaptureState(CaptureState* state, OS& os);
  static zx_status_t GetCapture(Capture* capture, const CaptureState& state, CaptureLevel level,
                                std::unique_ptr<CaptureStrategy> strategy, OS& os,
                                const std::vector<std::string>& rooted_vmo_names);
  void ReallocateDescendents(const std::vector<std::string>& rooted_vmo_names);
  void ReallocateDescendents(Vmo* parent);

  zx_time_t time_;
  zx_info_kmem_stats_t kmem_ = {};
  zx_info_kmem_stats_extended_t kmem_extended_ = {};
  std::unordered_map<zx_koid_t, Process> koid_to_process_;
  std::unordered_map<zx_koid_t, Vmo> koid_to_vmo_;
  std::vector<zx_koid_t> root_vmos_;

  class ProcessGetter;
  friend class TestUtils;
};

}  // namespace memory

#endif  // SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_H_
