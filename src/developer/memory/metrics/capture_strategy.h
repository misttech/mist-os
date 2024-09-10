// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_STRATEGY_H_
#define SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_STRATEGY_H_

#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <tuple>
#include <unordered_set>

#include "lib/zx/result.h"
#include "src/developer/memory/metrics/capture.h"

namespace memory {

const char STARNIX_KERNEL_PROCESS_NAME[] = "starnix_kernel.cm";

// A simple CaptureStrategy that retrieves the list of VMOs for each process through the
// |ZX_INFO_PROCESS_VMOS| zx_object_info topic.
class BaseCaptureStrategy : public memory::CaptureStrategy {
 public:
  explicit BaseCaptureStrategy();

  zx_status_t OnNewProcess(OS& os, Process process, zx::handle process_handle) override;

  zx::result<std::tuple<std::unordered_map<zx_koid_t, Process>, std::unordered_map<zx_koid_t, Vmo>>>
  Finalize(OS& os) override;

 private:
  std::unordered_map<zx_koid_t, Process> koid_to_process_;
  std::unordered_map<zx_koid_t, Vmo> koid_to_vmo_;
  std::vector<zx_info_vmo_t> vmos_;
};

// When a job contains a process named |process_name|, assume all processes within that job are
// shared processes. This strategy attributes the VMOs of processes of these jobs as follows:
//  - Memory mapped to the private, restricted address space of a process is attributed to this
//  process;
//  - Memory mapped into the shared address space is attributed to the process |process_name|;
//  - Handles of unmapped VMOs are attributed to the process |process_name|;
//  - Handles of mapped VMOs are not taken into account (their mapping is).
// For all other processes, this delegates to |BaseCaptureStrategy|.
class StarnixCaptureStrategy : public memory::CaptureStrategy {
 public:
  explicit StarnixCaptureStrategy(std::string process_name = STARNIX_KERNEL_PROCESS_NAME);

  zx_status_t OnNewProcess(OS& os, Process process, zx::handle process_handle) override;

  zx::result<std::tuple<std::unordered_map<zx_koid_t, Process>, std::unordered_map<zx_koid_t, Vmo>>>
  Finalize(OS& os) override;

 private:
  std::unordered_map<zx_koid_t, zx::handle> process_handles_;
  std::unordered_map<zx_koid_t, Process> koid_to_process_;
  std::unordered_map<zx_koid_t, Vmo> koid_to_vmo_;

  struct StarnixJob {
    zx_koid_t kernel_koid;
    std::unordered_set<zx_koid_t> kernel_mapped_vmos;
    std::unordered_map<zx_koid_t, std::unordered_set<zx_koid_t>> process_mapped_vmos;
    std::unordered_set<zx_koid_t> unmapped_vmos;
    bool vmos_retrieved = false;
  };

  const std::string process_name_;

  std::vector<zx_info_maps_t> mappings_;
  std::unordered_map<zx_koid_t, StarnixJob> starnix_jobs_;
};

}  // namespace memory

#endif  // SRC_DEVELOPER_MEMORY_METRICS_CAPTURE_STRATEGY_H_
