// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/capture_strategy.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <algorithm>
#include <iterator>
#include <tuple>

#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/capture_strategy.h"

namespace memory {
BaseCaptureStrategy::BaseCaptureStrategy() {}

zx_status_t BaseCaptureStrategy::OnNewProcess(OS& os, Process process, zx::handle process_handle) {
  TRACE_DURATION_BEGIN("memory_metrics", "BaseCaptureStrategy::OnNewProcess::GetVMOs");
  auto result = GetInfoVector<zx_info_vmo_t>(os, process_handle.get(), ZX_INFO_PROCESS_VMOS, vmos_);
  // We don't want to show processes for which we don't have data (e.g. because they exited).
  if (result.is_error()) {
    return result.error_value();
  }
  size_t num_vmos = result.value();
  TRACE_DURATION_END("memory_metrics", "BaseCaptureStrategy::OnNewProcess::GetVMOs");

  TRACE_DURATION_BEGIN("memory_metrics", "BaseCaptureStrategy::OnNewProcess::UniqueProcessVMOs");
  std::unordered_map<zx_koid_t, const zx_info_vmo_t*> unique_vmos;
  unique_vmos.reserve(num_vmos);
  for (size_t i = 0; i < num_vmos; i++) {
    const auto* vmo_info = vmos_.data() + i;
    unique_vmos.try_emplace(vmo_info->koid, vmo_info);
  }
  TRACE_DURATION_END("memory_metrics", "BaseCaptureStrategy::OnNewProcess::UniqueProcessVMOs");

  TRACE_DURATION_BEGIN("memory_metrics", "BaseCaptureStrategy::OnNewProcess::UniqueVMOs");
  process.vmos.reserve(unique_vmos.size());
  for (const auto& [koid, vmo] : unique_vmos) {
    koid_to_vmo_.try_emplace(koid, *vmo);
    process.vmos.push_back(koid);
  }
  TRACE_DURATION_END("memory_metrics", "BaseCaptureStrategy::OnNewProcess::UniqueVMOs");

  koid_to_process_[process.koid] = std::move(process);
  return ZX_OK;
}

zx::result<std::tuple<std::unordered_map<zx_koid_t, Process>, std::unordered_map<zx_koid_t, Vmo>>>
BaseCaptureStrategy::Finalize(OS& os) {
  return zx::ok(std::make_tuple(std::move(koid_to_process_), std::move(koid_to_vmo_)));
}

StarnixCaptureStrategy::StarnixCaptureStrategy(std::string process_name)
    : process_name_(std::move(process_name)) {}

zx_status_t StarnixCaptureStrategy::OnNewProcess(OS& os, Process process,
                                                 zx::handle process_handle) {
  if (process_name_ == process.name) {
    starnix_jobs_[process.job].kernel_koid = process.koid;
  }
  process_handles_[process.koid] = std::move(process_handle);
  koid_to_process_[process.koid] = std::move(process);
  return ZX_OK;
}

zx::result<std::tuple<std::unordered_map<zx_koid_t, Process>, std::unordered_map<zx_koid_t, Vmo>>>
StarnixCaptureStrategy::Finalize(OS& os) {
  TRACE_DURATION("memory_metrics", "StarnixCaptureStrategy::Finalize");

  BaseCaptureStrategy base;

  // Capture the data for each process.
  for (auto& [_, process] : koid_to_process_) {
    auto starnix_proc = starnix_jobs_.find(process.job);
    if (starnix_proc == starnix_jobs_.end()) {
      zx_status_t s =
          base.OnNewProcess(os, std::move(process), std::move(process_handles_[process.koid]));
      // No error or a process-specific error (e.g.: the process exited), we continue.
      if (s != ZX_OK && s != ZX_ERR_BAD_STATE) {
        return zx::error(s);
      }
      continue;
    }

    // This is the first process in this Starnix job. We get the list of all VMOs in that job only
    // once as we assume this list is shared by all processes in that job.
    if (!starnix_proc->second.vmos_retrieved) {
      TRACE_DURATION_BEGIN("memory_metrics", "StarnixCaptureStrategy::Finalize::StarnixVMOs");
      std::vector<zx_info_vmo_t> vmos;
      auto result =
          GetInfoVector(os, process_handles_[process.koid].get(), ZX_INFO_PROCESS_VMOS, vmos);
      if (result.status_value() == ZX_ERR_BAD_STATE) {
        continue;
      } else if (result.is_error()) {
        return result.take_error();
      }
      starnix_proc->second.vmos_retrieved = true;

      // We fill |unmapped_vmos| with all the known VMOs of this process group. Mapped VMOs will be
      // removed from |unmapped_vmos| later, as we learn of the mappings.
      size_t num_vmos = result.value();
      for (size_t i = 0; i < num_vmos; i++) {
        auto [it, is_new] = starnix_proc->second.unmapped_vmos.insert(vmos[i].koid);
        // If we have already seen the VMO in this process, then we have seen it globally and we
        // don't need to insert it again.
        if (is_new) {
          koid_to_vmo_.try_emplace(vmos[i].koid, vmos[i]);
        }
      }
      TRACE_DURATION_END("memory_metrics", "StarnixCaptureStrategy::Finalize::StarnixVMOs");
    }

    TRACE_DURATION_BEGIN("memory_metrics", "StarnixCaptureStrategy::Finalize::StarnixMappings");
    auto result =
        GetInfoVector(os, process_handles_[process.koid].get(), ZX_INFO_PROCESS_MAPS, mappings_);
    if (result.status_value() == ZX_ERR_BAD_STATE) {
      continue;
    } else if (result.is_error()) {
      return result.take_error();
    }

    size_t num_mappings = result.value();
    for (size_t i = 0; i < num_mappings; i++) {
      const auto& mapping = mappings_[i];
      if (mapping.type == ZX_INFO_MAPS_TYPE_MAPPING) {
        if (koid_to_vmo_.find(mapping.u.mapping.vmo_koid) == koid_to_vmo_.end()) {
          // It is a new VMO that we haven't captured. This can happen if the list of VMOs change
          // while we do the data collection.
          continue;
        }
        if (mapping.base >= 0x400000100000) {
          // This is a Starnix kernel mapping.
          starnix_proc->second.kernel_mapped_vmos.insert(mapping.u.mapping.vmo_koid);
        } else {
          // This is a restricted space mapping.
          starnix_proc->second.process_mapped_vmos[process.koid].insert(mapping.u.mapping.vmo_koid);
        }
        starnix_proc->second.unmapped_vmos.erase(mapping.u.mapping.vmo_koid);
      }
    }
    TRACE_DURATION_END("memory_metrics", "StarnixCaptureStrategy::Finalize::StarnixMappings");
  }

  auto result = base.Finalize(os);
  // base.Finalize() cannot fail.
  auto& [base_koid_to_process, base_koid_to_vmo] = result.value();

  // Both |koid_to_process_| and |base_koid_to_process| will contain process entries for
  // non-Starnix processes. However, only the |base_koid_to_process| entries will be filled with
  // the non-Starnix process VMOs. This calls adds the entries for Starnix processes to
  // |base_koid_to_process|, ready to be filled by the loop below..
  base_koid_to_process.merge(koid_to_process_);
  base_koid_to_vmo.merge(koid_to_vmo_);

  for (auto& [job, starnix_job] : starnix_jobs_) {
    for (auto& [process_koid, vmos] : starnix_job.process_mapped_vmos) {
      std::copy(vmos.begin(), vmos.end(),
                std::back_inserter(base_koid_to_process[process_koid].vmos));
    }
    std::copy(starnix_job.kernel_mapped_vmos.begin(), starnix_job.kernel_mapped_vmos.end(),
              std::back_inserter(base_koid_to_process[starnix_job.kernel_koid].vmos));
    std::copy(starnix_job.unmapped_vmos.begin(), starnix_job.unmapped_vmos.end(),
              std::back_inserter(base_koid_to_process[starnix_job.kernel_koid].vmos));
  }

  return zx::ok(std::make_tuple(std::move(base_koid_to_process), std::move(base_koid_to_vmo)));
}

}  // namespace memory
